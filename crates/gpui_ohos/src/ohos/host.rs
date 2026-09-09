//! Bridge to the ArkTS host.
//!
//! The host owns the OHOS window manager, file pickers, cursor and lifecycle,
//! so the backend talks to it through two C function pointers supplied at
//! startup: a command channel and a synchronous query channel. Host events
//! (focus, window status, color mode, lifecycle, picker results) arrive back
//! through \`host_event\`.

use std::ffi::{c_char, CStr, CString};
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
}

/// Events the host pushes into the backend.
pub(crate) mod event {
    pub const APPEARANCE: i32 = 1;
    pub const FOCUS: i32 = 2;
    pub const WINDOW_STATUS: i32 = 3;
    pub const LIFECYCLE: i32 = 4;
    pub const PICK_RESULT: i32 = 5;
    pub const WINDOW_RECT: i32 = 6;
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
