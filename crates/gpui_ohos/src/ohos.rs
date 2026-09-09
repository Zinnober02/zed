mod atlas;
mod clipboard;
mod dispatcher;
mod display;
mod host;
mod inputmethod;
mod keyboard;
mod platform;
mod text_system;
mod vk;
mod window;

use std::{cell::RefCell, ffi::c_void, rc::{Rc, Weak}};

pub use vk::LogFn;

pub use host::HostOps;

pub use inputmethod::{
    commit_text as ime_commit_text, delete_backward as ime_delete_backward,
    delete_forward as ime_delete_forward, finish_preview as ime_finish_preview,
    hide as ime_hide, preview_text as ime_preview_text, show as ime_show,
};

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
    /// Writable directory the app should browse, set from ArkTS filesDir.
    static ROOT_DIR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Set the directory the application should browse (from ArkTS context.filesDir).
pub fn set_root_dir(path: &str) {
    ROOT_DIR.with(|root| *root.borrow_mut() = Some(path.to_string()));
}

/// The directory set by set_root_dir, if any.
pub fn root_dir() -> Option<String> {
    ROOT_DIR.with(|root| root.borrow().clone())
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
///
/// Reuses the platform the host already attached a surface to, so an
/// application that builds its own `Application` (Zed) ends up on the same
/// platform as the entry point that received the surface.
pub fn current_platform(_headless: bool) -> Rc<dyn Platform> {
    with_current(|platform| platform.clone() as Rc<dyn Platform>)
        .unwrap_or_else(|| new_platform() as Rc<dyn Platform>)
}

/// Attach a surface, let `run_app` build and register the application on the
/// shared platform, then launch it and start the frame loop.
pub fn run_app_on_surface<F>(
    id: &str,
    window: *mut c_void,
    width: u32,
    height: u32,
    log: LogFn,
    run_app: F,
) -> i32
where
    F: FnOnce(),
{
    vk::set_logger(log);
    vk::log(&format!(
        "[gpui_ohos] run_app_on_surface id={id} window={:p} {}x{}",
        window, width, height
    ));
    let platform = new_platform();
    platform.set_surface(id, window, width, height);
    run_app();
    platform.launch();
    inputmethod::attach();
    platform.request_frames();
    vk::log(&format!(
        "[gpui_ohos] launched, windows={}",
        platform.window_count()
    ));
    0
}

/// Start a GPUI application on an XComponent surface and render the first frame.
///
/// Called by the C++ host from OnSurfaceCreated. GPUI's Platform::run only
/// records the launch callback, so we invoke it here once the surface exists;
/// the ArkTS frame loop then drives subsequent frames through tick().
pub fn run_with_surface<F>(
    id: &str,
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
        "[gpui_ohos] run_with_surface id={id} window={:p} {}x{}",
        window, width, height
    ));
    let platform = new_platform();
    platform.set_surface(id, window, width, height);
    {
        let text_system = platform.text_system();
        let names = text_system.all_font_names();
        vk::log(&format!("[gpui_ohos] text_system fonts={}", names.len()));
        if let Some(sample) = names.iter().find(|name| name.contains("HarmonyOS")) {
            vk::log(&format!("[gpui_ohos] text_system sample={sample}"));
        }
    }
    {
        // Clipboard self-test: write needs no permission, read needs the
        // READ_PASTEBOARD ACL permission.
        let marker = "gpui_ohos clipboard test";
        let wrote = clipboard::write_text(marker);
        let read = clipboard::read_text();
        vk::log(&format!(
            "[gpui_ohos] clipboard write={wrote} read={read:?}"
        ));
    }
    Application::with_platform(platform.clone() as Rc<dyn Platform>).run(app);
    platform.launch();
    inputmethod::attach();
    platform.request_frames();
    vk::log(&format!(
        "[gpui_ohos] launched, windows={}",
        platform.window_count()
    ));
    0
}

/// Read a file the user picked, through the descriptor the host opened.
pub fn read_picked_file(fd: i32) -> Result<String, String> {
    host::read_fd(fd).map_err(|error| error.to_string())
}

/// Overwrite a file the user picked, through its descriptor.
pub fn write_picked_file(fd: i32, data: &str) -> Result<(), String> {
    host::write_fd(fd, data).map_err(|error| error.to_string())
}

/// Open a picked file read/write and return its descriptor.
pub fn open_picked_file(uri: &str) -> Option<i32> {
    host::open_file(uri)
}

/// List a picked directory as (name, is_directory, uri).
pub fn list_picked_dir(uri: &str) -> Vec<(String, bool, String)> {
    host::list_dir(uri)
}

/// Logical pixels per device pixel used by the backend renderer.
pub fn scale() -> f32 {
    window::SCALE
}

/// Read the system clipboard. Needs ohos.permission.READ_PASTEBOARD.
pub fn read_clipboard() -> Option<String> {
    clipboard::read_text()
}

/// Write plain text to the system clipboard. No permission required.
pub fn write_clipboard(text: &str) -> bool {
    clipboard::write_text(text)
}

/// Take the folder URI the host restored from persisted authorization.
pub fn take_restore_folder() -> Option<String> {
    with_current(|platform| platform.take_restore_folder()).flatten()
}

/// Pick files/folders and return the host's raw answer lines
/// ("f|fd|name" for files, "d|uri" for folders).
pub fn pick_items(
    files: bool,
    directories: bool,
    multiple: bool,
) -> futures::channel::oneshot::Receiver<Vec<String>> {
    with_current(|platform| platform.pick_raw(files, directories, multiple)).unwrap_or_else(|| {
        let (sender, receiver) = futures::channel::oneshot::channel();
        let _ = sender.send(Vec::new());
        receiver
    })
}

/// Install the host function pointers supplied by the ArkTS layer.
pub fn set_host_ops(ops: HostOps) {
    host::set_ops(ops);
}

/// Handle an event pushed from the ArkTS host.
pub fn host_event(kind: i32, arg: &str) {
    with_current(|platform| platform.handle_host_event(kind, arg));
}

/// An additional XComponent surface was created for another window.
pub fn surface_created(id: &str, window: *mut c_void, width: u32, height: u32) {
    vk::log(&format!(
        "[gpui_ohos] surface_created id={id} window={:p} {}x{}",
        window, width, height
    ));
    with_current(|platform| {
        platform.add_surface(id, window, width, height);
        platform.request_frames();
    });
}

/// XComponent surface changed size (rotation / resize).
pub fn surface_resized(id: &str, width: u32, height: u32) {
    vk::log(&format!(
        "[gpui_ohos] surface_resized id={id} {}x{}",
        width, height
    ));
    with_current(|platform| platform.surface_resized(id, width, height));
}

/// XComponent surface destroyed.
pub fn surface_destroyed(id: &str) {
    vk::log(&format!("[gpui_ohos] surface_destroyed id={id}"));
    with_current(|platform| platform.surface_destroyed(id));
}

/// One frame tick, driven by the ArkTS host (DisplaySync or setInterval).
pub fn tick() {
    with_current(|platform| {
        for command in inputmethod::drain() {
            platform.handle_ime(command);
        }
        if let Some((text, caret, cursor)) = platform.ime_context() {
            inputmethod::update_context(&text, caret, cursor);
        }
        platform.tick();
    });
}

fn map_button(button: u32) -> MouseButton {
    match button {
        2 => MouseButton::Right,
        4 => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

// The host converts ArkUI's vp coordinates into GPUI logical pixels before
// calling in, so this is a pass-through.
fn logical(value: f32) -> crate::Pixels {
    crate::px(value)
}

pub fn pointer_down(id: &str, x: f32, y: f32, button: u32) {
    // Clicking the surface must give the XComponent ArkUI focus, otherwise its
    // onKeyEvent never fires and non-text keys (arrows) are dropped.
    host::window_op(host::op::REQUEST_FOCUS, "");
    with_current(|platform| {
        for window in platform.route_targets(id) {
            window.pointer_down(map_button(button), crate::point(logical(x), logical(y)));
        }
    });
}

pub fn pointer_up(id: &str, x: f32, y: f32, button: u32) {
    with_current(|platform| {
        for window in platform.route_targets(id) {
            window.pointer_up(map_button(button), crate::point(logical(x), logical(y)));
        }
    });
}

pub fn pointer_move(id: &str, x: f32, y: f32) {
    with_current(|platform| {
        for window in platform.route_targets(id) {
            window.pointer_move(crate::point(logical(x), logical(y)));
        }
    });
}

/// ArkUI axis events (mouse wheel / touchpad) are already in logical pixels.
pub fn scroll(id: &str, x: f32, y: f32, delta_x: f32, delta_y: f32, phase: i32) {
    let phase = match phase {
        0 => crate::TouchPhase::Started,
        2 => crate::TouchPhase::Ended,
        _ => crate::TouchPhase::Moved,
    };
    with_current(|platform| {
        for window in platform.route_targets(id) {
            window.scroll(
                crate::point(crate::px(x), crate::px(y)),
                crate::point(crate::px(delta_x), crate::px(delta_y)),
                phase,
            );
        }
    });
}

thread_local! {
    /// Keyboard modifier state, tracked from NDK key events.
    static MODIFIERS: std::cell::Cell<crate::Modifiers> =
        std::cell::Cell::new(crate::Modifiers::default());
}

fn is_modifier_key(code: i32) -> bool {
    matches!(code, 2072 | 2073 | 2047 | 2048 | 2045 | 2046 | 2076 | 2077)
}

fn update_modifiers(code: i32, down: bool) -> crate::Modifiers {
    MODIFIERS.with(|cell| {
        let mut modifiers = cell.get();
        match code {
            2072 | 2073 => modifiers.control = down,
            2047 | 2048 => modifiers.shift = down,
            2045 | 2046 => modifiers.alt = down,
            2076 | 2077 => modifiers.platform = down,
            _ => {}
        }
        cell.set(modifiers);
        modifiers
    })
}

/// Keys GPUI must see before the input method: modifiers, navigation and any
/// shortcut combination. Everything else is text and goes to the input method.
fn is_gpui_key(code: i32, modifiers: crate::Modifiers) -> bool {
    is_modifier_key(code)
        || modifiers.control
        || modifiers.alt
        || modifiers.platform
        || matches!(
            code,
            2012 | 2013 | 2014 | 2015 // arrows
                | 2049 // tab
                | 2054 // enter
                | 2055 // backspace
                | 2068 | 2069 // page up / down
                | 2070 // escape
                | 2071 // delete
                | 2081 | 2082 // home / end
                | 2083 // insert
                | 2090..=2101 // function keys
        )
}

/// Pre-IME key event. Returns true to consume it so the input method never
/// sees it.
pub fn key_pre_ime(id: &str, action: i32, code: i32, _unicode: i32) -> bool {
    let down = action == 0;
    let modifiers = update_modifiers(code, down);
    if is_modifier_key(code) {
        with_current(|platform| platform.dispatch_modifiers(id, modifiers));
        return true;
    }
    if !is_gpui_key(code, modifiers) {
        return false;
    }
    dispatch_key(id, down, code, modifiers, None);
    true
}

/// Post-IME key event (action: 0 = down, 1 = up). The NDK reports the Unicode
/// value for printable keys.
pub fn key_event(id: &str, action: i32, code: i32, unicode: i32) {
    let down = action == 0;
    let modifiers = update_modifiers(code, down);
    if is_modifier_key(code) {
        with_current(|platform| platform.dispatch_modifiers(id, modifiers));
        return;
    }
    let unicode = u32::try_from(unicode).ok().filter(|value| *value > 0);
    dispatch_key(id, down, code, modifiers, unicode);
}

fn dispatch_key(
    id: &str,
    down: bool,
    code: i32,
    modifiers: crate::Modifiers,
    unicode: Option<u32>,
) {
    let Some((key, character)) = ohos_key(code) else {
        vk::log(&format!("[gpui_ohos] unmapped key code={code}"));
        return;
    };
    let key_char = if modifiers.control || modifiers.platform {
        None
    } else {
        unicode
            .and_then(char::from_u32)
            .or(character)
            .map(|ch| {
                if modifiers.shift {
                    ch.to_uppercase().to_string()
                } else {
                    ch.to_string()
                }
            })
    };
    let keystroke = crate::Keystroke {
        modifiers,
        key,
        key_char,
    };
    vk::log(&format!(
        "[gpui_ohos] key {} {}",
        if down { "down" } else { "up" },
        keystroke
    ));
    with_current(|platform| platform.dispatch_key(id, down, keystroke));
}

/// Map an OHOS key code to a GPUI key name and the character it types.
fn ohos_key(code: i32) -> Option<(String, Option<char>)> {
    Some(match code {
        2000..=2009 => {
            let ch = (b'0' + (code - 2000) as u8) as char;
            (ch.to_string(), Some(ch))
        }
        2017..=2042 => {
            let ch = (b'a' + (code - 2017) as u8) as char;
            (ch.to_string(), Some(ch))
        }
        2012 => ("up".to_string(), None),
        2013 => ("down".to_string(), None),
        2014 => ("left".to_string(), None),
        2015 => ("right".to_string(), None),
        2043 => (",".to_string(), Some(',')),
        2044 => (".".to_string(), Some('.')),
        2049 => ("tab".to_string(), None),
        2050 => ("space".to_string(), Some(' ')),
        2054 => ("enter".to_string(), None),
        2055 => ("backspace".to_string(), None),
        2056 => ("\u{60}".to_string(), Some('\u{60}')),
        2057 => ("-".to_string(), Some('-')),
        2058 => ("=".to_string(), Some('=')),
        2059 => ("[".to_string(), Some('[')),
        2060 => ("]".to_string(), Some(']')),
        2061 => ("\\".to_string(), Some('\\')),
        2062 => (";".to_string(), Some(';')),
        2063 => ("'".to_string(), Some('\'')),
        2064 => ("/".to_string(), Some('/')),
        2065 => ("@".to_string(), Some('@')),
        2066 => ("+".to_string(), Some('+')),
        2068 => ("pageup".to_string(), None),
        2069 => ("pagedown".to_string(), None),
        2070 => ("escape".to_string(), None),
        2071 => ("delete".to_string(), None),
        2081 => ("home".to_string(), None),
        2082 => ("end".to_string(), None),
        2083 => ("insert".to_string(), None),
        2090..=2101 => (format!("f{}", code - 2089), None),
        _ => return None,
    })
}
