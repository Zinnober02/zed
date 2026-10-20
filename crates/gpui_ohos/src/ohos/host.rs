//! Bridge to the ArkTS host.
//!
//! The host owns the OHOS window manager, file pickers, cursor and lifecycle,
//! so the backend talks to it through the function pointers supplied at startup:
//! commands, questions, and the answer to a close question. Host events (focus,
//! window status, color mode, lifecycle, picker results, query answers) arrive
//! back through \`host_event\`.

use std::collections::HashMap;
use std::ffi::{CString, c_char, c_void};

use super::fields;
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

/// 问宿主的那些问题。答案是一份 JSON 文档，形状按 op 写在 `query` 上方。
pub(crate) mod query {
    /// Open a picked file read/write and return its descriptor.
    pub const OPEN_FILE: i32 = 105;
    /// List a picked directory: [{"name": string, "is_dir": bool, "uri": string}].
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
    /// "<request number>\t<status code>\t<JSON document>", where the number is the
    /// one the backend chose and the third field is the whole answer as JSON --
    /// a number, a boolean, a string, an array or an object, per the table above
    /// `query`. It is not wrapped in a JSON string: the document is the payload.
    /// JSON escapes tabs inside strings, so splitting the first two fields at
    /// their tabs stays safe.
    pub const QUERY_ANSWER: i32 = 10;
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
    /// Give up on a question by its request number.
    ///
    /// The host keeps what it still owes an answer for; a question the watchdog
    /// has already released would otherwise stay recorded there for the life of
    /// the process, and the caller is not coming back for the answer.
    pub forget_query: Option<unsafe extern "C" fn(i32)>,
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
            // 宿主还记着这个号；告诉它不必再等答案，否则那条记录留到进程结束。
            forget_query(request);
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
/// 每个问题回答什么形状（宿主用 JSON.stringify 产生，这里按 op 取用）：
///
///   OPEN_FILE (105)        数字：picker 打开文件拿到的描述符；打不开是 null
///   LIST_DIR (106)         数组：[{"name": string, "is_dir": bool, "uri": string}]
///   RESTORE_FOLDER (107)   字符串：上次的目录 URI；没有是 null
///   CREATE_DIR (109) / REMOVE (110) / RENAME (111) / CLOSE_FILE (112)
///                          true 表示成功，其它值是错误文本
///   窗口矩形/状态/配色/id (100-103)  后端已不再提问，页面仍按 JSON 回答
///
/// 这份表与 pages/Index.ets 的 QUERY_* 分支一一对应，改一边就要改另一边。
pub(crate) async fn query(op: i32, arg: &str) -> anyhow::Result<serde_json::Value> {
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
        Ok(answer) if answer.code == 0 => parse_answer(op, &answer.text),
        Ok(answer) if answer.code == -7 => {
            anyhow::bail!("host query {op} was not answered within {QUERY_ANSWER_TIMEOUT:?}")
        }
        Ok(answer) => anyhow::bail!("host query {op} failed with status {}", answer.code),
        Err(error) => anyhow::bail!("the host never answered query {op}: {error}"),
    }
}

/// 把应答文本按 JSON 解析。
///
/// 调用方只拿得到"是个错误"，所以"宿主到底回了什么形状"只能靠这一行日志。
fn parse_answer(op: i32, text: &str) -> anyhow::Result<serde_json::Value> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => Ok(value),
        Err(error) => {
            super::vk::log(&format!(
                "[gpui_ohos] answer to query {op} is not JSON: {error} in {text:?}"
            ));
            anyhow::bail!("host query {op} answered with something that is not JSON: {error}")
        }
    }
}

/// 把应答拆成三段：请求号、状态码、答案的 JSON 文档。
///
/// 只切前两刀：第三段是整份 JSON 文档（数字、布尔、字符串、数组或对象），JSON 里的
/// 制表符只能写成 \u0009，所以第三段本身不含裸制表符。
fn answer_fields(payload: &str) -> anyhow::Result<(i32, i32, &str)> {
    let mut fields = payload.splitn(3, '\t');
    let request = fields.next().unwrap_or_default();
    let status = fields
        .next()
        .ok_or_else(|| anyhow::anyhow!("no status after the request number"))?;
    let answer = fields
        .next()
        .ok_or_else(|| anyhow::anyhow!("no answer after the status"))?;
    let request: i32 = request
        .parse()
        .map_err(|error| anyhow::anyhow!("request number {request:?} is not a number: {error}"))?;
    let code: i32 = status
        .parse()
        .map_err(|error| anyhow::anyhow!("status {status:?} is not a number: {error}"))?;
    Ok((request, code, answer))
}

/// Hand the host's answer to the caller waiting for it.
///
/// 约定只有一处：宿主回送的是 "<请求号>\t<状态码>\t<JSON 文档>"，请求号与状态码是
/// 十进制，第三段就是答案本身（见 json_answer.h）。形状按 JSON 解析留给等待者那一侧
/// ——它知道 op，能把失败写进带 op 的日志里。
pub(crate) fn complete_query(payload: &str) {
    let (request, code, encoded) = match answer_fields(payload) {
        Ok(fields) => fields,
        Err(error) => {
            // 连请求号都读不出来，就没有等待者可以叫醒：只能留下记录，等看门狗在
            // 调用方那边兜底并写另一行。
            super::vk::log(&format!(
                "[gpui_ohos] query answer {payload:?} cannot be read: {error}"
            ));
            return;
        }
    };
    let answer = QueryAnswer {
        code,
        text: encoded.to_string(),
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
/// 告诉宿主某个问题不必再等答案。
fn forget_query(request: i32) {
    let Some(ops) = OPS.get() else {
        return;
    };
    let Some(forget) = ops.forget_query else {
        return;
    };
    // SAFETY: the host installed this pointer and it stays valid for the process.
    unsafe { forget(request) };
}

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

/// Ask the host to open a picked file read/write, returning its descriptor.
///
/// 宿主回一个 JSON 数字（打不开时是 null）。不是数字就是应答坏了：留下日志，并按
/// "打不开"交还调用方 —— 那本来就是这条询问的失败形态。
pub(crate) async fn open_file(uri: String) -> Option<i32> {
    let value = match query(query::OPEN_FILE, &uri).await {
        Ok(value) => value,
        Err(error) => {
            super::vk::log(&format!(
                "[gpui_ohos] opening picked file {uri} failed: {error}"
            ));
            return None;
        }
    };
    if value.is_null() {
        return None;
    }
    match value.as_i64().and_then(|fd| i32::try_from(fd).ok()) {
        Some(fd) => Some(fd),
        None => {
            super::vk::log(&format!(
                "[gpui_ohos] answer to query {} is not a descriptor: {value}",
                query::OPEN_FILE
            ));
            None
        }
    }
}

/// 一个改动作业的结果：true 是成功，其它值是宿主给的错误文本。
async fn status(op: i32, arg: String) -> std::io::Result<()> {
    match query(op, &arg).await {
        Ok(serde_json::Value::Bool(true)) => Ok(()),
        Ok(value) => {
            let text = match value.as_str() {
                Some(text) => text.to_string(),
                None => value.to_string(),
            };
            // 作业失败与应答坏了在这里长得一样，都只表现为"没反应"；留下 op 与原文，
            // 两种都能查到。
            super::vk::log(&format!("[gpui_ohos] query {op} did not succeed: {text}"));
            Err(std::io::Error::other(text))
        }
        Err(error) => Err(std::io::Error::other(error.to_string())),
    }
}

/// Create a directory inside a picked root.
pub(crate) async fn create_dir(uri: String) -> std::io::Result<()> {
    status(query::CREATE_DIR, uri).await
}

/// Remove a file, or a directory when `is_dir` is set, inside a picked root.
///
/// 请求的载荷是 JSON：{"is_dir": bool, "uri": string}。路径里的制表符、竖线与换行
/// 都不再需要转义，宿主按 JSON 取字段。
pub(crate) async fn remove(uri: String, is_dir: bool) -> std::io::Result<()> {
    let payload = super::json_object(vec![
        (fields::IS_DIR, serde_json::Value::Bool(is_dir)),
        (fields::URI, serde_json::Value::String(uri)),
    ]);
    status(query::REMOVE, payload).await
}

/// Rename an entry inside a picked root.
///
/// 请求的载荷是 JSON：{"from": string, "to": string}。
pub(crate) async fn rename(from: String, to: String) -> std::io::Result<()> {
    let payload = super::json_object(vec![
        (fields::FROM, serde_json::Value::String(from)),
        (fields::TO, serde_json::Value::String(to)),
    ]);
    status(query::RENAME, payload).await
}

/// The folder the host wants restored, if any.
///
/// 宿主回一个 JSON 字符串（没有就是 null）。空串与 null 都算"没有目录"；别的形状是
/// 应答坏了，上报成错误，不再当成"没有"。
pub(crate) async fn restore_folder() -> anyhow::Result<Option<String>> {
    let value = query(query::RESTORE_FOLDER, "").await?;
    if value.is_null() {
        return Ok(None);
    }
    match value.as_str() {
        Some("") => Ok(None),
        Some(uri) => Ok(Some(uri.to_string())),
        None => {
            let message = format!(
                "host query {} answered with something that is not a directory: {value}",
                query::RESTORE_FOLDER
            );
            super::vk::log(&format!("[gpui_ohos] {message}"));
            Err(anyhow::anyhow!(message))
        }
    }
}

/// Ask the host to list a picked directory as (name, is_dir, uri).
///
/// A failed listing is an error and not an empty one: the caller compares
/// listings to decide what appeared and what disappeared, so an empty answer
/// reads as every file in the directory having been deleted.
pub(crate) async fn list_dir(uri: String) -> std::io::Result<Vec<(String, bool, String)>> {
    let value = query(query::LIST_DIR, &uri)
        .await
        .map_err(|error| std::io::Error::other(format!("listing {uri} failed: {error}")))?;
    parse_listing(&uri, &value).map_err(|error| {
        super::vk::log(&format!(
            "[gpui_ohos] answer to query {} is not a listing of {uri}: {error}",
            query::LIST_DIR
        ));
        error
    })
}

/// 列目录的应答：一个 JSON 数组，每项是 {"name": string, "is_dir": bool, "uri": string}。
///
/// 单独一个函数是为了能脱离宿主测：一项坏掉就整份上报，不跳过、不猜名字。
fn parse_listing(
    uri: &str,
    value: &serde_json::Value,
) -> std::io::Result<Vec<(String, bool, String)>> {
    let Some(array) = value.as_array() else {
        return Err(std::io::Error::other(format!(
            "listing {uri} is not a JSON array: {value}"
        )));
    };
    let mut entries = Vec::with_capacity(array.len());
    for entry in array {
        let name = entry[fields::NAME].as_str().ok_or_else(|| {
            std::io::Error::other(format!(
                "listing {uri} has an entry without a name: {entry}"
            ))
        })?;
        let is_dir = entry[fields::IS_DIR].as_bool().ok_or_else(|| {
            std::io::Error::other(format!(
                "listing {uri} has an entry without is_dir: {entry}"
            ))
        })?;
        let child_uri = entry[fields::URI].as_str().ok_or_else(|| {
            std::io::Error::other(format!("listing {uri} has an entry without a uri: {entry}"))
        })?;
        entries.push((name.to_string(), is_dir, child_uri.to_string()));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::{answer_fields, parse_answer, parse_listing};
    use crate::ohos::fields;
    use crate::ohos::json_value;

    #[test]
    fn answer_fields_splits_at_the_first_two_tabs() {
        assert_eq!(
            answer_fields("7\t0\t{}").expect("一个完整的空对象"),
            (7, 0, "{}")
        );
        // 第三段是整份 JSON 文档：里面的空格、逗号、方括号都原样留着。
        assert_eq!(
            answer_fields("9\t-2\t{\"a\": [1, 2]}").expect("第三段带结构的应答"),
            (9, -2, "{\"a\": [1, 2]}")
        );
    }

    #[test]
    fn answer_fields_refuses_a_payload_with_missing_fields() {
        assert!(answer_fields("7").is_err(), "没有状态码与答案");
        assert!(answer_fields("7\t0").is_err(), "没有答案");
        assert!(answer_fields("seven\t0\t{}").is_err(), "请求号不是数字");
        assert!(answer_fields("7\tzero\t{}").is_err(), "状态码不是数字");
    }

    #[test]
    fn parse_answer_reads_the_document() {
        assert_eq!(
            parse_answer(105, "42").expect("数字"),
            serde_json::json!(42)
        );
        assert_eq!(
            parse_answer(109, "true").expect("布尔"),
            serde_json::json!(true)
        );
        assert_eq!(
            parse_answer(106, "[]").expect("空数组"),
            serde_json::json!([])
        );
        assert_eq!(
            parse_answer(107, "null").expect("空值"),
            serde_json::Value::Null
        );
        assert!(parse_answer(105, "").is_err(), "空应答");
        assert!(parse_answer(105, "not json").is_err(), "不是 JSON");
    }

    fn entry(name: &str, is_dir: bool, uri: &str) -> serde_json::Value {
        json_value(vec![
            (fields::NAME, serde_json::Value::String(name.to_string())),
            (fields::IS_DIR, serde_json::Value::Bool(is_dir)),
            (fields::URI, serde_json::Value::String(uri.to_string())),
        ])
    }

    #[test]
    fn parse_listing_reads_entries() {
        let value = serde_json::Value::Array(vec![
            entry("a|b", true, "dir:x/a|b"),
            entry("line\nbreak", false, "dir:x/line\nbreak"),
        ]);
        let entries = parse_listing("dir:x", &value).expect("一份完整的列目录应答");
        assert_eq!(
            entries,
            vec![
                ("a|b".to_string(), true, "dir:x/a|b".to_string()),
                (
                    "line\nbreak".to_string(),
                    false,
                    "dir:x/line\nbreak".to_string()
                ),
            ]
        );
    }

    #[test]
    fn parse_listing_refuses_a_damaged_entry() {
        assert!(
            parse_listing("dir:x", &serde_json::json!("not an array")).is_err(),
            "不是数组"
        );
        let missing_name = json_value(vec![
            (fields::IS_DIR, serde_json::Value::Bool(true)),
            (fields::URI, serde_json::Value::String("u".to_string())),
        ]);
        assert!(
            parse_listing("dir:x", &serde_json::Value::Array(vec![missing_name])).is_err(),
            "缺 name"
        );
        let missing_is_dir = json_value(vec![
            (fields::NAME, serde_json::Value::String("a".to_string())),
            (fields::URI, serde_json::Value::String("u".to_string())),
        ]);
        assert!(
            parse_listing("dir:x", &serde_json::Value::Array(vec![missing_is_dir])).is_err(),
            "缺 is_dir"
        );
        let missing_uri = json_value(vec![
            (fields::NAME, serde_json::Value::String("a".to_string())),
            (fields::IS_DIR, serde_json::Value::Bool(true)),
        ]);
        assert!(
            parse_listing("dir:x", &serde_json::Value::Array(vec![missing_uri])).is_err(),
            "缺 uri"
        );
        let wrong_type = json_value(vec![
            (fields::NAME, serde_json::Value::String("a".to_string())),
            (fields::IS_DIR, serde_json::Value::String("yes".to_string())),
            (fields::URI, serde_json::Value::String("u".to_string())),
        ]);
        assert!(
            parse_listing("dir:x", &serde_json::Value::Array(vec![wrong_type])).is_err(),
            "is_dir 不是布尔"
        );
    }
}
