//! Bridge to the ArkTS host.
//!
//! The host owns the OHOS window manager, file pickers, cursor and lifecycle,
//! so the backend talks to it through two C function pointers supplied at
//! startup: a command channel and a synchronous query channel. Host events
//! (focus, window status, color mode, lifecycle, picker results) arrive back
//! through \`host_event\`.

use std::ffi::{CString, c_char, c_void};
use std::sync::OnceLock;

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
    /// Answer a query: returns the byte count written, or negative on failure.
    pub query:
        Option<unsafe extern "C" fn(i32, *const c_char, *const c_char, *mut c_char, usize) -> i32>,
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

/// Ask the host a question, returning its UTF-8 answer.
///
/// The answer is asked for in two steps - size first, then bytes - because the
/// host truncates at whatever buffer it is handed, and a fixed buffer silently
/// cut directory listings in half. Asking twice is free: the host caches the
/// answer so the second step does not repeat the work it did for the first.
pub(crate) fn query(op: i32, arg: &str) -> anyhow::Result<String> {
    let ops = OPS
        .get()
        .ok_or_else(|| anyhow::anyhow!("host ops are not installed"))?;
    let call = ops
        .query
        .ok_or_else(|| anyhow::anyhow!("the host does not answer queries"))?;
    let id = CString::new("").expect("an empty id has no NUL");
    let arg = CString::new(arg).map_err(|_| anyhow::anyhow!("query argument contains a NUL"))?;
    let needed = unsafe { call(op, id.as_ptr(), arg.as_ptr(), std::ptr::null_mut(), 0) };
    if needed < 0 {
        anyhow::bail!("host query {op} failed while sizing its answer: {needed}");
    }
    let mut buffer = vec![0u8; needed as usize];
    let written = unsafe {
        call(
            op,
            id.as_ptr(),
            arg.as_ptr(),
            buffer.as_mut_ptr() as *mut c_char,
            buffer.len(),
        )
    };
    if written < 0 {
        anyhow::bail!("host query {op} failed while copying its answer: {written}");
    }
    buffer.truncate(written as usize);
    String::from_utf8(buffer).map_err(|error| {
        anyhow::anyhow!(
            "host query {op} answered with invalid UTF-8 at byte {}",
            error.utf8_error().valid_up_to()
        )
    })
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
pub(crate) fn close_fd(fd: i32) -> std::io::Result<()> {
    status(query::CLOSE_FILE, &fd.to_string())
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
pub(crate) fn open_file(uri: &str) -> Option<i32> {
    query(query::OPEN_FILE, uri).ok()?.trim().parse().ok()
}

/// Run a command whose answer is "0" on success.
fn status(op: i32, arg: &str) -> std::io::Result<()> {
    match query(op, arg) {
        Ok(answer) if answer.trim() == "0" => Ok(()),
        Ok(answer) => Err(std::io::Error::other(answer.trim().to_string())),
        Err(error) => Err(std::io::Error::other(error.to_string())),
    }
}

/// Create a directory inside a picked root.
pub(crate) fn create_dir(uri: &str) -> std::io::Result<()> {
    status(query::CREATE_DIR, uri)
}

/// Remove a file, or a directory when `is_dir` is set, inside a picked root.
pub(crate) fn remove(uri: &str, is_dir: bool) -> std::io::Result<()> {
    status(
        query::REMOVE,
        &format!("{}	{}", if is_dir { 1 } else { 0 }, encode_field(uri)),
    )
}

/// Rename an entry inside a picked root.
pub(crate) fn rename(from: &str, to: &str) -> std::io::Result<()> {
    status(
        query::RENAME,
        &format!("{}	{}", encode_field(from), encode_field(to)),
    )
}

/// The folder the host wants restored, if any.
pub(crate) fn restore_folder() -> Option<String> {
    let answer = query(query::RESTORE_FOLDER, "").ok()?;
    if answer.is_empty() {
        return None;
    }
    Some(answer)
}

/// Ask the host to list a picked directory as (name, is_dir, uri).
///
/// A failed listing is an error and not an empty one: the caller compares
/// listings to decide what appeared and what disappeared, so an empty answer
/// reads as every file in the directory having been deleted.
pub(crate) fn list_dir(uri: &str) -> std::io::Result<Vec<(String, bool, String)>> {
    let answer = query(query::LIST_DIR, uri)
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
