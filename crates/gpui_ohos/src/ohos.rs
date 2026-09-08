mod atlas;
mod dispatcher;
mod display;
mod keyboard;
mod platform;
mod text_system;
mod vk;
mod window;

use std::{cell::RefCell, ffi::c_void, rc::{Rc, Weak}};

pub use vk::LogFn;

/// Log a line through the host-provided hilog sink.
pub fn log_line(message: &str) {
    vk::log(message);
}

use crate::{App, Application, MouseButton, Platform};

use platform::OhosPlatform;

thread_local! {
    /// The live platform for this (UI) thread. XComponent callbacks, ArkTS
    /// frame ticks and input all arrive on the ArkUI UI thread.
    static CURRENT: RefCell<Weak<OhosPlatform>> = RefCell::new(Weak::new());
}

fn set_current(platform: &Rc<OhosPlatform>) {
    CURRENT.with(|current| *current.borrow_mut() = Rc::downgrade(platform));
}

fn with_current<R>(f: impl FnOnce(&Rc<OhosPlatform>) -> R) -> Option<R> {
    CURRENT.with(|current| current.borrow().upgrade().map(|platform| f(&platform)))
}

fn new_platform() -> Rc<OhosPlatform> {
    let platform = Rc::new(
        OhosPlatform::new()
            .unwrap_or_else(|error| panic!("Failed to initialize OHOS platform: {error}")),
    );
    set_current(&platform);
    platform
}

/// Entry point used by gpui_platform and gpui_ohos_demo.
pub fn current_platform(_headless: bool) -> Rc<dyn Platform> {
    new_platform()
}

/// Start a GPUI application on an XComponent surface and render the first frame.
///
/// Called by the C++ host from OnSurfaceCreated. GPUI's Platform::run only
/// records the launch callback, so we invoke it here once the surface exists;
/// the ArkTS frame loop then drives subsequent frames through tick().
pub fn run_with_surface<F>(
    window: *mut c_void,
    width: u32,
    height: u32,
    log: LogFn,
    app: F,
) -> i32
where
    F: FnOnce(&mut App) + 'static,
{
    vk::set_logger(log);
    vk::log(&format!(
        "[gpui_ohos] run_with_surface window={:p} {}x{}",
        window, width, height
    ));
    let platform = new_platform();
    platform.set_surface(window, width, height);
    {
        let text_system = platform.text_system();
        let names = text_system.all_font_names();
        vk::log(&format!("[gpui_ohos] text_system fonts={}", names.len()));
        if let Some(sample) = names.iter().find(|name| name.contains("HarmonyOS")) {
            vk::log(&format!("[gpui_ohos] text_system sample={sample}"));
        }
    }
    Application::with_platform(platform.clone() as Rc<dyn Platform>).run(app);
    platform.launch();
    platform.request_frames();
    vk::log(&format!(
        "[gpui_ohos] launched, windows={}",
        platform.window_count()
    ));
    0
}

/// XComponent surface changed size (rotation / resize).
pub fn surface_resized(width: u32, height: u32) {
    vk::log(&format!("[gpui_ohos] surface_resized {}x{}", width, height));
    with_current(|platform| platform.surface_resized(width, height));
}

/// XComponent surface destroyed.
pub fn surface_destroyed() {
    vk::log("[gpui_ohos] surface_destroyed");
    with_current(|platform| platform.surface_destroyed());
}

/// One frame tick, driven by the ArkTS host (DisplaySync or setInterval).
pub fn tick() {
    with_current(|platform| platform.tick());
}

fn map_button(button: u32) -> MouseButton {
    match button {
        2 => MouseButton::Right,
        4 => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

pub fn pointer_down(x: f32, y: f32, button: u32) {
    with_current(|platform| {
        for window in platform.windows() {
            window.pointer_down(map_button(button), crate::point(crate::px(x), crate::px(y)));
        }
    });
}

pub fn pointer_up(x: f32, y: f32, button: u32) {
    with_current(|platform| {
        for window in platform.windows() {
            window.pointer_up(map_button(button), crate::point(crate::px(x), crate::px(y)));
        }
    });
}

pub fn pointer_move(x: f32, y: f32) {
    with_current(|platform| {
        for window in platform.windows() {
            window.pointer_move(crate::point(crate::px(x), crate::px(y)));
        }
    });
}

pub fn scroll(x: f32, y: f32, delta_x: f32, delta_y: f32, phase: i32) {
    let phase = match phase {
        0 => crate::TouchPhase::Started,
        2 => crate::TouchPhase::Ended,
        _ => crate::TouchPhase::Moved,
    };
    with_current(|platform| {
        for window in platform.windows() {
            window.scroll(
                crate::point(crate::px(x), crate::px(y)),
                crate::point(crate::px(delta_x), crate::px(delta_y)),
                phase,
            );
        }
    });
}
