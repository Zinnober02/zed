#![cfg(target_env = "ohos")]

//! OHOS entry point for Zed.
//!
//! The ArkTS/C++ host creates an XComponent, then calls
//! ohos_gpui_app_main with its surface. The shared platform receives the
//! surface, Zed builds its application on it, and the host drives the frames.

mod host_abi;

use std::ffi::{CStr, c_char, c_void};

use gpui_ohos::LogFn;

/// Called by the host from OnSurfaceCreated.
#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_app_main(
    id: *const c_char,
    window: *mut c_void,
    width: u32,
    height: u32,
    log: LogFn,
) -> i32 {
    let id = if id.is_null() {
        String::new()
    } else {
        // SAFETY: the host passes a valid, NUL-terminated identifier.
        unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned()
    };
    gpui_ohos::run_app_on_surface(&id, window, width, height, log, zed_app::run)
}
