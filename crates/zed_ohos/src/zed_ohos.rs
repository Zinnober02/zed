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
    // Zed resolves its data, config and log directories from HOME, which the
    // sandbox does not define; without this every directory creation is
    // denied and the app stops at its launch-failure dialog.
    // SAFETY: called before any other thread exists.
    unsafe {
        std::env::set_var("HOME", "/data/storage/el2/base/haps/entry/files");
    }

    install_filesystem_bridge();

    gpui_ohos::run_app_on_surface(&id, window, width, height, log, zed_app::run)
}

/// Let the filesystem wrapper reach picked documents through the host.
///
/// The picker hands back `file://docs` URIs and descriptors instead of paths the
/// sandbox can open, so `fs::OhosFs` forwards tagged paths to these entry
/// points and leaves every other path to the real filesystem.
fn install_filesystem_bridge() {
    fs::set_ohos_fs_bridge(fs::OhosFsBridge {
        list_dir: gpui_ohos::list_picked_dir,
        open_file: gpui_ohos::open_picked_file,
        read_fd: |fd| {
            gpui_ohos::read_picked_file_bytes(fd)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))
        },
        write_fd: |fd, data| {
            gpui_ohos::write_picked_file_bytes(fd, data)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))
        },
    });
}
