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
    pub const START_RESIZE: i32 = 6;
    pub const SET_DECOR: i32 = 7;
    pub const OPEN_URL: i32 = 8;
    pub const REVEAL_PATH: i32 = 9;
    pub const OPEN_WITH: i32 = 10;
    pub const SET_CURSOR: i32 = 11;
    pub const PICK_PATHS: i32 = 12;
    pub const PICK_NEW_PATH: i32 = 13;
    pub const REQUEST_FOCUS: i32 = 15;
    /// Ask the host to draw a frame now rather than at the next refresh.
    pub const REQUEST_FRAME: i32 = 17;
    /// Create another OHOS window with an XComponent named by the argument.
    pub const CREATE_WINDOW: i32 = 16;
    /// Which way round the theme is: the host draws the window buttons for it.
    pub const SET_APPEARANCE: i32 = 18;
    /// Close the window this id names, because the application is done with it.
    pub const CLOSE_WINDOW: i32 = 19;
}

/// Synchronous queries. The host writes a UTF-8 answer into the buffer.
pub(crate) mod query {
    pub const WINDOW_RECT: i32 = 100;
    pub const COLOR_MODE: i32 = 102;
    /// Open a picked file read/write and return its descriptor.
    pub const OPEN_FILE: i32 = 105;
    /// List a picked directory: "name|isDir|uri" per line.
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
    /// A window is gone: the platform closed it, and the application has to hear
    /// about it or it keeps the window's workspace in its session for ever.
    pub const WINDOW_CLOSED: i32 = 9;
}

/// Function pointers implemented by the ArkTS host.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostOps {
    /// Run a command: returns 0 on success, negative on failure.
    pub window_op: Option<unsafe extern "C" fn(i32, *const c_char) -> i32>,
    /// Answer a query: returns the byte count written, or negative on failure.
    pub query: Option<unsafe extern "C" fn(i32, *const c_char, *mut c_char, usize) -> i32>,
}

static OPS: OnceLock<HostOps> = OnceLock::new();

pub(crate) fn set_ops(ops: HostOps) {
    let _ = OPS.set(ops);
    super::vk::log("[gpui_ohos] host ops installed");
}

/// Send a command that names the window it targets. The host strips the id
/// prefix before handling the payload, so ids must not contain a tab.
pub(crate) fn window_op_for(id: &str, op: i32, arg: &str) {
    window_op(op, &format!("{id}\t{arg}"));
}

/// Send a command to the host. Silently no-ops when the host is absent.
pub(crate) fn window_op(op: i32, arg: &str) {
    super::vk::log(&format!("[gpui_ohos] host op {op}: {arg}"));
    send(op, arg);
}

/// Send a command that fires often enough that logging it would drown the log.
pub(crate) fn window_op_quiet(op: i32, arg: &str) {
    send(op, arg);
}

fn send(op: i32, arg: &str) {
    let Some(ops) = OPS.get() else {
        return;
    };
    let Some(call) = ops.window_op else {
        return;
    };
    let Ok(arg) = CString::new(arg) else {
        return;
    };
    unsafe { call(op, arg.as_ptr()) };
}

/// Ask for a frame now. gpui calls this as soon as anything changes, and the
/// host answers by running a frame immediately instead of waiting for the next
/// refresh, which is what keeps a keystroke from landing a period late.
pub(crate) fn request_frame() {
    window_op_quiet(op::REQUEST_FRAME, "");
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
    let arg = CString::new(arg).map_err(|_| anyhow::anyhow!("query argument contains a NUL"))?;
    let needed = unsafe { call(op, arg.as_ptr(), std::ptr::null_mut(), 0) };
    if needed < 0 {
        anyhow::bail!("host query {op} failed while sizing its answer: {needed}");
    }
    let mut buffer = vec![0u8; needed as usize];
    let written = unsafe {
        call(
            op,
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
    fn close(fd: i32) -> i32;
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
/// Nothing else closes these: leaking one per read or write exhausts the
/// process descriptor table after a few hundred operations.
pub(crate) fn close_fd(fd: i32) {
    let closed = unsafe { close(fd) };
    if closed != 0 {
        crate::log_line(&format!(
            "close_fd {fd} failed: {}",
            std::io::Error::last_os_error()
        ));
    }
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
        &format!("{}	{uri}", if is_dir { 1 } else { 0 }),
    )
}

/// Rename an entry inside a picked root.
pub(crate) fn rename(from: &str, to: &str) -> std::io::Result<()> {
    status(query::RENAME, &format!("{from}	{to}"))
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
pub(crate) fn list_dir(uri: &str) -> Vec<(String, bool, String)> {
    let answer = match query(query::LIST_DIR, uri) {
        Ok(answer) => answer,
        Err(error) => {
            // An empty list and a failed query used to look identical to the
            // caller, which turned a broken listing into an empty directory.
            crate::log_line(&format!("list_dir failed: {error}"));
            return Vec::new();
        }
    };
    answer
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '|');
            let name = parts.next()?.to_string();
            let is_dir = parts.next()? == "1";
            let child_uri = parts.next()?.to_string();
            Some((name, is_dir, child_uri))
        })
        .collect()
}
