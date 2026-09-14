//! C ABI expected by the HAP host (see vulkan_shell/entry/src/main/cpp/napi_init.cpp).
//!
//! These are thin wrappers over gpui_ohos; they live here rather than in
//! gpui_ohos so the linker keeps them in the cdylib.

use std::ffi::{CStr, c_char, c_void};

fn cstr(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the host passes NUL-terminated strings.
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .ok()
        .map(str::to_string)
}

fn text(ptr: *const c_char) -> String {
    cstr(ptr).unwrap_or_default()
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_set_root_dir(path: *const c_char) {
    if let Some(value) = cstr(path) {
        gpui_ohos::set_root_dir(&value);
    }
}

/// Ask the application whether this window may close.
///
/// The platform has to ask before a window closes: that question is what runs
/// the application's own close handling, which saves dirty buffers and takes the
/// window's workspace out of the session. Closing without asking removed the
/// window under the application's feet.
#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_should_close_window(id: *const c_char) -> bool {
    gpui_ohos::should_close_window(&text(id))
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_key_event(id: *const c_char, action: i32, code: i32, unicode: i32) {
    gpui_ohos::key_event(&text(id), action, code, unicode);
}

/// Returns 1 to consume the event before the input method sees it.
#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_key_pre_ime(
    id: *const c_char,
    action: i32,
    code: i32,
    unicode: i32,
) -> i32 {
    i32::from(gpui_ohos::key_pre_ime(&text(id), action, code, unicode))
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_scale() -> f32 {
    gpui_ohos::scale()
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_ime_commit(commit: *const c_char) {
    if let Some(value) = cstr(commit) {
        gpui_ohos::ime_commit_text(&value);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_ime_delete_backward(length: i32) {
    gpui_ohos::ime_delete_backward(length.max(0) as usize);
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_ime_delete_forward(length: i32) {
    gpui_ohos::ime_delete_forward(length.max(0) as usize);
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_ime_preview(preview: *const c_char) {
    if let Some(value) = cstr(preview) {
        gpui_ohos::ime_preview_text(&value);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_ime_finish_preview() {
    gpui_ohos::ime_finish_preview();
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_surface_created(
    id: *const c_char,
    window: *mut c_void,
    width: u32,
    height: u32,
) {
    gpui_ohos::surface_created(&text(id), window, width, height);
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_surface_resized(id: *const c_char, width: u32, height: u32) {
    gpui_ohos::surface_resized(&text(id), width, height);
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_surface_destroyed(id: *const c_char) {
    gpui_ohos::surface_destroyed(&text(id));
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_tick() {
    gpui_ohos::tick();
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_pointer_down(id: *const c_char, x: f32, y: f32, button: u32) {
    gpui_ohos::pointer_down(&text(id), x, y, button);
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_pointer_up(id: *const c_char, x: f32, y: f32, button: u32) {
    gpui_ohos::pointer_up(&text(id), x, y, button);
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_pointer_move(id: *const c_char, x: f32, y: f32) {
    gpui_ohos::pointer_move(&text(id), x, y);
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_scroll(
    id: *const c_char,
    x: f32,
    y: f32,
    dx: f32,
    dy: f32,
    phase: i32,
) {
    gpui_ohos::scroll(&text(id), x, y, dx, dy, phase);
}

/// Install the host operation callbacks.
///
/// # Safety
///
/// The pointer must reference a HostOps that outlives the process.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_gpui_set_host_ops(ops: *const gpui_ohos::HostOps) {
    if ops.is_null() {
        return;
    }
    // SAFETY: the caller guarantees the HostOps outlives the process.
    gpui_ohos::set_host_ops(unsafe { *ops });
}

#[unsafe(no_mangle)]
pub extern "C" fn ohos_gpui_host_event(kind: i32, arg: *const c_char) {
    gpui_ohos::host_event(kind, &text(arg));
}
