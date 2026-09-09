//! Bridge to the ArkTS host.
//!
//! The host owns the OHOS window manager, file pickers, cursor and lifecycle,
//! so the backend talks to it through two C function pointers supplied at
//! startup: a command channel and a synchronous query channel. Host events
//! (focus, window status, color mode, lifecycle, picker results) arrive back
//! through \`host_event\`.

use std::ffi::{c_char, c_void, CStr, CString};
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
    pub const SET_MENUS: i32 = 14;
    pub const REQUEST_FOCUS: i32 = 15;
}

/// Synchronous queries. The host writes a UTF-8 answer into the buffer.
pub(crate) mod query {
    pub const WINDOW_RECT: i32 = 100;
    pub const WINDOW_STATUS: i32 = 101;
    pub const COLOR_MODE: i32 = 102;
    pub const WINDOW_ID: i32 = 103;
    pub const DISPLAY: i32 = 104;
    /// Open a picked file read/write and return its descriptor.
    pub const OPEN_FILE: i32 = 105;
    /// List a picked directory: "name|isDir|uri" per line.
    pub const LIST_DIR: i32 = 106;
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

/// Send a command to the host. Silently no-ops when the host is absent.
pub(crate) fn window_op(op: i32, arg: &str) {
    let Some(ops) = OPS.get() else {
        return;
    };
    let Some(call) = ops.window_op else {
        return;
    };
    super::vk::log(&format!("[gpui_ohos] host op {op}: {arg}"));
    let Ok(arg) = CString::new(arg) else {
        return;
    };
    unsafe { call(op, arg.as_ptr()) };
}

/// Ask the host a question, returning its UTF-8 answer.
pub(crate) fn query(op: i32, arg: &str) -> Option<String> {
    let ops = OPS.get()?;
    let call = ops.query?;
    let arg = CString::new(arg).ok()?;
    let mut buffer = vec![0u8; 4096];
    let written = unsafe { call(op, arg.as_ptr(), buffer.as_mut_ptr() as *mut c_char, buffer.len()) };
    if written < 0 {
        return None;
    }
    buffer.truncate((written as usize).min(buffer.len()));
    while buffer.last() == Some(&0) {
        buffer.pop();
    }
    String::from_utf8(buffer).ok()
}

/// Convert a host-provided C string to an owned Rust string.
pub(crate) fn cstr(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

const SEEK_SET: i32 = 0;

unsafe extern "C" {
    fn read(fd: i32, buffer: *mut c_void, count: usize) -> isize;
    fn write(fd: i32, buffer: *const c_void, count: usize) -> isize;
    fn lseek(fd: i32, offset: i64, whence: i32) -> i64;
    fn ftruncate(fd: i32, length: i64) -> i32;
}

/// Read a picked public file through the descriptor the host opened for it.
pub(crate) fn read_fd(fd: i32) -> std::io::Result<String> {
    unsafe { lseek(fd, 0, SEEK_SET) };
    let mut out = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = unsafe { read(fd, buffer.as_mut_ptr() as *mut c_void, buffer.len()) };
        if count < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if count == 0 {
            break;
        }
        out.extend_from_slice(&buffer[..count as usize]);
    }
    String::from_utf8(out)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Replace a picked public file's contents through its descriptor.
pub(crate) fn write_fd(fd: i32, data: &str) -> std::io::Result<()> {
    unsafe {
        ftruncate(fd, 0);
        lseek(fd, 0, SEEK_SET);
    }
    let bytes = data.as_bytes();
    let mut written = 0usize;
    while written < bytes.len() {
        let count =
            unsafe { write(fd, bytes[written..].as_ptr() as *const c_void, bytes.len() - written) };
        if count <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        written += count as usize;
    }
    Ok(())
}

/// Ask the host to open a picked file read/write, returning its descriptor.
pub(crate) fn open_file(uri: &str) -> Option<i32> {
    query(query::OPEN_FILE, uri)?.trim().parse().ok()
}

/// Ask the host to list a picked directory as (name, is_dir, uri).
pub(crate) fn list_dir(uri: &str) -> Vec<(String, bool, String)> {
    let Some(answer) = query(query::LIST_DIR, uri) else {
        return Vec::new();
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
