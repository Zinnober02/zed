mod atlas;
mod clipboard;
mod dispatcher;
mod display;
mod host;
mod inputmethod;
mod keyboard;
mod platform;
mod protocol_fields;
mod text_system;
mod vk;
mod window;

use std::{
    cell::RefCell,
    collections::VecDeque,
    ffi::c_void,
    rc::{Rc, Weak},
    sync::{
        Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use futures::future::BoxFuture;

pub use vk::LogFn;

pub use host::HostOps;

/// schema 生成的字段常量（gen-protocol.py 的产物）。读写点用它取 JSON 的键，
/// 字段名就不再有三处手写。
pub(crate) use protocol_fields::fields;

/// 用生成的字段常量拼一份 JSON 对象文本。
///
/// 收一组（常量, 值），顺序原样保留：跨边界的形状由 schema 描述，这里只负责把
/// 常量变成键。字段名因此不会在读写点再出现字面量。
pub(crate) fn json_value(pairs: Vec<(&str, serde_json::Value)>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (name, value) in pairs {
        map.insert(name.to_string(), value);
    }
    serde_json::Value::Object(map)
}

/// 同上，直接给出可以回送的 JSON 文本。
pub(crate) fn json_object(pairs: Vec<(&str, serde_json::Value)>) -> String {
    json_value(pairs).to_string()
}

pub use inputmethod::{
    commit_text as ime_commit_text, delete_backward as ime_delete_backward,
    delete_forward as ime_delete_forward, finish_preview as ime_finish_preview,
    preview_text as ime_preview_text,
};

/// Log a line through the host-provided hilog sink.
pub fn log_line(message: &str) {
    vk::log(message);
}

use crate::{App, Application, MouseButton, NavigationDirection, Platform};

use platform::OhosPlatform;

thread_local! {
    /// The live platform for this (UI) thread. XComponent callbacks, ArkTS
    /// frame ticks and input all arrive on the ArkUI UI thread.
    static CURRENT: RefCell<Weak<OhosPlatform>> = RefCell::new(Weak::new());
}

/// 应用可写入的目录，来自 ArkTS 的 filesDir。
///
/// panic hook 在发生崩溃的那个线程上写 panic 记录，而这个值由 ArkUI 线程设置，
/// 所以它必须是进程级共享的：线程局部变量在这一边写入、在另一边读出空值，
/// 记录文件永远不会出现。
static ROOT_DIR: OnceLock<String> = OnceLock::new();

/// Set the directory the application should browse (from ArkTS context.filesDir).
pub fn set_root_dir(path: &str) {
    match ROOT_DIR.set(path.to_string()) {
        Ok(()) => vk::log(&format!("[gpui_ohos] root directory {path}")),
        // 每个页面都会调一次，同一进程里重复到达同一个目录属于正常情况；
        // 目录不同则说明两个页面认为的沙箱位置不一致，必须留下记录。
        Err(existing) if existing.as_str() == path => {}
        Err(existing) => vk::log(&format!(
            "[gpui_ohos] root directory stays {existing}, ignoring {path}"
        )),
    }
}

/// The directory set by set_root_dir, if any.
pub fn root_dir() -> Option<String> {
    ROOT_DIR.get().cloned()
}

fn set_current(platform: &Rc<OhosPlatform>) {
    CURRENT.with(|current| *current.borrow_mut() = Rc::downgrade(platform));
}

fn with_current<R>(f: impl FnOnce(&Rc<OhosPlatform>) -> R) -> Option<R> {
    CURRENT.with(|current| current.borrow().upgrade().map(|platform| f(&platform)))
}

/// Work the host asks for is handed to the thread that owns the platform. When the
/// caller already is that thread it runs inline, which keeps the common path free
/// of a queue and a wakeup; from any other thread it is queued and the owner is
/// woken. The owner is whichever thread the platform was created on.
type PlatformJob = Box<dyn FnOnce(&Rc<OhosPlatform>) + Send>;

pub(crate) struct PlatformQueue {
    jobs: Mutex<VecDeque<PlatformJob>>,
    wake: Condvar,
}

static PLATFORM_QUEUE: OnceLock<PlatformQueue> = OnceLock::new();

/// A native window handle the host owns. The handle names a process-wide surface
/// and the platform thread is the only thing that renders with it, so carrying it
/// across the queue is sound. The pointer is never dereferenced here.
#[derive(Clone, Copy)]
pub(crate) struct NativeWindow(*mut c_void);

unsafe impl Send for NativeWindow {}

impl NativeWindow {
    pub(crate) fn as_ptr(self) -> *mut c_void {
        self.0
    }
}
pub(crate) fn platform_queue() -> &'static PlatformQueue {
    PLATFORM_QUEUE.get_or_init(|| PlatformQueue {
        jobs: Mutex::new(VecDeque::new()),
        wake: Condvar::new(),
    })
}

impl PlatformQueue {
    fn push(&self, job: PlatformJob) {
        self.jobs.lock().unwrap().push_back(job);
        self.wake.notify_all();
    }

    /// Wake the loop without handing it work: the caller set state it has to look
    /// at, such as a pending frame request.
    ///
    /// 通知与 take() 里"看完标志再睡"用同一把锁：只对着条件变量叫一声会撞上还没睡下
    /// 的等待者，那一拍的通知就丢了，于是这一帧要等到下一次 vsync 才画出来。
    pub(crate) fn wake(&self) {
        let _guard = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.wake.notify_all();
    }

    /// Take everything queued so far, waiting up to `timeout` for the first one.
    /// An empty result means the wait ran out, not that there is nothing to do:
    /// frames are driven by the host, so a quiet queue is normal.
    ///
    /// 已经记下的帧请求同样结束等待：vsync 回调大多落在平台线程正在画帧的时候，那次
    /// 通知没有人接，只剩这个标志，不先看它一眼就睡下会白丢一拍。
    pub(crate) fn take(&self, timeout: Duration) -> Vec<PlatformJob> {
        let mut jobs = self.jobs.lock().unwrap();
        while jobs.is_empty() && !TICK_WANTED.load(Ordering::SeqCst) {
            let (guard, _) = self.wake.wait_timeout(jobs, timeout).unwrap();
            jobs = guard;
        }
        jobs.drain(..).collect()
    }
}

/// The gate the platform's thread holds while it draws a frame.
///
/// The host destroys a native window as soon as its own call returns, so that
/// call must not return while a frame is still being presented into the window.
/// It takes this gate once, which can only mean the frame in flight has finished;
/// and the platform's thread, which draws only while holding it, then reads the
/// retired list before drawing, so no frame starts on a window the host has
/// released.
///
/// The wait is bounded by what the platform's thread holds the gate for: one frame
/// while the application runs, and the whole application start while
/// `start_platform` is inside it (starting draws too, and a window can be released
/// as soon as it exists). That span never waits for the ArkUI thread in return -
/// every host call the platform thread makes is a post - so the ArkUI thread's
/// wait has an end without either side waiting for the other.
static PRESENT_GATE: Mutex<()> = Mutex::new(());

/// Windows the host is about to release, waiting for the platform's thread to
/// apply them inside the frame gate.
static RETIRED_SURFACES: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn present_gate() -> std::sync::MutexGuard<'static, ()> {
    // 写入者只有本文件；锁中毒只可能来自持锁时的 panic，而它保护的只是一个空值，
    // 取回内层值继续画帧比让界面停在最后一帧更接近事实。
    PRESENT_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Note a window the host is releasing, for the platform's thread to apply.
fn retire_surface(id: &str) {
    let mut retired = RETIRED_SURFACES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if retired.iter().any(|existing| existing == id) {
        return;
    }
    retired.push(id.to_string());
}

/// Take the windows the host has released. Called from the platform's thread
/// inside the frame gate.
fn take_retired_surfaces() -> Vec<String> {
    let mut retired = RETIRED_SURFACES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::mem::take(&mut *retired)
}

/// Hand a job to the platform's thread without waiting for it.
pub(crate) fn post(job: impl FnOnce(&Rc<OhosPlatform>) + Send + 'static) {
    if let Some(platform) = with_current(|platform| platform.clone()) {
        job(&platform);
        return;
    }
    platform_queue().push(Box::new(job));
}
fn new_platform() -> Rc<OhosPlatform> {
    let platform = Rc::new(
        OhosPlatform::new()
            .unwrap_or_else(|error| panic!("Failed to initialize OHOS platform: {error}")),
    );
    set_current(&platform);
    platform
}

/// Whether the platform thread has been started. The boot entry is called from a
/// JavaScript thread, which owns nothing, so this - not a thread local - decides
/// whether a surface belongs to a running platform.
static PLATFORM_STARTED: AtomicBool = AtomicBool::new(false);

/// Whether a frame has been asked for and not drawn yet.
static TICK_WANTED: AtomicBool = AtomicBool::new(false);

/// Whether the application asked to quit and the host has not been told yet.
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Panics recovered from in the platform loop since the last reported one, and the
/// time of that report.
///
/// A frame that panics on every request would otherwise write a line per frame,
/// which buries the report that says where the trouble started.
static RECOVERED_PANICS: Mutex<(u64, Option<Instant>)> = Mutex::new((0, None));

/// Run one step of the platform loop so that a panic inside it cannot end the
/// thread.
///
/// The thread answers every request the host makes of the platform, so letting it
/// end takes input, window closes and frame requests with it, and the host waits
/// for answers nobody produces. After a panic the thread only promises to keep
/// serving the queue: the platform's state may have been left partway through an
/// update, so the next frame can draw something that no longer matches it.
///
/// The step is not `UnwindSafe` - it holds `Rc` handles and `RefCell`s whose
/// borrow flags a panic may have left marked - and `AssertUnwindSafe` accepts
/// that, on the grounds that continuing with possibly stale state beats a window
/// that can no longer be used or closed. The panic hook installed in
/// `install_panic_hook` has already written the message and backtrace; this adds
/// the step it came from.
fn run_platform_step(stage: &str, step: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(step)).is_err() {
        report_recovered_panic(stage);
    }
}

fn report_recovered_panic(stage: &str) {
    const REPORT_INTERVAL: Duration = Duration::from_secs(5);
    let now = Instant::now();
    let report = {
        let mut recovered = RECOVERED_PANICS.lock().unwrap();
        recovered.0 += 1;
        let due = match recovered.1 {
            Some(last) => now.duration_since(last) >= REPORT_INTERVAL,
            None => true,
        };
        if due {
            let suppressed = recovered.0 - 1;
            recovered.0 = 0;
            recovered.1 = Some(now);
            Some(suppressed)
        } else {
            None
        }
    };
    let Some(suppressed) = report else {
        return;
    };
    // Written after the lock is released: a panic inside the logger would poison
    // the mutex, and every later report would end the thread this keeps alive.
    let message = if suppressed == 0 {
        format!("[gpui_ohos] recovered from a panic in {stage}")
    } else {
        format!(
            "[gpui_ohos] recovered from a panic in {stage}; {suppressed} more were left unreported"
        )
    };
    vk::log(&message);
}

/// Start the platform's own thread. Everything that touches gpui runs there, and
/// the host only ever hands work over: a JavaScript thread dying no longer takes
/// the frame loop, the input method session or a window's surface with it.
fn start_platform<F>(id: String, window: NativeWindow, width: u32, height: u32, boot: F) -> i32
where
    F: FnOnce(&Rc<OhosPlatform>) + Send + 'static,
{
    let spawned = std::thread::Builder::new()
        .name("gpui-ohos".to_string())
        // Zed runs deep recursion while parsing; the default stack is not enough.
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let platform = new_platform();
            platform.set_surface(&id, window.as_ptr(), width, height);
            {
                // Starting the application draws into the surface too, and the host
                // may release that window as soon as it exists; the frame gate is
                // what tells the host when it is safe to.
                let gate = present_gate();
                boot(&platform);
                platform.launch();
                drop(gate);
            }
            inputmethod::attach();
            platform.request_frames();
            vk::log(&format!(
                "[gpui_ohos] platform thread {:?} running, windows={}, root dir {:?}",
                std::thread::current().id(),
                platform.window_count(),
                root_dir()
            ));
            loop {
                for job in platform_queue().take(Duration::from_millis(500)) {
                    run_platform_step("a queued job", || job(&platform));
                }
                // One frame per request however many arrived, and never from
                // inside a job: a frame request made while drawing would otherwise
                // re-enter the frame loop.
                if TICK_WANTED.swap(false, Ordering::SeqCst) {
                    run_platform_step("a frame", || tick_platform(&platform));
                }
                // The queue that carried the quit has drained, so everything the
                // shutdown queued behind it has run. Now the host can end it.
                if QUIT_REQUESTED.swap(false, Ordering::SeqCst) {
                    vk::log("[gpui_ohos] telling the host to quit");
                    run_platform_step("the quit announcement", || {
                        host::window_op(host::op::QUIT, "")
                    });
                }
            }
        });
    match spawned {
        Ok(_) => 0,
        Err(error) => {
            vk::log(&format!(
                "[gpui_ohos] could not start the platform thread: {error}"
            ));
            1
        }
    }
}
/// Entry point used by gpui_platform and gpui_ohos_demo.
///
/// Reuses the platform the host already attached a surface to, so an
/// application that builds its own `Application` (Zed) ends up on the same
/// platform as the entry point that received the surface.
pub fn current_platform(_headless: bool) -> Rc<dyn Platform> {
    with_current(|platform| platform.clone() as Rc<dyn Platform>)
        .unwrap_or_else(|| new_platform() as Rc<dyn Platform>)
}

/// Attach a surface, let `run_app` build and register the application on the
/// shared platform, then launch it and start the frame loop.
/// Route panic messages somewhere we can read them: stderr is not captured in a
/// HAP, and the application installs its own handler while starting, so this runs
/// again later (see `reinstall_panic_hook`) to win that race.
pub fn install_panic_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = format!(
            "[gpui_ohos] PANIC: {info}\n{:?}",
            std::backtrace::Backtrace::force_capture()
        );
        vk::log(&message);
        // The next launch mirrors this file into hilog, which survives a crash
        // that kills the process mid log flush.
        //
        // 写不进去必须留下记录：记录文件不存在与写入失败看起来完全一样，
        // 只凭文件为空无法断定有没有崩溃。
        match root_dir() {
            Some(root) => {
                let path = format!("{root}/panics.log");
                match std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    Ok(mut file) => {
                        use std::io::Write;
                        if let Err(error) = writeln!(file, "{message}") {
                            vk::log(&format!("[gpui_ohos] could not write {path}: {error}"));
                        }
                    }
                    Err(error) => vk::log(&format!("[gpui_ohos] could not open {path}: {error}")),
                }
            }
            None => vk::log("[gpui_ohos] no root directory, the panic is not in panics.log"),
        }
        previous_hook(info);
    }));
}

pub fn run_app_on_surface<F>(
    id: &str,
    window: *mut c_void,
    width: u32,
    height: u32,
    log: LogFn,
    run_app: F,
) -> i32
where
    F: FnOnce() + Send + 'static,
{
    vk::set_logger(log);
    vk::log(&format!(
        "[gpui_ohos] run_app_on_surface id={id} window={:p} {}x{}",
        window, width, height
    ));
    install_panic_hook();

    let id = id.to_string();
    let window = NativeWindow(window);
    // The first surface starts the application, on the platform's own thread. Later
    // surfaces are the XComponents the host created for windows the application
    // asked for; running the application again for one of those would open yet
    // another window, so only the surface is attached.
    if PLATFORM_STARTED.swap(true, Ordering::SeqCst) {
        post(move |platform| {
            platform.add_surface(&id, window.as_ptr(), width, height);
            platform.request_frames();
        });
        return 0;
    }
    start_platform(id, window, width, height, move |_platform| run_app())
}

/// Start a GPUI application on an XComponent surface and render the first frame.
///
/// Called by the C++ host from OnSurfaceCreated. GPUI's Platform::run only
/// records the launch callback, so we invoke it here once the surface exists;
/// the ArkTS frame loop then drives subsequent frames through tick().
pub fn run_with_surface<F>(
    id: &str,
    window: *mut c_void,
    width: u32,
    height: u32,
    log: LogFn,
    app: F,
) -> i32
where
    F: FnOnce(&mut App) + Send + 'static,
{
    vk::set_logger(log);
    vk::log(&format!(
        "[gpui_ohos] run_with_surface id={id} window={:p} {}x{}",
        window, width, height
    ));
    let id = id.to_string();
    let window = NativeWindow(window);
    if PLATFORM_STARTED.swap(true, Ordering::SeqCst) {
        post(move |platform| {
            platform.add_surface(&id, window.as_ptr(), width, height);
            platform.request_frames();
        });
        return 0;
    }
    start_platform(id, window, width, height, move |platform| {
        let platform: &Rc<OhosPlatform> = platform;
        {
            let text_system = platform.text_system();
            let names = text_system.all_font_names();
            vk::log(&format!("[gpui_ohos] text_system fonts={}", names.len()));
            if let Some(sample) = names.iter().find(|name| name.contains("HarmonyOS")) {
                vk::log(&format!("[gpui_ohos] text_system sample={sample}"));
            }
        }
        {
            // Clipboard self-test: write needs no permission, read needs the
            // READ_PASTEBOARD ACL permission.
            let marker = "gpui_ohos clipboard test";
            let wrote = clipboard::write_text(marker);
            let read = clipboard::read_text();
            vk::log(&format!(
                "[gpui_ohos] clipboard write={wrote} read={read:?}"
            ));
        }
        Application::with_platform(platform.clone() as Rc<dyn Platform>).run(app);
    })
}

/// List a picked directory. The answer is awaited, never waited for on the host's
/// side: the question carries a number and the host answers it from its own thread.
pub fn list_picked_dir_async(
    uri: String,
) -> BoxFuture<'static, std::io::Result<Vec<(String, bool, String)>>> {
    Box::pin(host::list_dir(uri))
}

/// Open a picked file, with the answer awaited.
pub fn open_picked_file_async(uri: String) -> BoxFuture<'static, Option<i32>> {
    Box::pin(host::open_file(uri))
}

/// Release a descriptor opened for a single picked-file operation.
pub fn close_picked_file_async(fd: i32) -> BoxFuture<'static, std::io::Result<()>> {
    Box::pin(host::close_fd(fd))
}

/// Read a picked file's bytes through its descriptor.
///
/// The descriptor is the process's, so no thread hop is involved: the read happens
/// wherever the caller is waiting for it.
pub fn read_picked_file_bytes_async(fd: i32) -> BoxFuture<'static, std::io::Result<Vec<u8>>> {
    Box::pin(async move { host::read_fd_bytes(fd) })
}

/// Overwrite a picked file through its descriptor.
pub fn write_picked_file_bytes_async(
    fd: i32,
    data: Vec<u8>,
) -> BoxFuture<'static, std::io::Result<()>> {
    Box::pin(async move { host::write_fd_bytes(fd, &data) })
}

/// Create a directory under a picked root.
pub fn create_picked_dir_async(uri: String) -> BoxFuture<'static, std::io::Result<()>> {
    Box::pin(host::create_dir(uri))
}

/// Remove a file or directory under a picked root.
pub fn remove_picked_path_async(
    uri: String,
    is_dir: bool,
) -> BoxFuture<'static, std::io::Result<()>> {
    Box::pin(host::remove(uri, is_dir))
}

/// Rename an entry under a picked root.
pub fn rename_picked_path_async(
    from: String,
    to: String,
) -> BoxFuture<'static, std::io::Result<()>> {
    Box::pin(host::rename(from, to))
}

/// Logical pixels per device pixel used by the backend renderer.
pub fn scale() -> f32 {
    window::SCALE
}

/// Take the folder URI the host restored from persisted authorization.
pub fn take_restore_folder() -> Option<String> {
    with_current(|platform| platform.take_restore_folder()).flatten()
}

/// Install the host function pointers supplied by the ArkTS layer.
pub fn set_host_ops(ops: HostOps) {
    host::set_ops(ops);
}

/// Handle an event pushed from the ArkTS host.
pub fn host_event(kind: i32, arg: &str) {
    // 查询的答案按请求号配对，谁在等谁就知道，所以它在这里就交给等待者，
    // 不必再走平台线程的队列：那个队列可能正被一帧占着，而答案的等待者可能就在等这一帧。
    if kind == host::event::QUERY_ANSWER {
        host::complete_query(arg);
        return;
    }
    // 焦点离开时最后按下的修饰键不会再等来抬起事件，累积的状态只能在这里复位；
    // 应用侧保留的修饰键状态同样要收一次复位后的事件，否则它也会停在按下。
    // 解析失败不在这里报：焦点事件会交给平台线程的 handle_host_event，它记那一行。
    if kind == host::event::FOCUS {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(arg) {
            if value[fields::FOCUSED].as_bool() == Some(false) {
                forget_modifiers();
                post(|platform| platform.dispatch_modifiers("", crate::Modifiers::default()));
            }
        }
    }
    let arg = arg.to_string();
    post(move |platform| platform.handle_host_event(kind, &arg));
}

/// Whether the application lets this window close. Asked by the host before it
/// closes a window on its own.
///
/// The answer is not returned here: only the platform's thread may give it, and
/// this is the host's own close handler asking, on the thread that has to keep
/// running the windows. Waiting for the platform there was half of a wait that
/// could only end in a timeout. The question is queued, the answer goes back
/// through `host::answer_close`, and the return value only says whether that
/// answer is still to come.
pub fn should_close_window(id: &str) -> bool {
    let id = id.to_string();
    if let Some(platform) = with_current(|platform| platform.clone()) {
        // Already on the platform's thread: nothing to wait for, so the answer goes
        // back with this call.
        let allowed = platform.should_close_window(&id);
        host::answer_close(&id, allowed);
        return true;
    }
    if !PLATFORM_STARTED.load(Ordering::SeqCst) {
        // No platform means no application state to save: the window may go.
        vk::log(&format!(
            "[gpui_ohos] close question for {id}: no platform, the window may close"
        ));
        host::answer_close(&id, true);
        return true;
    }
    vk::log(&format!("[gpui_ohos] close question for {id} queued"));
    post(move |platform| {
        let allowed = platform.should_close_window(&id);
        vk::log(&format!(
            "[gpui_ohos] close answer for {id}: {allowed} (deferred by the host)"
        ));
        host::answer_close(&id, allowed);
    });
    false
}

/// An additional XComponent surface was created for another window.
pub fn surface_created(id: &str, window: *mut c_void, width: u32, height: u32) {
    let id = id.to_string();
    let window = NativeWindow(window);
    post(move |platform| {
        vk::log(&format!(
            "[gpui_ohos] surface_created id={id} window={:p} {}x{}",
            window.as_ptr(),
            width,
            height
        ));
        platform.add_surface(&id, window.as_ptr(), width, height);
        platform.request_frames();
    });
}

/// XComponent surface changed size (rotation / resize).
pub fn surface_resized(id: &str, width: u32, height: u32) {
    let id = id.to_string();
    post(move |platform| {
        vk::log(&format!(
            "[gpui_ohos] surface_resized id={id} {}x{}",
            width, height
        ));
        platform.surface_resized(&id, width, height);
    });
}

/// XComponent surface destroyed.
/// The host is about to destroy a native window.
///
/// The platform's thread is the only one rendering into that window, so the frame
/// it may be drawing has to finish before the host tears the window down, which it
/// does as soon as this returns. The window is noted for retirement first, and
/// then the frame gate is taken once: getting it means no frame is in flight, and
/// the platform's thread, which draws only while holding it, reads the retirement
/// list before drawing, so it never starts a frame on a window the host has
/// released. The wait is the frame in flight, or the rest of the application start
/// while `start_platform` holds the gate; it is never a queue round-trip, and the
/// platform's thread never waits for this one inside that span.
pub fn surface_destroyed(id: &str) {
    let id_text = id.to_string();
    // 平台线程自己报的销毁：它不在帧里（帧循环里没有销毁窗口的入口），直接应用。
    if with_current(|platform| {
        vk::log(&format!(
            "[gpui_ohos] surface_destroyed on the platform thread: {id_text}"
        ));
        platform.surface_destroyed(&id_text);
    })
    .is_some()
    {
        return;
    }
    retire_surface(&id_text);
    {
        let gate = present_gate();
        drop(gate);
    }
    vk::log(&format!(
        "[gpui_ohos] surface retired {id_text}: no frame is in flight, the host may release it"
    ));
}

/// One frame's work: the input method's queued commands and cursor context, then
/// the frame itself.
fn tick_platform(platform: &Rc<OhosPlatform>) {
    // 帧必须在这把锁里画完：宿主正拿着它去释放已经退役的窗口。拿到锁的第一件事是应用
    // 退役表，这样接下来这一帧不会画到一个宿主已经释放的窗口上。
    let gate = present_gate();
    for id in take_retired_surfaces() {
        platform.surface_destroyed(&id);
    }
    for command in inputmethod::drain() {
        platform.handle_ime(command);
    }
    if let Some((text, caret, cursor)) = platform.ime_context() {
        inputmethod::update_context(&text, caret, cursor);
    }
    // The input method binds to a window only while that window holds focus, so a
    // request made while another window was focused is replayed here once it does.
    inputmethod::apply(platform.focused_surface().as_deref());
    platform.tick();
    drop(gate);
}

/// The application wants the process to end. Recorded rather than sent: the host
/// may only be told once the work the announcement came from has drained.
pub(crate) fn request_quit() {
    QUIT_REQUESTED.store(true, Ordering::SeqCst);
    platform_queue().wake();
}

/// A frame was asked for. Requests coalesce: the first one wakes the loop and the
/// loop draws once for all of them, which is the backpressure the frame loop had.
pub(crate) fn want_tick() {
    TICK_WANTED.store(true, Ordering::SeqCst);
    platform_queue().wake();
}

/// Called by the host once per refresh.
pub fn tick() {
    want_tick();
}

/// The NDK's own button numbers: left 1, right 2, middle 3, back 4, forward 5.
fn map_button(button: u32) -> MouseButton {
    match button {
        2 => MouseButton::Right,
        3 => MouseButton::Middle,
        4 => MouseButton::Navigate(NavigationDirection::Back),
        5 => MouseButton::Navigate(NavigationDirection::Forward),
        _ => MouseButton::Left,
    }
}

// The host converts ArkUI's pointer vp coordinates into GPUI logical pixels
// (it knows the display density and the node offset), so this is a pass-through.
fn logical(value: f32) -> crate::Pixels {
    crate::px(value)
}

pub fn pointer_down(id: &str, x: f32, y: f32, button: u32) {
    let id = id.to_string();
    post(move |platform| {
        // Clicking the surface must give the XComponent ArkUI focus, otherwise its
        // onKeyEvent never fires and non-text keys (arrows) are dropped. The request
        // has to name the surface, or focus lands on the main window instead. A
        // surface that already holds focus needs no request, and skipping it keeps
        // the common case free of a synchronous call into JS.
        let needs_focus = platform.focused_surface().as_deref() != Some(id.as_str());
        if needs_focus {
            // The request has to name the node it means: a page asks its own
            // window for focus by the component id, and the pages that are not the
            // main one take that name from the payload rather than from the id.
            host::window_op_for(&id, host::op::REQUEST_FOCUS, &id);
        }
        let targets = platform.route_targets(&id);
        // The one line that tells input that never arrived from input that arrived
        // and went nowhere.
        vk::log(&format!(
            "[gpui_ohos] pointer_down id={id} targets={}",
            targets.len()
        ));
        for window in targets {
            let position = crate::point(logical(x), logical(y));
            // Hit testing follows the last mouse position, which a synthetic
            // click never updates, so seed it before pressing.
            window.pointer_move(position);
            window.pointer_down(map_button(button), position);
        }
    });
}

pub fn pointer_up(id: &str, x: f32, y: f32, button: u32) {
    let id = id.to_string();
    post(move |platform| {
        let button = map_button(button);
        for window in platform.route_targets(&id) {
            // A release with no press behind it never belonged to this window. The
            // host hands the node events of windows the pointer is not over as
            // well, and an unpaired release moved the editor out of a drag it had
            // never started.
            if window.pressed_button().is_none() {
                vk::log(&format!("[gpui_ohos] pointer up id={id} without-press"));
                continue;
            }
            window.pointer_up(button, crate::point(logical(x), logical(y)));
        }
    });
}

pub fn pointer_move(id: &str, x: f32, y: f32) {
    let id = id.to_string();
    post(move |platform| {
        // Which window a pointer event belongs to is decided in the host, where the
        // hover reports arrive: it knows both the window the pointer is over and
        // whether a button is still held. A second copy of that rule here could
        // only disagree with the first.
        for window in platform.route_targets(&id) {
            window.pointer_move(crate::point(logical(x), logical(y)));
        }
    });
}

/// ArkUI axis events (mouse wheel / touchpad) are already in logical pixels.
pub fn scroll(id: &str, x: f32, y: f32, delta_x: f32, delta_y: f32, phase: i32) {
    let phase = match phase {
        0 => crate::TouchPhase::Started,
        2 => crate::TouchPhase::Ended,
        _ => crate::TouchPhase::Moved,
    };
    let id = id.to_string();
    post(move |platform| {
        for window in platform.route_targets(&id) {
            window.scroll(
                crate::point(crate::px(x), crate::px(y)),
                crate::point(crate::px(delta_x), crate::px(delta_y)),
                phase,
            );
        }
    });
}

/// 键盘修饰键的状态，由 NDK 的按键事件累积出来。
///
/// 按键事件到达的是 ArkUI 线程，窗口事件在平台线程上分派，所以这里必须是进程级
/// 共享的状态：任意一侧改用线程局部变量，另一侧读到的就是自己的默认值，
/// Ctrl/Shift/Alt + 点击会丢掉修饰键。
static MODIFIERS: AtomicU8 = AtomicU8::new(0);

const MODIFIER_CONTROL: u8 = 1 << 0;
const MODIFIER_ALT: u8 = 1 << 1;
const MODIFIER_SHIFT: u8 = 1 << 2;
const MODIFIER_PLATFORM: u8 = 1 << 3;

/// 当前的修饰键状态。
pub(crate) fn modifiers() -> crate::Modifiers {
    let bits = MODIFIERS.load(Ordering::SeqCst);
    crate::Modifiers {
        control: bits & MODIFIER_CONTROL != 0,
        alt: bits & MODIFIER_ALT != 0,
        shift: bits & MODIFIER_SHIFT != 0,
        platform: bits & MODIFIER_PLATFORM != 0,
        // 这个平台没有把功能键当成修饰键上报。
        function: false,
    }
}

/// 丢掉累积的修饰键状态。
///
/// 焦点离开之后，按下的修饰键不会再有抬起事件送达，状态卡在按下会让此后的输入
/// 全部被当成快捷键，所以焦点丢失时要复位。
fn forget_modifiers() {
    if MODIFIERS.swap(0, Ordering::SeqCst) != 0 {
        vk::log("[gpui_ohos] modifiers reset after focus loss");
    }
}

thread_local! {
    /// Code of the key whose down event reached the application, or 0 when none did. The input
    /// method takes the down of the keys it handles itself and lets the up through, so an up
    /// without a down is that case rather than a stray event.
    static PRESSED_KEY: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
    /// When that down arrived. A release never follows a press that the input method took,
    /// and one is dropped as well when focus leaves the window mid-press, so a code left
    /// behind would make the next release of the same key look paired up and be dropped.
    /// The age is what tells the two apart.
    static PRESSED_AT: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

/// How long a press may stay unanswered before its release counts as a press of its own.
///
/// The pair only ever distinguishes "this release had its own press" from "the input method
/// took the press", so the number wants to be longer than any deliberate single press and
/// shorter than the time between two separate ones.
const PRESS_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(3);

fn is_modifier_key(code: i32) -> bool {
    matches!(code, 2072 | 2073 | 2047 | 2048 | 2045 | 2046 | 2076 | 2077)
}

/// Keys the application acts on when they go down, and which the input method also uses while
/// it composes: it takes their down (enter accepts a candidate, backspace edits the pinyin,
/// the arrows pick one) and the application is left with the up, which on its own types
/// nothing.
fn is_press_key(code: i32) -> bool {
    matches!(code, 2012..=2015 | 2049 | 2054 | 2055 | 2068..=2071 | 2081..=2083)
}

fn update_modifiers(code: i32, down: bool) -> crate::Modifiers {
    let bit = match code {
        2072 | 2073 => MODIFIER_CONTROL,
        2047 | 2048 => MODIFIER_SHIFT,
        2045 | 2046 => MODIFIER_ALT,
        2076 | 2077 => MODIFIER_PLATFORM,
        _ => return modifiers(),
    };
    if down {
        MODIFIERS.fetch_or(bit, Ordering::SeqCst);
    } else {
        MODIFIERS.fetch_and(!bit, Ordering::SeqCst);
    }
    modifiers()
}

/// Keys GPUI must see before the input method: modifiers, navigation and any
/// shortcut combination. Everything else is text and goes to the input method.
fn is_gpui_key(code: i32, modifiers: crate::Modifiers) -> bool {
    // Only shortcuts are taken before the input method. Navigation and editing
    // keys are left to it as well, because it needs them while it composes:
    // backspace edits the pinyin, the arrows pick a candidate, escape cancels
    // and enter accepts. Whatever it does not consume comes back through the
    // post input method callback, so the editor still sees every key.
    is_modifier_key(code)
        || modifiers.control
        || modifiers.alt
        || modifiers.platform
        || matches!(code, 2090..=2101) // function keys
}

/// Pre-IME key event. Returns true to consume it so the input method never
/// sees it.
pub fn key_pre_ime(id: &str, action: i32, code: i32, _unicode: i32) -> bool {
    let down = action == 0;
    let modifiers = update_modifiers(code, down);
    if is_modifier_key(code) {
        dispatch_modifiers(id, modifiers);
        // The input method never sees a consumed event, and it needs the
        // modifier state to recognise its own switch shortcuts, so modifiers are
        // forwarded to it as well as to the application.
        return false;
    }
    // Shift or Super with space switches input methods here; Ctrl with space is
    // the application's completion shortcut, so it stays consumed.
    let is_switch_combo = code == 2050
        && (modifiers.shift || modifiers.platform)
        && !modifiers.control
        && !modifiers.alt;
    if is_switch_combo {
        dispatch_modifiers(id, modifiers);
        return false;
    }
    if !is_gpui_key(code, modifiers) {
        return false;
    }
    dispatch_key(id, down, code, modifiers);
    true
}

/// Post-IME key event (action: 0 = down, 1 = up). The NDK reports the Unicode
/// value for printable keys.
pub fn key_event(id: &str, action: i32, code: i32, unicode: i32) {
    let down = action == 0;
    let modifiers = update_modifiers(code, down);
    if is_modifier_key(code) {
        dispatch_modifiers(id, modifiers);
        return;
    }
    // A key that types a character is text, and text enters the application the same way the
    // input method's own commits do. The input method hands over the keys it composes with and
    // leaves the rest - digits, punctuation - to this callback, and a key event by itself types
    // nothing: the editor takes text through its input handler alone.
    if let Some(text) = typed_text(code, unicode, modifiers) {
        if down {
            dispatch_text(id, &text);
        }
        return;
    }
    if down {
        PRESSED_KEY.with(|cell| cell.set(code));
        PRESSED_AT.with(|cell| cell.set(Some(std::time::Instant::now())));
    } else {
        let previous = PRESSED_KEY.with(|cell| cell.replace(0));
        let pressed_at = PRESSED_AT.with(|cell| cell.take());
        let had_down =
            previous == code && pressed_at.is_some_and(|at| at.elapsed() < PRESS_STALE_AFTER);
        if !had_down && is_press_key(code) {
            // Nothing was composing by the time an unconsumed up arrived, so the press is
            // rebuilt here: without the down the application never sees this key at all, and
            // the terminal and the editor both act on the down.
            vk::log(&format!("[gpui_ohos] press rebuilt for key up code={code}"));
            dispatch_key(id, true, code, modifiers);
        }
    }
    dispatch_key(id, down, code, modifiers);
}

/// The character a key types, for the keys that type anything at all.
///
/// Shift shapes the character; the modifiers that turn a keystroke into a command do not.
fn typed_text(code: i32, unicode: i32, modifiers: crate::Modifiers) -> Option<String> {
    if modifiers.control || modifiers.alt || modifiers.platform {
        return None;
    }
    let character = u32::try_from(unicode)
        .ok()
        .filter(|value| *value > 0)
        .and_then(char::from_u32)
        .or_else(|| ohos_key(code).and_then(|(_, character)| character))?;
    // Enter, tab and escape carry a Unicode value on this platform and stay on the key path,
    // where the bindings that act on them live.
    if character.is_control() {
        return None;
    }
    Some(if modifiers.shift {
        character.to_uppercase().to_string()
    } else {
        character.to_string()
    })
}

/// Hand text to the focused input handler of the surface's window.
fn dispatch_text(id: &str, text: &str) {
    let id = id.to_string();
    let text = text.to_string();
    post(move |platform| platform.dispatch_text(&id, &text));
}

/// Forward a modifier-state change to the surface's window.
///
/// The host calls this from the ArkUI thread while the application lives on the
/// platform thread, so - like every other entry point - it is queued for that
/// thread instead of being applied here. Doing it here reached a thread local
/// that only the platform thread ever sets, so the change went nowhere.
fn dispatch_modifiers(id: &str, modifiers: crate::Modifiers) {
    let id = id.to_string();
    post(move |platform| platform.dispatch_modifiers(&id, modifiers));
}

fn dispatch_key(id: &str, down: bool, code: i32, modifiers: crate::Modifiers) {
    let Some((key, character)) = ohos_key(code) else {
        vk::log(&format!("[gpui_ohos] unmapped key code={code}"));
        return;
    };
    let key_char = character
        .filter(|_| !modifiers.control && !modifiers.platform)
        .map(|character| character.to_string());
    let keystroke = crate::Keystroke {
        modifiers,
        key,
        key_char,
    };
    vk::log(&format!(
        "[gpui_ohos] key {} {}",
        if down { "down" } else { "up" },
        keystroke
    ));
    let id = id.to_string();
    post(move |platform| platform.dispatch_key(&id, down, keystroke));
}

/// Map an OHOS key code to a GPUI key name and the character it types.
fn ohos_key(code: i32) -> Option<(String, Option<char>)> {
    Some(match code {
        2000..=2009 => {
            let ch = (b'0' + (code - 2000) as u8) as char;
            (ch.to_string(), Some(ch))
        }
        2017..=2042 => {
            let ch = (b'a' + (code - 2017) as u8) as char;
            (ch.to_string(), Some(ch))
        }
        2012 => ("up".to_string(), None),
        2013 => ("down".to_string(), None),
        2014 => ("left".to_string(), None),
        2015 => ("right".to_string(), None),
        2043 => (",".to_string(), Some(',')),
        2044 => (".".to_string(), Some('.')),
        2049 => ("tab".to_string(), None),
        2050 => ("space".to_string(), Some(' ')),
        2054 => ("enter".to_string(), None),
        2055 => ("backspace".to_string(), None),
        2056 => ("\u{60}".to_string(), Some('\u{60}')),
        2057 => ("-".to_string(), Some('-')),
        2058 => ("=".to_string(), Some('=')),
        2059 => ("[".to_string(), Some('[')),
        2060 => ("]".to_string(), Some(']')),
        2061 => ("\\".to_string(), Some('\\')),
        2062 => (";".to_string(), Some(';')),
        2063 => ("'".to_string(), Some('\'')),
        2064 => ("/".to_string(), Some('/')),
        2065 => ("@".to_string(), Some('@')),
        2066 => ("+".to_string(), Some('+')),
        2068 => ("pageup".to_string(), None),
        2069 => ("pagedown".to_string(), None),
        2070 => ("escape".to_string(), None),
        2071 => ("delete".to_string(), None),
        2081 => ("home".to_string(), None),
        2082 => ("end".to_string(), None),
        2083 => ("insert".to_string(), None),
        2090..=2101 => (format!("f{}", code - 2089), None),
        _ => return None,
    })
}
