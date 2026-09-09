use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::c_void,
    path::PathBuf,
    rc::{Rc, Weak},
    sync::Arc,
};

use anyhow::Result;
use futures::channel::oneshot;

use crate::{
    Action, AnyWindowHandle, BackgroundExecutor, ClipboardItem, CursorStyle, ForegroundExecutor,
    Keymap, Menu, MenuItem, Modifiers, OwnedMenu, PathPromptOptions, Platform, PlatformDisplay,
    PlatformInput, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem,
    PlatformWindow, PriorityQueueReceiver, Result as GpuiResult, RunnableVariant, Task,
    ThermalState, WindowAppearance, WindowParams,
};

use super::dispatcher::OhosDispatcher;
use super::display::OhosDisplay;
use super::host;
use super::keyboard::{OhosKeyboardLayout, OhosKeyboardMapper};
use super::text_system::OhosTextSystem;
use super::window::{OhosWindow, WindowShared};

/// State of the XComponent surface backing the GPUI window.
pub(crate) struct SurfaceState {
    /// OHNativeWindow pointer handed to us by OnSurfaceCreated.
    pub window: *mut c_void,
    pub width: u32,
    pub height: u32,
    pub valid: bool,
}

impl Default for SurfaceState {
    fn default() -> Self {
        Self {
            window: std::ptr::null_mut(),
            width: 0,
            height: 0,
            valid: false,
        }
    }
}

pub(crate) struct OhosPlatform {
    dispatcher: Arc<OhosDispatcher>,
    background_executor: BackgroundExecutor,
    foreground_executor: ForegroundExecutor,
    text_system: Arc<dyn PlatformTextSystem>,
    main_receiver: PriorityQueueReceiver<RunnableVariant>,
    /// Every XComponent surface, keyed by its XComponent id; index 0 is the
    /// primary surface that launched the application.
    surfaces: Rc<RefCell<Vec<(String, Rc<RefCell<SurfaceState>>)>>>,
    /// How many GPUI windows have been bound to a surface.
    windows_opened: Cell<usize>,
    pending_launch: RefCell<Option<Box<dyn 'static + FnOnce()>>>,
    windows: Rc<RefCell<Vec<Rc<WindowShared>>>>,
    /// GPUI window handles paired with their platform window, in open order.
    handles: RefCell<Vec<(AnyWindowHandle, Weak<WindowShared>)>>,
    menus: RefCell<Vec<OwnedMenu>>,
    app_menu_action: RefCell<Option<Box<dyn FnMut(&dyn Action)>>>,
    app_menu_will_open: RefCell<Option<Box<dyn FnMut()>>>,
    app_menu_validate: RefCell<Option<Box<dyn FnMut(&dyn Action) -> bool>>>,
    open_urls: RefCell<Option<Box<dyn FnMut(Vec<String>)>>>,
    on_quit: RefCell<Option<Box<dyn FnMut() -> bool>>>,
    on_reopen: RefCell<Option<Box<dyn FnMut()>>>,
    on_system_wake: RefCell<Option<Box<dyn FnMut()>>>,
    appearance: Cell<WindowAppearance>,
    /// Raw host picker answers: "f|fd|name" for files, "d|uri" for folders.
    pending_picks: RefCell<HashMap<u64, oneshot::Sender<Vec<String>>>>,
    next_pick_id: Cell<u64>,
    /// Folder URI the host restored from its persisted authorization.
    restore_folder: RefCell<Option<String>>,
    /// Window rect in physical pixels: (x, y, width, height).
    window_rect: Cell<(f32, f32, f32, f32)>,
}

impl OhosPlatform {
    pub(crate) fn new() -> Result<Self> {
        let (main_sender, main_receiver) = PriorityQueueReceiver::new();
        let dispatcher = Arc::new(OhosDispatcher::new(main_sender));
        let background_executor = BackgroundExecutor::new(dispatcher.clone());
        let foreground_executor = ForegroundExecutor::new(dispatcher.clone());
        let appearance = match host::query(host::query::COLOR_MODE, "").as_deref() {
            Some("0") => WindowAppearance::Dark,
            _ => WindowAppearance::Light,
        };
        let window_rect = host::query(host::query::WINDOW_RECT, "")
            .and_then(|value| parse_rect(&value))
            .unwrap_or((0.0, 0.0, 0.0, 0.0));
        Ok(Self {
            dispatcher,
            background_executor,
            foreground_executor,
            text_system: Arc::new(OhosTextSystem::new()),
            main_receiver,
            surfaces: Rc::new(RefCell::new(Vec::new())),
            windows_opened: Cell::new(0),
            pending_launch: RefCell::new(None),
            windows: Rc::new(RefCell::new(Vec::new())),
            handles: RefCell::new(Vec::new()),
            menus: RefCell::new(Vec::new()),
            app_menu_action: RefCell::new(None),
            app_menu_will_open: RefCell::new(None),
            app_menu_validate: RefCell::new(None),
            open_urls: RefCell::new(None),
            on_quit: RefCell::new(None),
            on_reopen: RefCell::new(None),
            on_system_wake: RefCell::new(None),
            appearance: Cell::new(appearance),
            pending_picks: RefCell::new(HashMap::new()),
            next_pick_id: Cell::new(1),
            restore_folder: RefCell::new(None),
            window_rect: Cell::new(window_rect),
        })
    }

    /// Host-pushed events: focus, window status, color mode, lifecycle, picker
    /// results. Everything arrives on the ArkUI UI thread.
    pub(crate) fn handle_host_event(&self, kind: i32, arg: &str) {
        super::vk::log(&format!("[gpui_ohos] host event {kind}: {arg}"));
        match kind {
            host::event::APPEARANCE => {
                let appearance = if arg == "0" {
                    WindowAppearance::Dark
                } else {
                    WindowAppearance::Light
                };
                if self.appearance.get() != appearance {
                    self.appearance.set(appearance);
                    for window in self.windows() {
                        window.set_appearance(appearance);
                    }
                }
            }
            host::event::FOCUS => {
                let active = arg != "0";
                for window in self.windows() {
                    window.set_active(active);
                }
            }
            host::event::WINDOW_STATUS => {
                // 1 full screen, 2 maximize, 3 minimize, 4 floating, 5 split.
                // On 2in1 the system reports MAXIMIZE for a screen-filling
                // window, which is what GPUI's titlebar treats as fullscreen.
                let fullscreen = arg == "1" || arg == "2";
                for window in self.windows() {
                    window.set_fullscreen(fullscreen);
                }
            }
            host::event::WINDOW_RECT => {
                if let Some(rect) = parse_rect(arg) {
                    self.window_rect.set(rect);
                }
            }
            host::event::LIFECYCLE => match arg {
                "background" | "destroy" => {
                    if let Some(mut callback) = self.on_quit.borrow_mut().take() {
                        let _ = callback();
                        *self.on_quit.borrow_mut() = Some(callback);
                    }
                }
                "newwant" => {
                    if let Some(mut callback) = self.on_reopen.borrow_mut().take() {
                        callback();
                        *self.on_reopen.borrow_mut() = Some(callback);
                    }
                }
                "foreground" => {
                    if let Some(mut callback) = self.on_system_wake.borrow_mut().take() {
                        callback();
                        *self.on_system_wake.borrow_mut() = Some(callback);
                    }
                }
                _ => {}
            },
            host::event::RESTORE_FOLDER => {
                if !arg.is_empty() {
                    *self.restore_folder.borrow_mut() = Some(arg.to_string());
                }
            }
            host::event::PICK_RESULT => {
                let (token, rest) = arg.split_once('\t').unwrap_or((arg, ""));
                let id: u64 = token[1..].parse().unwrap_or(0);
                if let Some(sender) = self.pending_picks.borrow_mut().remove(&id) {
                    let lines: Vec<String> = rest
                        .lines()
                        .filter(|line| !line.is_empty())
                        .map(str::to_string)
                        .collect();
                    let _ = sender.send(lines);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn take_restore_folder(&self) -> Option<String> {
        self.restore_folder.borrow_mut().take()
    }

    /// Pick files/folders, returning the host's raw answer lines
    /// ("f|fd|name" for files, "d|uri" for folders).
    pub(crate) fn pick_raw(
        &self,
        files: bool,
        directories: bool,
        multiple: bool,
    ) -> oneshot::Receiver<Vec<String>> {
        let (sender, receiver) = oneshot::channel();
        let id = self.next_pick_id.get();
        self.next_pick_id.set(id + 1);
        self.pending_picks.borrow_mut().insert(id, sender);
        let payload = format!(
            "p{id}|{}|{}|{}",
            files as u8, directories as u8, multiple as u8
        );
        host::window_op(host::op::PICK_PATHS, &payload);
        receiver
    }

    /// Register the primary surface (the one that launched the application).
    pub(crate) fn set_surface(&self, id: &str, window: *mut c_void, width: u32, height: u32) {
        let mut surfaces = self.surfaces.borrow_mut();
        // Reuse the existing entry so any window already holding this Rc sees
        // the update; otherwise insert the primary at the front.
        if let Some((_, surface)) = surfaces.iter().find(|(existing, _)| existing == id) {
            let mut state = surface.borrow_mut();
            state.window = window;
            state.width = width;
            state.height = height;
            state.valid = true;
            return;
        }
        surfaces.insert(
            0,
            (
                id.to_string(),
                Rc::new(RefCell::new(SurfaceState {
                    window,
                    width,
                    height,
                    valid: true,
                })),
            ),
        );
    }

    /// Register an additional XComponent surface created for another window.
    ///
    /// The window was opened with a placeholder for this id, so update that
    /// entry in place rather than replacing it.
    pub(crate) fn add_surface(&self, id: &str, window: *mut c_void, width: u32, height: u32) {
        let surface = {
            let mut surfaces = self.surfaces.borrow_mut();
            if let Some((_, surface)) = surfaces.iter().find(|(existing, _)| existing == id) {
                surface.clone()
            } else {
                let surface = Rc::new(RefCell::new(SurfaceState::default()));
                surfaces.push((id.to_string(), surface.clone()));
                surface
            }
        };
        {
            let mut state = surface.borrow_mut();
            state.window = window;
            state.width = width;
            state.height = height;
            state.valid = true;
        }
        for window in self.windows.borrow().iter() {
            if window.shares_surface(&surface) {
                window.on_surface_resized(width, height);
            }
        }
    }

    /// The surface a new window should use: an existing one, or a fresh
    /// placeholder plus a host request to create the XComponent.
    fn surface_for_window(&self) -> Rc<RefCell<SurfaceState>> {
        let index = self.windows_opened.get();
        self.windows_opened.set(index + 1);
        let mut surfaces = self.surfaces.borrow_mut();
        let (width, height) = surfaces
            .first()
            .map(|(_, surface)| {
                let state = surface.borrow();
                (state.width, state.height)
            })
            .unwrap_or((2200, 1430));
        while surfaces.len() <= index {
            let id = format!("gpui_surface_{}", surfaces.len());
            surfaces.push((
                id.clone(),
                Rc::new(RefCell::new(SurfaceState {
                    window: std::ptr::null_mut(),
                    width,
                    height,
                    valid: false,
                })),
            ));
            drop(surfaces);
            host::window_op(host::op::CREATE_WINDOW, &id);
            surfaces = self.surfaces.borrow_mut();
        }
        surfaces[index].1.clone()
    }

    pub(crate) fn surface_resized(&self, id: &str, width: u32, height: u32) {
        let surface = {
            let surfaces = self.surfaces.borrow();
            surfaces
                .iter()
                .find(|(existing, _)| existing == id)
                .map(|(_, surface)| surface.clone())
        };
        let Some(surface) = surface else {
            return;
        };
        {
            let mut state = surface.borrow_mut();
            state.width = width;
            state.height = height;
            state.valid = true;
        }
        for window in self.windows.borrow().iter() {
            if window.shares_surface(&surface) {
                window.on_surface_resized(width, height);
            }
        }
        self.request_frames();
    }

    pub(crate) fn surface_destroyed(&self, id: &str) {
        let surface = {
            let surfaces = self.surfaces.borrow();
            surfaces
                .iter()
                .find(|(existing, _)| existing == id)
                .map(|(_, surface)| surface.clone())
        };
        let Some(surface) = surface else {
            return;
        };
        {
            let mut state = surface.borrow_mut();
            state.valid = false;
            state.window = std::ptr::null_mut();
        }
        // The host closed that window: stop ticking it and drop the platform's
        // reference so it is recycled.
        let mut windows = self.windows.borrow_mut();
        for window in windows.iter() {
            if window.shares_surface(&surface) {
                window.mark_closed();
            }
        }
        windows.retain(|window| !window.is_closed());
    }

    /// Invoke the GPUI launch callback recorded by Platform::run.
    pub(crate) fn launch(&self) {
        let callback = self.pending_launch.borrow_mut().take();
        if let Some(callback) = callback {
            callback();
        }
    }

    /// One host frame: drain foreground tasks, fire due timers, then draw.
    pub(crate) fn tick(&self) {
        let mut receiver = self.main_receiver.clone();
        while let Ok(Some(runnable)) = receiver.try_pop() {
            runnable.run();
        }
        self.dispatcher.run_due_timers();
        self.request_frames();
    }

    pub(crate) fn request_frames(&self) {
        for window in self.windows.borrow().iter() {
            if !window.is_closed() {
                window.request_frame();
            }
        }
    }

    pub(crate) fn window_count(&self) -> usize {
        self.windows
            .borrow()
            .iter()
            .filter(|window| !window.is_closed())
            .count()
    }

    /// Windows an input event from a surface should reach. An empty id keeps
    /// the legacy broadcast to every window.
    pub(crate) fn route_targets(&self, id: &str) -> Vec<Rc<WindowShared>> {
        if id.is_empty() {
            return self.windows();
        }
        let surface = {
            let surfaces = self.surfaces.borrow();
            surfaces
                .iter()
                .find(|(existing, _)| existing == id)
                .map(|(_, surface)| surface.clone())
        };
        let Some(surface) = surface else {
            return Vec::new();
        };
        self.windows
            .borrow()
            .iter()
            .filter(|window| !window.is_closed() && window.shares_surface(&surface))
            .cloned()
            .collect()
    }

    /// Snapshot of live window state, for input routing and frame ticks.
    pub(crate) fn windows(&self) -> Vec<Rc<WindowShared>> {
        self.windows
            .borrow()
            .iter()
            .filter(|window| !window.is_closed())
            .cloned()
            .collect()
    }

    /// Text, caret and absolute caret rect (physical pixels) of the focused
    /// editor, for IME context notifications.
    pub(crate) fn ime_context(&self) -> Option<(String, usize, (f64, f64, f64, f64))> {
        let (rect_x, rect_y, _, _) = self.window_rect.get();
        let scale = super::window::SCALE as f64;
        for window in self.windows() {
            if let Some((text, caret, cursor)) = window.ime_context() {
                let left = rect_x as f64 + cursor.origin.x.as_f32() as f64 * scale;
                let top = rect_y as f64 + cursor.origin.y.as_f32() as f64 * scale;
                let width = cursor.size.width.as_f32() as f64 * scale;
                let height = cursor.size.height.as_f32() as f64 * scale;
                return Some((text, caret, (left, top, width, height)));
            }
        }
        None
    }

    /// Forward an IME command to every window's input handler.
    pub(crate) fn handle_ime(&self, command: super::inputmethod::ImeCommand) {
        for window in self.windows() {
            window.handle_ime(&command);
        }
    }

    /// Forward a modifier-state change to the surface's window.
    pub(crate) fn dispatch_modifiers(&self, id: &str, modifiers: Modifiers) {
        for window in self.route_targets(id) {
            window.dispatch_input(PlatformInput::ModifiersChanged(
                crate::ModifiersChangedEvent {
                    modifiers,
                    capslock: crate::Capslock::default(),
                },
            ));
        }
    }

    /// Forward a key press or release to the surface's window.
    pub(crate) fn dispatch_key(&self, id: &str, down: bool, keystroke: crate::Keystroke) {
        for window in self.route_targets(id) {
            if down {
                window.dispatch_input(PlatformInput::KeyDown(crate::KeyDownEvent {
                    keystroke: keystroke.clone(),
                    is_held: false,
                    prefer_character_input: false,
                }));
            } else {
                window.dispatch_input(PlatformInput::KeyUp(crate::KeyUpEvent {
                    keystroke: keystroke.clone(),
                }));
            }
        }
    }
}

/// Map a GPUI cursor style to the OHOS PointerStyle enum member name.
fn cursor_style_name(style: CursorStyle) -> &'static str {
    match style {
        CursorStyle::Arrow => "DEFAULT",
        CursorStyle::IBeam => "TEXT_CURSOR",
        CursorStyle::Crosshair => "CROSS",
        CursorStyle::ClosedHand => "HAND_GRABBING",
        CursorStyle::OpenHand => "HAND_OPEN",
        CursorStyle::PointingHand => "HAND_POINTING",
        CursorStyle::ResizeLeft => "WEST",
        CursorStyle::ResizeRight => "EAST",
        CursorStyle::ResizeLeftRight => "WEST_EAST",
        CursorStyle::ResizeUp => "NORTH",
        CursorStyle::ResizeDown => "SOUTH",
        CursorStyle::ResizeUpDown => "NORTH_SOUTH",
        CursorStyle::ResizeUpLeftDownRight => "NORTH_WEST_SOUTH_EAST",
        CursorStyle::ResizeUpRightDownLeft => "NORTH_EAST_SOUTH_WEST",
        CursorStyle::ResizeColumn => "RESIZE_LEFT_RIGHT",
        CursorStyle::ResizeRow => "RESIZE_UP_DOWN",
        CursorStyle::IBeamCursorForVerticalLayout => "HORIZONTAL_TEXT_CURSOR",
        CursorStyle::OperationNotAllowed => "CURSOR_FORBID",
        CursorStyle::DragLink => "MOVE",
        CursorStyle::DragCopy => "CURSOR_COPY",
        CursorStyle::ContextualMenu => "CURSOR_CIRCLE",
    }
}

/// Encode a raw picker answer as the opaque handle GPUI passes around:
/// "f|fd|name" becomes "fd:<fd>", "d|uri" becomes "dir:<uri>".
fn picked_handle(line: &str) -> PathBuf {
    if let Some(rest) = line.strip_prefix("f|") {
        if let Some((fd, _name)) = rest.split_once('|') {
            return PathBuf::from(format!("fd:{fd}"));
        }
    }
    if let Some(uri) = line.strip_prefix("d|") {
        return PathBuf::from(format!("dir:{uri}"));
    }
    PathBuf::from(line)
}

/// Parse a host rect string of the form "x,y,w,h" (physical pixels).
fn parse_rect(value: &str) -> Option<(f32, f32, f32, f32)> {
    let mut parts = value.split(',');
    let x = parts.next()?.trim().parse().ok()?;
    let y = parts.next()?.trim().parse().ok()?;
    let width = parts.next()?.trim().parse().ok()?;
    let height = parts.next()?.trim().parse().ok()?;
    Some((x, y, width, height))
}

impl Platform for OhosPlatform {
    fn background_executor(&self) -> BackgroundExecutor {
        self.background_executor.clone()
    }

    fn foreground_executor(&self) -> ForegroundExecutor {
        self.foreground_executor.clone()
    }

    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.text_system.clone()
    }

    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        // The ArkTS host owns the run loop; we record the callback and invoke it
        // from run_with_surface once the XComponent surface exists.
        *self.pending_launch.borrow_mut() = Some(on_finish_launching);
    }

    fn quit(&self) {}

    fn restart(&self, _binary_path: Option<PathBuf>, _arguments: Vec<std::ffi::OsString>) {}

    fn activate(&self, _ignoring_other_apps: bool) {}

    fn hide_cursor_until_mouse_moves(&self) {}

    fn is_cursor_visible(&self) -> bool {
        true
    }

    fn hide(&self) {}

    fn hide_other_apps(&self) {}

    fn unhide_other_apps(&self) {}

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        let (w, h) = {
            let surfaces = self.surfaces.borrow();
            surfaces
                .first()
                .map(|(_, surface)| {
                    let state = surface.borrow();
                    (state.width, state.height)
                })
                .unwrap_or((3120, 2080))
        };
        vec![Rc::new(OhosDisplay::new(w, h, super::window::SCALE)) as Rc<dyn PlatformDisplay>]
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        self.displays().into_iter().next()
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        let active = self
            .windows()
            .into_iter()
            .find(|window| window.is_active())?;
        self.handles
            .borrow()
            .iter()
            .find(|(_, shared)| {
                shared
                    .upgrade()
                    .map_or(false, |shared| Rc::ptr_eq(&shared, &active))
            })
            .map(|(handle, _)| *handle)
    }

    fn window_stack(&self) -> Option<Vec<AnyWindowHandle>> {
        Some(
            self.handles
                .borrow()
                .iter()
                .map(|(handle, _)| *handle)
                .collect(),
        )
    }

    fn is_screen_capture_supported(&self) -> bool {
        false
    }

    fn screen_capture_sources(
        &self,
    ) -> oneshot::Receiver<GpuiResult<Vec<Rc<dyn crate::ScreenCaptureSource>>>> {
        let (tx, rx) = oneshot::channel();
        tx.send(Err(anyhow::anyhow!("Screen capture not supported on OHOS")))
            .ok();
        rx
    }

    fn open_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        // The first window uses the primary surface; later windows get a fresh
        // XComponent, created by the host on demand. Its surface may still be
        // invalid here and is filled in when the host reports it.
        let surface = self.surface_for_window();
        if let Some(title) = options
            .titlebar
            .as_ref()
            .and_then(|titlebar| titlebar.title.as_ref())
        {
            host::window_op(host::op::SET_TITLE, title);
        }
        let shared = WindowShared::new(surface, options, self.foreground_executor.clone());
        self.windows.borrow_mut().push(shared.clone());
        self.handles
            .borrow_mut()
            .push((handle, Rc::downgrade(&shared)));
        Ok(Box::new(OhosWindow::new(shared)))
    }

    fn window_appearance(&self) -> WindowAppearance {
        self.appearance.get()
    }

    fn open_url(&self, url: &str) {
        host::window_op(host::op::OPEN_URL, url);
    }

    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>) {
        *self.open_urls.borrow_mut() = Some(callback);
    }

    fn register_url_scheme(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!(
            "URL scheme registration not supported on OHOS"
        )))
    }

    fn prompt_for_paths(
        &self,
        options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        let raw = self.pick_raw(options.files, options.directories, options.multiple);
        let (sender, receiver) = oneshot::channel();
        self.foreground_executor
            .spawn(async move {
                let answer = match raw.await {
                    Ok(lines) => {
                        let paths: Vec<PathBuf> =
                            lines.iter().map(|line| picked_handle(line)).collect();
                        if paths.is_empty() {
                            Ok(None)
                        } else {
                            Ok(Some(paths))
                        }
                    }
                    Err(_) => Ok(None),
                };
                let _ = sender.send(answer);
            })
            .detach();
        receiver
    }

    fn prompt_for_new_path(
        &self,
        directory: &std::path::Path,
        suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        let (sender, raw) = oneshot::channel();
        let id = self.next_pick_id.get();
        self.next_pick_id.set(id + 1);
        self.pending_picks.borrow_mut().insert(id, sender);
        let payload = format!(
            "n{id}|{}|{}",
            directory.display(),
            suggested_name.unwrap_or("")
        );
        host::window_op(host::op::PICK_NEW_PATH, &payload);
        let (tx, rx) = oneshot::channel();
        self.foreground_executor
            .spawn(async move {
                let answer = match raw.await {
                    Ok(lines) => Ok(lines.first().map(|line| picked_handle(line))),
                    Err(_) => Ok(None),
                };
                let _ = tx.send(answer);
            })
            .detach();
        rx
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        false
    }

    fn reveal_path(&self, path: &std::path::Path) {
        host::window_op(host::op::REVEAL_PATH, &path.display().to_string());
    }

    fn open_with_system(&self, path: &std::path::Path) {
        host::window_op(host::op::OPEN_WITH, &path.display().to_string());
    }

    fn on_quit(&self, callback: Box<dyn FnMut() -> bool>) {
        *self.on_quit.borrow_mut() = Some(callback);
    }

    fn on_reopen(&self, callback: Box<dyn FnMut()>) {
        *self.on_reopen.borrow_mut() = Some(callback);
    }

    fn on_system_wake(&self, callback: Box<dyn FnMut()>) {
        *self.on_system_wake.borrow_mut() = Some(callback);
    }

    fn set_menus(&self, menus: Vec<Menu>, _keymap: &Keymap) {
        // OHOS has no system menu bar for application windows; keep the menus
        // so the application can render its own (see get_menus).
        *self.menus.borrow_mut() = menus.into_iter().map(Menu::owned).collect();
    }

    fn get_menus(&self) -> Option<Vec<OwnedMenu>> {
        Some(self.menus.borrow().clone())
    }

    fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &Keymap) {}

    fn on_app_menu_action(&self, callback: Box<dyn FnMut(&dyn Action)>) {
        *self.app_menu_action.borrow_mut() = Some(callback);
    }

    fn on_will_open_app_menu(&self, callback: Box<dyn FnMut()>) {
        *self.app_menu_will_open.borrow_mut() = Some(callback);
    }

    fn on_validate_app_menu_command(&self, callback: Box<dyn FnMut(&dyn Action) -> bool>) {
        *self.app_menu_validate.borrow_mut() = Some(callback);
    }

    fn compositor_name(&self) -> &'static str {
        "OHOS"
    }

    fn app_path(&self) -> Result<PathBuf> {
        Err(anyhow::anyhow!("app_path not available on OHOS"))
    }

    fn path_for_auxiliary_executable(&self, _name: &str) -> Result<PathBuf> {
        Err(anyhow::anyhow!(
            "path_for_auxiliary_executable not available on OHOS"
        ))
    }

    fn set_cursor_style(&self, style: CursorStyle) {
        host::window_op(host::op::SET_CURSOR, cursor_style_name(style));
    }

    fn should_auto_hide_scrollbars(&self) -> bool {
        false
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        super::clipboard::read_text().map(ClipboardItem::new_string)
    }

    fn write_to_clipboard(&self, item: ClipboardItem) {
        if let Some(text) = item.text() {
            super::clipboard::write_text(&text);
        }
    }

    fn read_from_primary(&self) -> Option<ClipboardItem> {
        self.read_from_clipboard()
    }

    fn write_to_primary(&self, item: ClipboardItem) {
        self.write_to_clipboard(item)
    }

    fn write_credentials(&self, _url: &str, _username: &str, _password: &[u8]) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!(
            "Credential storage not supported on OHOS"
        )))
    }

    fn read_credentials(&self, _url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        Task::ready(Ok(None))
    }

    fn delete_credentials(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!(
            "Credential deletion not supported on OHOS"
        )))
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(OhosKeyboardLayout)
    }

    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        Rc::new(OhosKeyboardMapper)
    }

    fn on_keyboard_layout_change(&self, _callback: Box<dyn FnMut()>) {}

    fn thermal_state(&self) -> ThermalState {
        ThermalState::Nominal
    }

    fn on_thermal_state_change(&self, _callback: Box<dyn FnMut()>) {}
}
