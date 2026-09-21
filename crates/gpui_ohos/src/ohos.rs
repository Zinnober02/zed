mod atlas;
mod clipboard;
mod dispatcher;
mod display;
mod host;
mod inputmethod;
mod keyboard;
mod platform;
mod text_system;
mod vk;
mod window;

use std::{
    cell::RefCell,
    collections::VecDeque,
    ffi::c_void,
    future::Future,
    rc::{Rc, Weak},
    sync::{
        Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use futures::{FutureExt as _, channel::oneshot, future::BoxFuture};

pub use vk::LogFn;

pub use host::HostOps;

pub use inputmethod::{
    commit_text as ime_commit_text, delete_backward as ime_delete_backward,
    delete_forward as ime_delete_forward, finish_preview as ime_finish_preview, hide as ime_hide,
    preview_text as ime_preview_text, show as ime_show,
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
    /// Writable directory the app should browse, set from ArkTS filesDir.
    static ROOT_DIR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Set the directory the application should browse (from ArkTS context.filesDir).
pub fn set_root_dir(path: &str) {
    ROOT_DIR.with(|root| *root.borrow_mut() = Some(path.to_string()));
}

/// The directory set by set_root_dir, if any.
pub fn root_dir() -> Option<String> {
    ROOT_DIR.with(|root| root.borrow().clone())
}

/// Closures waiting for the ArkUI UI thread, drained from platform::tick.
///
/// The host bridge ends up in the JavaScript VM, which may only be entered from
/// the thread it was created on, so filesystem work running on a background
/// executor has to be marshalled back here.
static UI_TASKS: Mutex<std::collections::VecDeque<Box<dyn FnOnce() + Send>>> =
    Mutex::new(std::collections::VecDeque::new());

/// Run every closure queued by run_on_ui. Called from the UI thread.
/// Run the tasks the filesystem bridge queued, for at most `budget`.
///
/// These tasks call into the host and some of them read whole files, so one of
/// them can easily outlast a frame; whatever the budget does not cover stays
/// queued and runs on the next frame instead of holding the display back for
/// several periods.
pub(crate) fn drain_ui_tasks(budget: std::time::Duration) {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if std::time::Instant::now() >= deadline {
            return;
        }
        let task = {
            let mut tasks = UI_TASKS.lock().unwrap();
            tasks.pop_front()
        };
        match task {
            Some(task) => task(),
            None => return,
        }
    }
}

/// Run a closure on the ArkUI UI thread and await its result.
pub fn run_on_ui<T, F>(f: F) -> impl Future<Output = T> + Send
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (sender, receiver) = oneshot::channel();
    UI_TASKS.lock().unwrap().push_back(Box::new(move || {
        let _ = sender.send(f());
    }));
    receiver.map(|result| result.expect("the UI thread dropped a host task"))
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
    pub(crate) fn wake(&self) {
        self.wake.notify_all();
    }

    /// Take everything queued so far, waiting up to `timeout` for the first one.
    /// An empty result means the wait ran out, not that there is nothing to do:
    /// frames are driven by the host, so a quiet queue is normal.
    pub(crate) fn take(&self, timeout: Duration) -> Vec<PlatformJob> {
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.is_empty() {
            let (guard, _) = self.wake.wait_timeout(jobs, timeout).unwrap();
            jobs = guard;
        }
        jobs.drain(..).collect()
    }
}

/// How long a caller waits for the platform's thread to answer.
const PLATFORM_ANSWER_TIMEOUT: Duration = Duration::from_secs(2);

/// Run a job on the platform's thread and wait for its answer. `None` means there
/// is no platform to run it on, or that the platform did not answer within
/// `PLATFORM_ANSWER_TIMEOUT`.
///
/// The host calls this from the ArkUI thread, which has to keep running the
/// application's windows. Waiting without an end there meant a platform thread
/// that stopped serving its queue froze the window with it, and the user's close
/// request never came back.
pub(crate) fn on_platform<R: Send + 'static>(
    job: impl FnOnce(&Rc<OhosPlatform>) -> R + Send + 'static,
) -> Option<R> {
    if let Some(platform) = with_current(|platform| platform.clone()) {
        return Some(job(&platform));
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    platform_queue().push(Box::new(move |platform| {
        // The caller may have given up waiting; the answer has nowhere to go then,
        // which is not an error worth reporting.
        if sender.send(job(platform)).is_err() {
            return;
        }
    }));
    match receiver.recv_timeout(PLATFORM_ANSWER_TIMEOUT) {
        Ok(answer) => Some(answer),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // The job stays in the queue, so the platform still performs it once it
            // serves the queue again; only the answer is lost.
            vk::log(&format!(
                "[gpui_ohos] the platform thread did not answer within {PLATFORM_ANSWER_TIMEOUT:?}"
            ));
            None
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => None,
    }
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
            boot(&platform);
            platform.launch();
            inputmethod::attach();
            platform.request_frames();
            vk::log(&format!(
                "[gpui_ohos] platform thread {:?} running, windows={}",
                std::thread::current().id(),
                platform.window_count()
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
        if let Some(root) = root_dir() {
            let path = format!("{root}/panics.log");
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                use std::io::Write;
                let _ = writeln!(file, "{message}");
            }
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

/// Read a file the user picked, through the descriptor the host opened.
pub fn read_picked_file(fd: i32) -> Result<String, String> {
    host::read_fd(fd).map_err(|error| error.to_string())
}

/// Read a picked file's raw bytes.
pub fn read_picked_file_bytes(fd: i32) -> Result<Vec<u8>, String> {
    host::read_fd_bytes(fd).map_err(|error| error.to_string())
}

/// Overwrite a file the user picked, through its descriptor.
pub fn write_picked_file(fd: i32, data: &str) -> Result<(), String> {
    host::write_fd(fd, data).map_err(|error| error.to_string())
}

/// Overwrite a picked file with raw bytes.
pub fn write_picked_file_bytes(fd: i32, data: &[u8]) -> Result<(), String> {
    host::write_fd_bytes(fd, data).map_err(|error| error.to_string())
}

/// Open a picked file read/write and return its descriptor.
pub fn open_picked_file(uri: &str) -> Option<i32> {
    host::open_file(uri)
}

/// List a picked directory, on the UI thread.
pub fn list_picked_dir_async(uri: String) -> BoxFuture<'static, Vec<(String, bool, String)>> {
    Box::pin(run_on_ui(move || host::list_dir(&uri)))
}

/// Open a picked file, on the UI thread.
pub fn open_picked_file_async(uri: String) -> BoxFuture<'static, Option<i32>> {
    Box::pin(run_on_ui(move || host::open_file(&uri)))
}

/// Release a descriptor opened for a single picked-file operation.
pub fn close_picked_file(fd: i32) {
    host::close_fd(fd);
}

/// Read a picked file's bytes, on the UI thread.
pub fn read_picked_file_bytes_async(fd: i32) -> BoxFuture<'static, std::io::Result<Vec<u8>>> {
    Box::pin(run_on_ui(move || host::read_fd_bytes(fd)))
}

/// Overwrite a picked file, on the UI thread.
pub fn write_picked_file_bytes_async(
    fd: i32,
    data: Vec<u8>,
) -> BoxFuture<'static, std::io::Result<()>> {
    Box::pin(run_on_ui(move || host::write_fd_bytes(fd, &data)))
}

/// Create a directory under a picked root, on the UI thread.
pub fn create_picked_dir_async(uri: String) -> BoxFuture<'static, std::io::Result<()>> {
    Box::pin(run_on_ui(move || host::create_dir(&uri)))
}

/// Remove a file or directory under a picked root, on the UI thread.
pub fn remove_picked_path_async(
    uri: String,
    is_dir: bool,
) -> BoxFuture<'static, std::io::Result<()>> {
    Box::pin(run_on_ui(move || host::remove(&uri, is_dir)))
}

/// Rename an entry under a picked root, on the UI thread.
pub fn rename_picked_path_async(
    from: String,
    to: String,
) -> BoxFuture<'static, std::io::Result<()>> {
    Box::pin(run_on_ui(move || host::rename(&from, &to)))
}

/// List a picked directory as (name, is_directory, uri).
pub fn list_picked_dir(uri: &str) -> Vec<(String, bool, String)> {
    host::list_dir(uri)
}

/// Logical pixels per device pixel used by the backend renderer.
pub fn scale() -> f32 {
    window::SCALE
}

/// Read the system clipboard. Needs ohos.permission.READ_PASTEBOARD.
pub fn read_clipboard() -> Option<String> {
    clipboard::read_text()
}

/// Write plain text to the system clipboard. No permission required.
pub fn write_clipboard(text: &str) -> bool {
    clipboard::write_text(text)
}

/// Take the folder URI the host restored from persisted authorization.
pub fn take_restore_folder() -> Option<String> {
    with_current(|platform| platform.take_restore_folder()).flatten()
}

/// Pick files/folders and return the host's raw answer lines
/// ("f|fd|name" for files, "d|uri" for folders).
pub fn pick_items(
    files: bool,
    directories: bool,
    multiple: bool,
) -> futures::channel::oneshot::Receiver<Vec<String>> {
    with_current(|platform| platform.pick_raw(files, directories, multiple)).unwrap_or_else(|| {
        let (sender, receiver) = futures::channel::oneshot::channel();
        let _ = sender.send(Vec::new());
        receiver
    })
}

/// Install the host function pointers supplied by the ArkTS layer.
pub fn set_host_ops(ops: HostOps) {
    host::set_ops(ops);
}

/// Handle an event pushed from the ArkTS host.
pub fn host_event(kind: i32, arg: &str) {
    let arg = arg.to_string();
    post(move |platform| platform.handle_host_event(kind, &arg));
}

/// Whether the application lets this window close. Asked by the host before it
/// closes a window on its own.
pub fn should_close_window(id: &str) -> bool {
    let id = id.to_string();
    // No answer means nothing to ask: there is no platform, or it did not answer
    // within `PLATFORM_ANSWER_TIMEOUT`. Both allow the close, because the host asked
    // on behalf of a user who wants the window gone, and refusing leaves a window
    // that can never be closed.
    on_platform(move |platform| platform.should_close_window(&id)).unwrap_or(true)
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
/// Waiting for the platform thread is the handshake that makes that safe: that
/// thread is the only one rendering into the window, and the host tears the window
/// down as soon as this returns. Answering without waiting would let a frame be
/// presented into a window that no longer exists.
pub fn surface_destroyed(id: &str) {
    let id = id.to_string();
    // A timeout has no value to fall back on: this call answers with nothing, and
    // the close is the host's to make. The job stays queued and removes the surface
    // whenever the platform serves its queue again, so only the handshake above is
    // lost, and the host tears the window down with the platform thread possibly
    // still pointing at it. That risk is smaller than holding the ArkUI thread for
    // as long as the platform thread stays stuck.
    on_platform(move |platform| {
        vk::log(&format!("[gpui_ohos] surface_destroyed id={id}"));
        platform.surface_destroyed(&id);
    });
}
/// One frame's work: the input method's queued commands and cursor context, then
/// the frame itself.
fn tick_platform(platform: &Rc<OhosPlatform>) {
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

// Focus is the platform's own state now: everything that reads it runs on the
// platform's thread, so a thread local here was a second truth waiting to
// disagree with the one in OhosPlatform.

thread_local! {
    /// Keyboard modifier state, tracked from NDK key events.
    static MODIFIERS: std::cell::Cell<crate::Modifiers> =
        std::cell::Cell::new(crate::Modifiers::default());
}

fn is_modifier_key(code: i32) -> bool {
    matches!(code, 2072 | 2073 | 2047 | 2048 | 2045 | 2046 | 2076 | 2077)
}

fn update_modifiers(code: i32, down: bool) -> crate::Modifiers {
    MODIFIERS.with(|cell| {
        let mut modifiers = cell.get();
        match code {
            2072 | 2073 => modifiers.control = down,
            2047 | 2048 => modifiers.shift = down,
            2045 | 2046 => modifiers.alt = down,
            2076 | 2077 => modifiers.platform = down,
            _ => {}
        }
        cell.set(modifiers);
        modifiers
    })
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
    dispatch_key(id, down, code, modifiers, None);
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
    let unicode = u32::try_from(unicode).ok().filter(|value| *value > 0);
    dispatch_key(id, down, code, modifiers, unicode);
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

fn dispatch_key(
    id: &str,
    down: bool,
    code: i32,
    modifiers: crate::Modifiers,
    unicode: Option<u32>,
) {
    let Some((key, character)) = ohos_key(code) else {
        vk::log(&format!("[gpui_ohos] unmapped key code={code}"));
        return;
    };
    let key_char = if modifiers.control || modifiers.platform {
        None
    } else {
        unicode.and_then(char::from_u32).or(character).map(|ch| {
            if modifiers.shift {
                ch.to_uppercase().to_string()
            } else {
                ch.to_string()
            }
        })
    };
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
