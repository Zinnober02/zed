//! Bridge to the ArkTS host.
//!
//! The host owns the OHOS window manager, file pickers, cursor and lifecycle,
//! so the backend talks to it through the function pointers supplied at startup:
//! commands, questions, and the answer to a close question. Host events (focus,
//! window status, color mode, lifecycle, picker results, query answers) arrive
//! back through \`host_event\`.

use std::collections::HashMap;
use std::ffi::{CString, c_char, c_void};
use std::sync::{
    Condvar, Mutex, OnceLock,
    atomic::{AtomicI32, Ordering},
};
use std::time::{Duration, Instant};

use futures::channel::oneshot;

/// Commands the backend sends to the host. Fire-and-forget.
pub(crate) mod op {
    pub const SET_TITLE: i32 = 1;
    pub const MINIMIZE: i32 = 2;
    pub const MAXIMIZE: i32 = 3;
    pub const SET_FULLSCREEN: i32 = 4;
    pub const START_MOVE: i32 = 5;
    pub const SET_DECOR: i32 = 7;
    pub const OPEN_URL: i32 = 8;
    pub const REVEAL_PATH: i32 = 9;
    pub const OPEN_WITH: i32 = 10;
    pub const SET_CURSOR: i32 = 11;
    pub const PICK_PATHS: i32 = 12;
    pub const PICK_NEW_PATH: i32 = 13;
    pub const REQUEST_FOCUS: i32 = 15;
    /// Ask the host to draw a frame now rather than at the next refresh.
    /// Create another OHOS window with an XComponent named by the argument.
    pub const CREATE_WINDOW: i32 = 16;
    /// Which way round the theme is: the host draws the window buttons for it.
    pub const SET_APPEARANCE: i32 = 18;
    /// Close the window this id names, because the application is done with it.
    pub const CLOSE_WINDOW: i32 = 19;
    /// The application is quitting: it has finished, and the host should end it.
    pub const QUIT: i32 = 20;
    /// What shows through the window: opaque, transparent or blurred, as the
    /// theme asks for.
    pub const SET_BACKGROUND: i32 = 21;
}

/// Synchronous queries. The host writes a UTF-8 answer into the buffer.
pub(crate) mod query {
    /// Open a picked file read/write and return its descriptor.
    pub const OPEN_FILE: i32 = 105;
    /// List a picked directory: "name|isDir|uri" per line, name and uri escaped
    /// by `encode_field`.
    pub const LIST_DIR: i32 = 106;
    /// The folder to restore, if the host has one. Pulled instead of pushed so
    /// the backend cannot miss it by starting a moment late.
    pub const RESTORE_FOLDER: i32 = 107;
    /// Create a directory inside a picked root.
    pub const CREATE_DIR: i32 = 109;
    /// Remove a file (or a directory) inside a picked root.
    pub const REMOVE: i32 = 110;
    /// Rename an entry inside a picked root.
    pub const RENAME: i32 = 111;
    /// Release a descriptor the host opened for one operation.
    pub const CLOSE_FILE: i32 = 112;
}

/// Events the host pushes into the backend.
pub(crate) mod event {
    pub const APPEARANCE: i32 = 1;
    pub const FOCUS: i32 = 2;
    pub const WINDOW_STATUS: i32 = 3;
    pub const LIFECYCLE: i32 = 4;
    pub const PICK_RESULT: i32 = 5;
    pub const WINDOW_RECT: i32 = 6;
    /// A folder URI to restore after the app restarts.
    pub const RESTORE_FOLDER: i32 = 7;
    /// Enable the forced-redraw frame-rate benchmark; the argument is the
    /// number of frames to present.
    pub const BENCHMARK: i32 = 8;
    /// The pointer entered or left a window: "<surface id>\t1" or "...\t0".
    pub const HOVER: i32 = 9;
    /// The host answers a question the backend asked: the argument is
    /// "<request number>\t<status code>\t<answer>", where the number is the one
    /// the backend chose and the answer is escaped by `encode_field`'s rules
    /// because it may hold a newline or a tab of its own.
    pub const QUERY_ANSWER: i32 = 10;
}

/// 把字段里的分隔符换成转义序列，分隔符本身只在字段之间起作用。
///
/// 标题、路径和目录项名字里都可能出现竖线与制表符，旧协议把它们当成了字段
/// 边界：标题含制表符时整条建窗请求的字段全部错位，路径含竖线时会打开另一个
/// 真实文件，目录项名字含竖线时这个文件是目录还是文件会被判反。
/// 调用方只对单个字段调用它，不要对拼好的整条应答调用。
pub(crate) fn encode_field(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '%' => encoded.push_str("%25"),
            '|' => encoded.push_str("%7C"),
            '\t' => encoded.push_str("%09"),
            '\n' => encoded.push_str("%0A"),
            other => encoded.push(other),
        }
    }
    encoded
}

/// 把字段还原成原来的文本。
///
/// 认不出的转义按错误上报，不猜也不截断：应答在传输中被改过的时候，任何猜测
/// 都等于让后续操作作用在用户没有点过的文件上。
pub(crate) fn decode_field(value: &str) -> anyhow::Result<String> {
    let mut decoded = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            decoded.push(character);
            continue;
        }
        let code: String = characters.by_ref().take(2).collect();
        let replacement = match code.as_str() {
            "25" => '%',
            "7C" => '|',
            "09" => '\t',
            "0A" => '\n',
            other => anyhow::bail!("undeclared escape %{other} in field {value:?}"),
        };
        decoded.push(replacement);
    }
    Ok(decoded)
}

/// Function pointers implemented by the ArkTS host.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostOps {
    /// Run a command: returns 0 on success, negative on failure. An empty id means
    /// the command names no window.
    pub window_op: Option<unsafe extern "C" fn(i32, *const c_char, *const c_char) -> i32>,
    /// Hand a question to the host and return at once. The host answers exactly
    /// once through `host_event` with `event::QUERY_ANSWER` and this request
    /// number; a negative return means the question was not taken and no answer
    /// will come.
    pub begin_query: Option<unsafe extern "C" fn(i32, *const c_char, *const c_char, i32) -> i32>,
    /// The platform's answer to a close question the host asked about a window.
    pub answer_close: Option<unsafe extern "C" fn(*const c_char, bool)>,
}

static OPS: OnceLock<HostOps> = OnceLock::new();

pub(crate) fn set_ops(ops: HostOps) {
    let _ = OPS.set(ops);
    super::vk::log("[gpui_ohos] host ops installed");
}

/// Send a command that names the window it targets.
pub(crate) fn window_op_for(id: &str, op: i32, arg: &str) {
    super::vk::log(&format!("[gpui_ohos] host op {op}: {id}\t{arg}"));
    send(id, op, arg);
}

/// Send a command to the host. Silently no-ops when the host is absent.
pub(crate) fn window_op(op: i32, arg: &str) {
    super::vk::log(&format!("[gpui_ohos] host op {op}: {arg}"));
    send("", op, arg);
}

/// Send a command that fires often enough that logging it would drown the log.
pub(crate) fn window_op_quiet(op: i32, arg: &str) {
    send("", op, arg);
}

/// The window and the payload travel separately: the host used to split them out
/// of one string, and a payload that happened to contain the separator was read as
/// a different window's command.
fn send(id: &str, op: i32, arg: &str) {
    let Some(ops) = OPS.get() else {
        return;
    };
    let Some(call) = ops.window_op else {
        return;
    };
    let Ok(id) = CString::new(id) else {
        return;
    };
    let Ok(arg) = CString::new(arg) else {
        return;
    };
    unsafe { call(op, id.as_ptr(), arg.as_ptr()) };
}

/// Ask for a frame now. gpui calls this as soon as anything changes, and the
/// platform thread draws one immediately instead of waiting for the next refresh,
/// which is what keeps a keystroke from landing a period late. Going out to the
/// host and back for this cost a trip through the page's JavaScript before
/// anything could be drawn at all.
pub(crate) fn request_frame() {
    super::want_tick();
}

/// Numbers for the questions this side asks.
///
/// The number, and not the position in any queue, is what matches an answer to the
/// caller still waiting for it: the answer is produced on the host's thread, which
/// knows nothing about who is waiting.
fn next_request_number() -> i32 {
    static NEXT: AtomicI32 = AtomicI32::new(0);
    loop {
        let number = NEXT.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        if number > 0 {
            return number;
        }
    }
}

/// One answer from the host: the status it gave and the text it produced.
struct QueryAnswer {
    code: i32,
    text: String,
}

/// One question waiting for its answer, and when it stops being worth waiting for.
struct PendingQuery {
    op: i32,
    sender: oneshot::Sender<QueryAnswer>,
    deadline: Instant,
}

/// 还在等回答的调用方，按请求号排列。
struct QueryWaiters {
    entries: Mutex<HashMap<i32, PendingQuery>>,
    /// 唤醒看门狗重算下一次醒来的时刻：有新问题进来，或者有问题被答掉了。
    changed: Condvar,
}

impl QueryWaiters {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i32, PendingQuery>> {
        // 写入者只有本文件，所以锁中毒只可能来自持锁时的 panic。这份表是一张待配对表，
        // 取回内层值继续用比让此后每个问题都失败更接近事实。
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

static QUERY_WAITERS: OnceLock<QueryWaiters> = OnceLock::new();
static QUERY_WATCHDOG: OnceLock<()> = OnceLock::new();

/// 多少算"没有人回答"。
///
/// 页面那一侧为一个问题只做几个文件系统调用，正常一帧之内就回；能超过这个数说明页面
/// 根本没有在处理问题。这是兜底，正常路径上它不会醒；没有它，页面卡住的问题会让调用方
/// 永远等下去，而且连一行日志都不留。
const QUERY_ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

fn query_waiters() -> &'static QueryWaiters {
    let waiters = QUERY_WAITERS.get_or_init(|| QueryWaiters {
        entries: Mutex::new(HashMap::new()),
        changed: Condvar::new(),
    });
    // 每个进程一个看门狗，第一次提问时启动。它不为单个问题注册定时器：只在最早的
    // 截止时刻醒一次，被应答唤醒就重算，所以正常路径上它一直睡着。
    QUERY_WATCHDOG.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("gpui-host-queries".to_string())
            .spawn(move || watch_queries(waiters));
        if let Err(error) = spawned {
            super::vk::log(&format!(
                "[gpui_ohos] no watchdog for unanswered questions: {error}"
            ));
        }
    });
    waiters
}

/// 把久到没有回答的问题交还给它自己的调用方，附一个错误。
fn watch_queries(waiters: &'static QueryWaiters) {
    loop {
        let expired = {
            let mut entries = waiters.lock();
            loop {
                // 先把最早的时刻算出来再放掉迭代器：它还借着这张表，而下面两个分支都要
                // 再次动它。
                let next = { entries.values().map(|entry| entry.deadline).min() };
                let Some(next) = next else {
                    // 没有待答的问题，睡到有人提问。
                    entries = waiters
                        .changed
                        .wait(entries)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    continue;
                };
                let now = Instant::now();
                if next <= now {
                    break;
                }
                let wait = next.saturating_duration_since(now);
                match waiters.changed.wait_timeout(entries, wait) {
                    Ok((guard, _)) => entries = guard,
                    Err(poisoned) => entries = poisoned.into_inner().0,
                }
            }
            let now = Instant::now();
            let due: Vec<i32> = entries
                .iter()
                .filter(|(_, entry)| entry.deadline <= now)
                .map(|(request, _)| *request)
                .collect();
            let mut expired = Vec::with_capacity(due.len());
            for request in due {
                if let Some(entry) = entries.remove(&request) {
                    expired.push((request, entry.op, entry.sender));
                }
            }
            expired
        };
        for (request, op, sender) in expired {
            super::vk::log(&format!(
                "[gpui_ohos] query {request} (op {op}) has no answer after \
                 {QUERY_ANSWER_TIMEOUT:?}; releasing its caller with an error"
            ));
            if sender
                .send(QueryAnswer {
                    code: -7,
                    text: String::new(),
                })
                .is_err()
            {
                super::vk::log(&format!(
                    "[gpui_ohos] query {request} timed out with nobody left waiting"
                ));
            }
        }
    }
}

/// Ask the host a question and wait for its answer without holding a thread.
///
/// The question goes out with a number this side chose and comes back through
/// `event::QUERY_ANSWER`, so neither thread waits for the other: the host's page
/// answers on its own thread, and the caller is woken wherever it happens to be
/// waiting. A question that cannot be answered still wakes its caller - refused
/// outright, or left unanswered past `QUERY_ANSWER_TIMEOUT` where the watchdog
/// releases it with an error and a log line - because waiting for an answer that
/// will never come is the failure this replaced.
pub(crate) async fn query(op: i32, arg: &str) -> anyhow::Result<String> {
    let ops = OPS
        .get()
        .ok_or_else(|| anyhow::anyhow!("host ops are not installed"))?;
    let call = ops
        .begin_query
        .ok_or_else(|| anyhow::anyhow!("the host does not answer questions"))?;
    let id = CString::new("").expect("an empty id has no NUL");
    let arg_text =
        CString::new(arg).map_err(|_| anyhow::anyhow!("query argument contains a NUL"))?;
    let waiters = query_waiters();
    let request = next_request_number();
    let (sender, receiver) = oneshot::channel();
    // Registered before the question leaves: the page may answer it before this
    // call returns, and that answer has to find its waiter.
    waiters.lock().insert(
        request,
        PendingQuery {
            op,
            sender,
            deadline: Instant::now() + QUERY_ANSWER_TIMEOUT,
        },
    );
    // 新问题的截止时刻可能更早，看门狗要重算。
    waiters.changed.notify_all();
    let status = unsafe { call(op, id.as_ptr(), arg_text.as_ptr(), request) };
    if status < 0 {
        // Refused: no answer will come, so the waiter has to go now.
        let taken = waiters.lock().remove(&request);
        if taken.is_some() {
            waiters.changed.notify_all();
        }
        match taken {
            Some(_) => {}
            None => super::vk::log(&format!(
                "[gpui_ohos] question {request} was refused after its waiter went away"
            )),
        }
        anyhow::bail!("host query {op} was not taken: {status}");
    }
    match receiver.await {
        Ok(answer) if answer.code == 0 => Ok(answer.text),
        Ok(answer) if answer.code == -7 => {
            anyhow::bail!("host query {op} was not answered within {QUERY_ANSWER_TIMEOUT:?}")
        }
        Ok(answer) => anyhow::bail!("host query {op} failed with status {}", answer.code),
        Err(error) => anyhow::bail!("the host never answered query {op}: {error}"),
    }
}

/// Hand the host's answer to the caller waiting for it.
///
/// The text is escaped at the field boundary like every other field the host
/// sends: a directory listing holds newlines and a name may hold a tab, and either
/// one would cut the answer short. An escape this side does not know is an error
/// handed to the caller; guessing would answer with somebody else's text.
pub(crate) fn complete_query(payload: &str) {
    let missing = |what: &str| {
        super::vk::log(&format!(
            "[gpui_ohos] query answer {payload:?} has no {what}"
        ));
    };
    let Some((request, rest)) = payload.split_once('\t') else {
        missing("request number");
        return;
    };
    let Ok(request) = request.parse::<i32>() else {
        missing("readable request number");
        return;
    };
    let Some((status, encoded)) = rest.split_once('\t') else {
        missing("status");
        return;
    };
    let Ok(code) = status.parse::<i32>() else {
        missing("readable status");
        return;
    };
    let answer = match decode_field(encoded) {
        Ok(text) => QueryAnswer { code, text },
        Err(error) => {
            super::vk::log(&format!(
                "[gpui_ohos] answer to query {request} was damaged in transit: {error}"
            ));
            QueryAnswer {
                code: -11,
                text: String::new(),
            }
        }
    };
    let waiters = query_waiters();
    let taken = waiters.lock().remove(&request);
    if taken.is_some() {
        // 这个问题不再需要看门狗：叫醒它去重算下一次醒来的时刻。
        waiters.changed.notify_all();
    }
    match taken {
        Some(entry) => {
            if entry.sender.send(answer).is_err() {
                super::vk::log(&format!(
                    "[gpui_ohos] answer to query {request} arrived too late"
                ));
            }
        }
        None => super::vk::log(&format!(
            "[gpui_ohos] answer to query {request} arrived with nobody waiting"
        )),
    }
}

/// Report the platform's answer to a close question about one window.
///
/// The host's close handler already returned, so this settles a promise there
/// rather than returning a value: the answer is the platform's to give and the
/// question cannot wait for it.
pub(crate) fn answer_close(id: &str, allowed: bool) {
    let Some(ops) = OPS.get() else {
        super::vk::log("[gpui_ohos] no host ops to answer a close question");
        return;
    };
    let Some(call) = ops.answer_close else {
        super::vk::log("[gpui_ohos] the host does not take close answers");
        return;
    };
    let Ok(id) = CString::new(id) else {
        super::vk::log("[gpui_ohos] the window id of a close answer contains a NUL");
        return;
    };
    unsafe { call(id.as_ptr(), allowed) };
}

const SEEK_SET: i32 = 0;

unsafe extern "C" {
    fn read(fd: i32, buffer: *mut c_void, count: usize) -> isize;
    fn write(fd: i32, buffer: *const c_void, count: usize) -> isize;
    fn lseek(fd: i32, offset: i64, whence: i32) -> i64;
    fn ftruncate(fd: i32, length: i64) -> i32;
}

/// Read a picked public file through the descriptor the host opened for it.
pub(crate) fn read_fd_bytes(fd: i32) -> std::io::Result<Vec<u8>> {
    let position = unsafe { lseek(fd, 0, SEEK_SET) };
    let mut out = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = unsafe { read(fd, buffer.as_mut_ptr() as *mut c_void, buffer.len()) };
        if count < 0 {
            let error = std::io::Error::last_os_error();
            crate::log_line(&format!(
                "read_fd {fd} failed after {} bytes (position {position}): {error}",
                out.len()
            ));
            return Err(error);
        }
        if count == 0 {
            break;
        }
        out.extend_from_slice(&buffer[..count as usize]);
    }
    crate::log_line(&format!(
        "read_fd {fd} read {} bytes (position {position})",
        out.len()
    ));
    Ok(out)
}

/// Release a descriptor the host opened for a single operation.
///
/// The host closes it, because the page holds the `fs.File` that opened it: a
/// raw `close` here released the number while that object stayed alive, and the
/// object closed whatever the process had handed the same number to afterwards.
pub(crate) async fn close_fd(fd: i32) -> std::io::Result<()> {
    status(query::CLOSE_FILE, fd.to_string()).await
}

/// Read a picked public file as UTF-8 text.
pub(crate) fn read_fd(fd: i32) -> std::io::Result<String> {
    let bytes = read_fd_bytes(fd)?;
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Replace a picked public file's contents through its descriptor.
pub(crate) fn write_fd_bytes(fd: i32, bytes: &[u8]) -> std::io::Result<()> {
    unsafe {
        ftruncate(fd, 0);
        lseek(fd, 0, SEEK_SET);
    }
    let mut written = 0usize;
    while written < bytes.len() {
        let count = unsafe {
            write(
                fd,
                bytes[written..].as_ptr() as *const c_void,
                bytes.len() - written,
            )
        };
        if count <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        written += count as usize;
    }
    Ok(())
}

/// Replace a picked public file's contents with UTF-8 text.
pub(crate) fn write_fd(fd: i32, data: &str) -> std::io::Result<()> {
    write_fd_bytes(fd, data.as_bytes())
}

/// Ask the host to open a picked file read/write, returning its descriptor.
pub(crate) async fn open_file(uri: String) -> Option<i32> {
    query(query::OPEN_FILE, &uri)
        .await
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Run a command whose answer is "0" on success.
async fn status(op: i32, arg: String) -> std::io::Result<()> {
    match query(op, &arg).await {
        Ok(answer) if answer.trim() == "0" => Ok(()),
        Ok(answer) => Err(std::io::Error::other(answer.trim().to_string())),
        Err(error) => Err(std::io::Error::other(error.to_string())),
    }
}

/// Create a directory inside a picked root.
pub(crate) async fn create_dir(uri: String) -> std::io::Result<()> {
    status(query::CREATE_DIR, uri).await
}

/// Remove a file, or a directory when `is_dir` is set, inside a picked root.
pub(crate) async fn remove(uri: String, is_dir: bool) -> std::io::Result<()> {
    status(
        query::REMOVE,
        format!("{}\t{}", if is_dir { 1 } else { 0 }, encode_field(&uri)),
    )
    .await
}

/// Rename an entry inside a picked root.
pub(crate) async fn rename(from: String, to: String) -> std::io::Result<()> {
    status(
        query::RENAME,
        format!("{}\t{}", encode_field(&from), encode_field(&to)),
    )
    .await
}

/// The folder the host wants restored, if any.
///
/// The error is kept apart from the empty answer: "the host has no folder" and
/// "the question failed" lead to the same silence, and only one of them is a bug.
pub(crate) async fn restore_folder() -> anyhow::Result<Option<String>> {
    let answer = query(query::RESTORE_FOLDER, "").await?;
    if answer.is_empty() {
        return Ok(None);
    }
    Ok(Some(answer))
}

/// Ask the host to list a picked directory as (name, is_dir, uri).
///
/// A failed listing is an error and not an empty one: the caller compares
/// listings to decide what appeared and what disappeared, so an empty answer
/// reads as every file in the directory having been deleted.
pub(crate) async fn list_dir(uri: String) -> std::io::Result<Vec<(String, bool, String)>> {
    let answer = query(query::LIST_DIR, &uri)
        .await
        .map_err(|error| std::io::Error::other(format!("listing {uri} failed: {error}")))?;
    let mut entries = Vec::new();
    for line in answer.lines() {
        // A line the host could not have produced means the answer is damaged, and
        // guessing which entry it was meant to be would act on the wrong file.
        let mut parts = line.splitn(3, '|');
        let (Some(name), Some(is_dir), Some(child_uri)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(std::io::Error::other(format!(
                "malformed listing line for {uri}: {line}"
            )));
        };
        let name = decode_field(name).map_err(|error| {
            std::io::Error::other(format!("listing {uri} has a damaged name: {error}"))
        })?;
        let child_uri = decode_field(child_uri).map_err(|error| {
            std::io::Error::other(format!("listing {uri} has a damaged path: {error}"))
        })?;
        entries.push((name, is_dir == "1", child_uri));
    }
    Ok(entries)
}
