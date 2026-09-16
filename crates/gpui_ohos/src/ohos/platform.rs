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
    ThermalState, WindowAppearance, WindowKind, WindowParams,
};

use super::dispatcher::OhosDispatcher;
use super::display::OhosDisplay;
use super::host;
use super::keyboard::{OhosKeyboardLayout, OhosKeyboardMapper};
use super::text_system::OhosTextSystem;
use super::window::{OhosWindow, WindowShared, benchmark_enabled};

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

/// Drop a closed window from the list the application saves as its session.
///
/// A handle that is never removed means the list only ever grows, so the session
/// could never shrink and every window the user closed came back the next time
/// the application started.
fn forget_window(handles: &RefCell<Vec<(AnyWindowHandle, Weak<WindowShared>)>>, id: &str) {
    handles
        .borrow_mut()
        .retain(|(_, shared)| shared.upgrade().map_or(false, |shared| shared.id() != id));
    super::vk::log(&format!(
        "[gpui_ohos] window stack now {} after closing {id}",
        handles.borrow().len()
    ));
}

/// Everything the platform knows about its windows, in one table.
///
/// The surface each window owns, the windows themselves and which of them the
/// host last reported as focused used to be three separate fields, so they could
/// disagree without anything saying so.
pub(crate) struct WindowRegistry {
    /// The surface ArkUI currently focuses. Only a different one needs a focus
    /// request; the host reports every change, so this cannot go stale.
    focused_surface: RefCell<Option<String>>,
    /// GPUI window handles paired with their platform window, in open order.
    handles: RefCell<Vec<(AnyWindowHandle, Weak<WindowShared>)>>,
    /// The windows themselves, in open order.
    windows: Rc<RefCell<Vec<Rc<WindowShared>>>>,
    /// Every XComponent surface, keyed by its XComponent id; index 0 is the
    /// primary surface that launched the application.
    surfaces: Rc<RefCell<Vec<(String, Rc<RefCell<SurfaceState>>)>>>,
}

pub(crate) struct OhosPlatform {
    dispatcher: Arc<OhosDispatcher>,
    background_executor: BackgroundExecutor,
    foreground_executor: ForegroundExecutor,
    text_system: Arc<dyn PlatformTextSystem>,
    main_receiver: PriorityQueueReceiver<RunnableVariant>,
    /// How many GPUI windows have been bound to a surface.
    windows_opened: Cell<usize>,
    /// Which way round the theme is, as Zed last reported it.
    theme_appearance: Cell<&'static str>,
    registry: WindowRegistry,
    pending_launch: RefCell<Option<Box<dyn 'static + FnOnce()>>>,
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
    /// Whether the folder was already pulled from the host, so the restore only
    /// happens once even though the delivery path runs repeatedly.
    restore_pulled: Cell<bool>,
    /// Whether the folder has been handed to the open listener yet.
    restore_delivered: Cell<bool>,
    /// Focus change reported by the host, applied from the frame loop.
    pending_focus: RefCell<Option<(String, bool)>>,
    /// The size each surface last reported, applied once per tick.
    ///
    /// Rebuilding the swapchain blocks this thread until the GPU is idle, and one
    /// maximize arrives from two places, so the work waits for the frame loop.
    /// One slot was not enough: two windows resized within the same frame left
    /// only the second, and the host reports a size only when it changes, so the
    /// first window kept rendering at its old size for good. Every surface keeps
    /// its own latest size and all of them are applied together.
    pending_surface_size: RefCell<HashMap<String, (u32, u32)>>,
    /// Window rect in physical pixels, per surface: (x, y, width, height).
    ///
    /// One value was not enough. The input method's candidate window is placed
    /// against the origin of the window being typed into, and with several windows
    /// open a single value put it over whichever window reported last.
    window_rect: RefCell<HashMap<String, (f32, f32, f32, f32)>>,
    /// What the host reported before any window reported a rectangle of its own.
    primary_window_rect: Cell<(f32, f32, f32, f32)>,
}

impl OhosPlatform {
    pub(crate) fn new() -> Result<Self> {
        let (main_sender, main_receiver) = PriorityQueueReceiver::new();
        let dispatcher = Arc::new(OhosDispatcher::new(main_sender));
        let background_executor = BackgroundExecutor::new(dispatcher.clone());
        let foreground_executor = ForegroundExecutor::new(dispatcher.clone());
        // Both of these are pushed by the page as soon as its host ops are in
        // place - the appearance and the window rectangle - so asking for them here
        // only bought a query the host had to answer during start-up, and the answer
        // was overwritten moments later. The defaults below stand for the instant
        // before that push arrives.
        let appearance = WindowAppearance::Light;
        let window_rect = (0.0, 0.0, 0.0, 0.0);
        Ok(Self {
            dispatcher,
            background_executor,
            foreground_executor,
            text_system: Arc::new(OhosTextSystem::new()),
            main_receiver,
            windows_opened: Cell::new(0),
            theme_appearance: Cell::new("system"),
            pending_launch: RefCell::new(None),
            menus: RefCell::new(Vec::new()),
            app_menu_action: RefCell::new(None),
            app_menu_will_open: RefCell::new(None),
            app_menu_validate: RefCell::new(None),
            open_urls: RefCell::new(None),
            on_quit: RefCell::new(None),
            on_reopen: RefCell::new(None),
            on_system_wake: RefCell::new(None),
            appearance: Cell::new(appearance),
            registry: WindowRegistry {
                focused_surface: RefCell::new(None),
                handles: RefCell::new(Vec::new()),
                windows: Rc::new(RefCell::new(Vec::new())),
                surfaces: Rc::new(RefCell::new(Vec::new())),
            },
            pending_picks: RefCell::new(HashMap::new()),
            next_pick_id: Cell::new(1),
            restore_folder: RefCell::new(None),
            restore_pulled: Cell::new(false),
            restore_delivered: Cell::new(false),
            pending_focus: RefCell::new(None),
            pending_surface_size: RefCell::new(HashMap::new()),
            window_rect: RefCell::new(HashMap::new()),
            primary_window_rect: Cell::new(window_rect),
        })
    }

    /// Host-pushed events: focus, window status, color mode, lifecycle, picker
    /// results. Everything arrives on the ArkUI UI thread.

    /// Record a focus change reported by the host.
    ///
    /// The platform keeps this rather than a thread local: everything that reads it
    /// runs on the platform's own thread, and a thread local there was a second
    /// truth waiting to disagree with this one.
    pub(crate) fn note_focus(&self, id: &str, focused: bool) {
        let mut current = self.registry.focused_surface.borrow_mut();
        if focused {
            *current = Some(id.to_string());
        } else if current.as_deref() == Some(id) {
            *current = None;
        }
    }

    /// The surface ArkUI currently focuses, if any. The input method binds to the
    /// focused window only, so this decides when a pending request may be replayed.
    pub(crate) fn focused_surface(&self) -> Option<String> {
        self.registry.focused_surface.borrow().clone()
    }
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
                // ArkUI delivers this while the application may be in the middle
                // of updating the window, and activating it there re-enters the
                // same update ("RefCell already borrowed"). Queue it for the frame
                // loop instead, which runs outside any update.
                let (id, value) = split_window_event(arg);
                self.note_focus(id, value != "0");
                *self.pending_focus.borrow_mut() = Some((id.to_string(), value != "0"));
            }
            host::event::WINDOW_STATUS => {
                // 1 full screen, 2 maximize, 3 minimize, 4 floating, 5 split.
                // On 2in1 the system reports MAXIMIZE for a screen-filling
                // window, which is what GPUI's titlebar treats as fullscreen.
                let (id, value) = split_window_event(arg);
                let fullscreen = value == "1" || value == "2";
                for window in self.route_targets(id) {
                    window.set_fullscreen(fullscreen);
                }
            }
            host::event::WINDOW_RECT => {
                let (id, value) = split_window_event(arg);
                if let Some(rect) = parse_rect(value) {
                    self.window_rect.borrow_mut().insert(id.to_string(), rect);
                    // The same size change also arrives as a window rectangle, and
                    // this used to be remembered only for the caret's origin. The
                    // surface was never told, so a maximized window kept rendering
                    // at its old size while the compositor stretched the picture,
                    // and layout and hit testing stayed stale at the old size too.
                    let (width, height) = (rect.2, rect.3);
                    if width > 0.0 && height > 0.0 {
                        let current = self
                            .registry
                            .surfaces
                            .borrow()
                            .iter()
                            .find(|(existing, _)| existing == id)
                            .map(|(_, surface)| {
                                let state = surface.borrow();
                                (state.width, state.height)
                            });
                        let wanted = (width as u32, height as u32);
                        if current != Some(wanted) {
                            self.surface_resized(id, wanted.0, wanted.1);
                        }
                    }
                }
            }
            host::event::BENCHMARK => {
                let frames: u32 = arg.parse().unwrap_or(720);
                super::window::BENCHMARK_FRAMES.store(frames, std::sync::atomic::Ordering::Relaxed);
                super::vk::log(&format!("[gpui_ohos] benchmark enabled frames={frames}"));
                self.request_frames();
            }
            host::event::LIFECYCLE => match arg {
                "background" | "destroy" => {
                    // Take the callback out first: holding the RefMut across
                    // the call would panic when it is stored back.
                    let mut callback = self.on_quit.borrow_mut().take();
                    if let Some(callback) = callback.as_mut() {
                        let _ = callback();
                    }
                    *self.on_quit.borrow_mut() = callback;
                }
                "newwant" => {
                    // The RefMut temporary lives until the end of the if-let
                    // body, so storing the callback back inside it would panic.
                    let mut callback = self.on_reopen.borrow_mut().take();
                    if let Some(callback) = callback.as_mut() {
                        callback();
                    }
                    *self.on_reopen.borrow_mut() = callback;
                }
                "foreground" => {
                    let mut callback = self.on_system_wake.borrow_mut().take();
                    if let Some(callback) = callback.as_mut() {
                        callback();
                    }
                    *self.on_system_wake.borrow_mut() = callback;
                }
                _ => {}
            },
            host::event::RESTORE_FOLDER => {
                if !arg.is_empty() {
                    *self.restore_folder.borrow_mut() = Some(arg.to_string());
                    self.deliver_restore_folder();
                }
            }
            host::event::PICK_RESULT => {
                let (token, rest) = arg.split_once('\t').unwrap_or((arg, ""));
                // The token is "p<id>" for a path pick and "n<id>" for a new
                // path, and the host echoes it back exactly as it was sent. Only
                // the digits are wanted, so a token that names nothing lands on
                // id 0 and finds no waiter instead of panicking on a slice.
                let id: u64 = token
                    .trim_start_matches(|character: char| !character.is_ascii_digit())
                    .parse()
                    .unwrap_or(0);
                let waiter = self.pending_picks.borrow_mut().remove(&id);
                match waiter {
                    Some(sender) => {
                        let lines: Vec<String> = rest
                            .lines()
                            .filter(|line| !line.is_empty())
                            .map(str::to_string)
                            .collect();
                        super::vk::log(&format!(
                            "[gpui_ohos] picker answer for {token}: {} item(s)",
                            lines.len()
                        ));
                        if sender.send(lines).is_err() {
                            super::vk::log(&format!(
                                "[gpui_ohos] picker answer for {token}: nobody is waiting"
                            ));
                        }
                    }
                    None => super::vk::log(&format!(
                        "[gpui_ohos] picker answer for {token} dropped: no waiter"
                    )),
                }
            }
            _ => {}
        }
    }

    pub(crate) fn take_restore_folder(&self) -> Option<String> {
        self.restore_folder.borrow_mut().take()
    }

    /// Hand the previous session's folder to the open listener once it exists.
    ///
    /// The host answers with the folder's sandbox path, and
    /// `OpenRequest::parse` strips the `file://` scheme and treats the rest as a
    /// path, so the URL carries that path unchanged.
    fn deliver_restore_folder(&self) {
        let uri = match self.take_restore_folder() {
            Some(uri) => {
                // A pushed folder is the one delivery; without this the later
                // pull would hand the same folder over twice.
                self.restore_pulled.set(true);
                uri
            }
            None if !self.restore_pulled.replace(true) => {
                // The host may have pushed the folder before the backend was
                // ready to receive events, so ask for it once.
                match host::restore_folder() {
                    Some(uri) => uri,
                    None => return,
                }
            }
            None => return,
        };
        let mut callback = self.open_urls.borrow_mut();
        match callback.as_mut() {
            Some(callback) => callback(vec![format!("file://{uri}")]),
            None => *self.restore_folder.borrow_mut() = Some(uri),
        }
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
        // Answers that will never be collected are dropped here: a caller that
        // gives up on a picker leaves its slot behind for ever otherwise, and the
        // sender and the future with it. Only entries whose receiver is already
        // gone are removed, so a picker still on screen is untouched.
        self.pending_picks
            .borrow_mut()
            .retain(|_, sender| !sender.is_canceled());
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
        let mut surfaces = self.registry.surfaces.borrow_mut();
        // Reuse the existing entry so any window already holding this Rc sees
        // the update; otherwise insert the primary at the front.
        if let Some((_, surface)) = surfaces.iter().find(|(existing, _)| existing == id) {
            let mut state = surface.borrow_mut();
            if state.valid && !state.window.is_null() && state.window != window {
                // Two native windows for one id. Overwriting would leave the first
                // window drawing into the second one's surface.
                super::vk::log(&format!(
                    "[gpui_ohos] surface {id} already has a window; not replacing it"
                ));
                return;
            }
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
            let mut surfaces = self.registry.surfaces.borrow_mut();
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
            if state.valid && !state.window.is_null() && state.window != window {
                // Two native windows for one id; the second would take the entry
                // over and the first window would draw into it.
                super::vk::log(&format!(
                    "[gpui_ohos] surface {id} already has a window; not replacing it"
                ));
                return;
            }
            state.window = window;
            state.width = width;
            state.height = height;
            state.valid = true;
        }
        for window in self.registry.windows.borrow().iter() {
            if window.shares_surface(&surface) {
                window.on_surface_resized(width, height);
            }
        }
    }

    /// The surface a new window should use: an existing one, or a fresh
    /// placeholder plus a host request to create the XComponent.
    fn surface_for_window(&self, params: &WindowParams) -> (String, Rc<RefCell<SurfaceState>>) {
        let index = self.windows_opened.get();
        self.windows_opened.set(index + 1);
        super::vk::log(&format!(
            "[gpui_ohos] window request index={index} surfaces={} reopened={}",
            self.registry.surfaces.borrow().len(),
            index < self.registry.surfaces.borrow().len()
        ));
        let mut surfaces = self.registry.surfaces.borrow_mut();
        let (width, height) = surfaces
            .first()
            .map(|(_, surface)| {
                let state = surface.borrow();
                (state.width, state.height)
            })
            .unwrap_or((2200, 1430));
        while surfaces.len() <= index {
            // Names are handed out once and never reused: a name that came back
            // would meet whatever the tables still hold for the window that had it.
            let id = format!(
                "gpui_surface_{}",
                NEXT_SURFACE.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            );
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
            let payload = create_window_payload(&id, params, width, height);
            host::window_op(host::op::CREATE_WINDOW, &payload);
            // The window needs the theme's appearance too: the report arrived
            // before this window existed.
            host::window_op_for(&id, host::op::SET_APPEARANCE, self.theme_appearance.get());
            surfaces = self.registry.surfaces.borrow_mut();
        }
        let id = surfaces[index].0.clone();
        (id, surfaces[index].1.clone())
    }

    pub(crate) fn surface_resized(&self, id: &str, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        // Recorded, not applied: this is called from the ArkUI resize callback,
        // and the work below blocks on the GPU and rebuilds the swapchain.
        self.pending_surface_size
            .borrow_mut()
            .insert(id.to_string(), (width, height));
    }

    /// Apply the sizes the surfaces last reported. Runs once per frame, and every
    /// surface with a pending size is applied: leaving one behind would keep that
    /// window at its old size until it was resized again.
    fn apply_pending_surface_size(&self) {
        let pending: Vec<(String, u32, u32)> = self
            .pending_surface_size
            .borrow_mut()
            .drain()
            .map(|(id, (width, height))| (id, width, height))
            .collect();
        for (id, width, height) in pending {
            self.apply_surface_resized(&id, width, height);
        }
    }

    fn apply_surface_resized(&self, id: &str, width: u32, height: u32) {
        let surface = {
            let surfaces = self.registry.surfaces.borrow();
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
        for window in self.registry.windows.borrow().iter() {
            if window.shares_surface(&surface) {
                window.on_surface_resized(width, height);
            }
        }
        self.request_frames();
    }

    pub(crate) fn surface_destroyed(&self, id: &str) {
        let surface = {
            let surfaces = self.registry.surfaces.borrow();
            surfaces
                .iter()
                .find(|(existing, _)| existing == id)
                .map(|(_, surface)| surface.clone())
        };
        let Some(surface) = surface else {
            // A second report about one window. The physical destruction is what
            // this stands for, and saying so twice must change nothing.
            super::vk::log(&format!("[gpui_ohos] surface destroyed again: {id}"));
            return;
        };
        {
            let mut state = surface.borrow_mut();
            state.valid = false;
            state.window = std::ptr::null_mut();
        }
        // The window is physically gone, and this is the only report the
        // application gets of it. It has to run its own close handling - taking
        // the workspace out of the session among it - or the window comes back on
        // the next start.
        let targets = self.route_targets(id);
        super::vk::log(&format!(
            "[gpui_ohos] surface gone {id}: {} window(s) matched",
            targets.len()
        ));
        for window in targets {
            window.notify_closed();
            window.mark_closed();
        }
        forget_window(&self.registry.handles, id);
        self.registry
            .windows
            .borrow_mut()
            .retain(|window| !window.is_closed());
        // A window that closes never reports a blur, so the surface the host
        // believes is focused would keep naming it and the next focus request
        // would look redundant when it is not.
        {
            let mut focused = self.registry.focused_surface.borrow_mut();
            if focused.as_deref() == Some(id) {
                *focused = None;
            }
        }
    }

    /// Invoke the GPUI launch callback recorded by Platform::run.
    pub(crate) fn launch(&self) {
        let callback = self.pending_launch.borrow_mut().take();
        if let Some(callback) = callback {
            callback();
        }
    }

    /// One host frame: drain host tasks and foreground tasks, fire due timers,
    /// then draw.
    pub(crate) fn tick(&self) {
        // Filesystem work hops here to reach the host bridge, which may only be
        // called from this thread. The budget keeps one slow read from costing
        // several frames.
        super::drain_ui_tasks(std::time::Duration::from_millis(2));
        // A resize waited for this moment: it blocks on the GPU.
        self.apply_pending_surface_size();
        // Asking for the previous folder while the app is still creating its
        // startup window opens the workspace in a second window and leaves an
        // empty one on top, so wait until a window has actually drawn.
        if !self.restore_delivered.get()
            && self
                .registry
                .windows
                .borrow()
                .iter()
                .any(|window| window.has_rendered())
        {
            self.restore_delivered.set(true);
            self.deliver_restore_folder();
        }
        let mut receiver = self.main_receiver.clone();
        while let Ok(Some(runnable)) = receiver.try_pop() {
            runnable.run();
        }
        // The application installs its own panic handler while starting; take the
        // hook back so panics keep reaching hilog and the panic file.
        static HOOK_TICKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        if HOOK_TICKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 30 {
            super::install_panic_hook();
        }
        // Focus changes reach gpui through a callback that updates the window,
        // and gpui's own appearance callback documents that this must not happen
        // while it still holds the app borrow: doing it here, between the frame
        // requests, produced "RefCell already borrowed" on every window
        // activation. Applying it after the frames leaves gpui idle.
        let frame_started = std::time::Instant::now();
        self.dispatcher.run_due_timers();
        self.request_frames();
        self.apply_pending_focus();
        // The benchmark measures how fast frames can be produced, not how often
        // the display asks for one, so keep drawing until its budget runs out.
        // The cap keeps a frame that never consumes budget from hanging here.
        let mut burst = 0;
        while benchmark_enabled() && burst < Self::BENCHMARK_BURST {
            self.request_frames();
            burst += 1;
        }
        // How long a frame costs on this thread is the number the frame rate
        // work is judged by, so sample it.
        static TICK_SAMPLE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let sample = TICK_SAMPLE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if sample % 120 == 0 {
            super::vk::log(&format!(
                "[gpui_ohos] tick work {}us burst={burst}",
                frame_started.elapsed().as_micros()
            ));
        }
    }

    /// How many frames one tick may produce while the benchmark runs.
    const BENCHMARK_BURST: usize = 32;

    /// Apply a focus change queued by `handle_host_event`. Only one window is
    /// active at a time, so the others are marked inactive; leaving them active
    /// keeps the input method attached to a window the user already left.
    fn apply_pending_focus(&self) {
        let Some((id, active)) = self.pending_focus.borrow_mut().take() else {
            return;
        };
        if active && !id.is_empty() {
            for window in self.windows() {
                window.set_active(window.id() == id);
            }
        } else {
            for window in self.route_targets(&id) {
                window.set_active(active);
            }
        }
    }

    pub(crate) fn request_frames(&self) {
        for window in self.registry.windows.borrow().iter() {
            if !window.is_closed() {
                window.request_frame();
            }
        }
    }

    pub(crate) fn window_count(&self) -> usize {
        self.registry
            .windows
            .borrow()
            .iter()
            .filter(|window| !window.is_closed())
            .count()
    }

    /// Windows an input event from a surface should reach. An empty id keeps
    /// the legacy broadcast to every window.
    /// Ask the windows an id names whether they may close.
    ///
    /// A window with no opinion allows it, so an unknown or already closed id
    /// does not hold a window open for ever.
    pub(crate) fn should_close_window(&self, id: &str) -> bool {
        // Not route_targets: that one skips windows already marked closed, and a
        // window whose surface has gone is exactly the one the application still
        // needs to be asked about - it is the application's answer that saves the
        // workspace out of the session before the window goes.
        let targets = self.targets_for_surface(id).len();
        let allowed = self
            .targets_for_surface(id)
            .iter()
            .all(|window| window.should_close());
        super::vk::log(&format!(
            "[gpui_ohos] asked to close {id}: {allowed} ({targets} window(s))"
        ));
        allowed
    }

    /// Every window bound to a surface, closed one included.
    fn targets_for_surface(&self, id: &str) -> Vec<Rc<WindowShared>> {
        if id.is_empty() {
            return self.registry.windows.borrow().clone();
        }
        let surface = {
            let surfaces = self.registry.surfaces.borrow();
            surfaces
                .iter()
                .find(|(existing, _)| existing == id)
                .map(|(_, surface)| surface.clone())
        };
        let Some(surface) = surface else {
            return Vec::new();
        };
        self.registry
            .windows
            .borrow()
            .iter()
            .filter(|window| window.shares_surface(&surface))
            .cloned()
            .collect()
    }

    pub(crate) fn route_targets(&self, id: &str) -> Vec<Rc<WindowShared>> {
        if id.is_empty() {
            return self.windows();
        }
        let surface = {
            let surfaces = self.registry.surfaces.borrow();
            surfaces
                .iter()
                .find(|(existing, _)| existing == id)
                .map(|(_, surface)| surface.clone())
        };
        let Some(surface) = surface else {
            return Vec::new();
        };
        self.registry
            .windows
            .borrow()
            .iter()
            .filter(|window| !window.is_closed() && window.shares_surface(&surface))
            .cloned()
            .collect()
    }

    /// Snapshot of live window state, for input routing and frame ticks.
    pub(crate) fn windows(&self) -> Vec<Rc<WindowShared>> {
        self.registry
            .windows
            .borrow()
            .iter()
            .filter(|window| !window.is_closed())
            .cloned()
            .collect()
    }

    /// Text, caret and absolute caret rect (physical pixels) of the focused
    /// editor, for IME context notifications.
    pub(crate) fn ime_context(&self) -> Option<(String, usize, (f64, f64, f64, f64))> {
        let rects = self.window_rect.borrow();
        let scale = super::window::SCALE as f64;
        // The focused window first: with several windows open, the first one with
        // an input handler is not necessarily the one being typed into, and the
        // input method would then be mirroring another window's text and caret.
        let windows = self.windows();
        let ordered = windows
            .iter()
            .filter(|window| window.is_active())
            .chain(windows.iter().filter(|window| !window.is_active()));
        for window in ordered {
            if let Some((text, caret, cursor)) = window.ime_context() {
                // This window's own origin, not whichever window reported last.
                let (rect_x, rect_y, _, _) = rects
                    .get(window.id())
                    .copied()
                    .unwrap_or_else(|| self.primary_window_rect.get());
                let left = rect_x as f64 + cursor.origin.x.as_f32() as f64 * scale;
                let top = rect_y as f64 + cursor.origin.y.as_f32() as f64 * scale;
                let width = cursor.size.width.as_f32() as f64 * scale;
                let height = cursor.size.height.as_f32() as f64 * scale;
                return Some((text, caret, (left, top, width, height)));
            }
        }
        None
    }

    /// Forward an IME command to the window being typed into.
    ///
    /// Every window used to be offered it in turn and the first one with an input
    /// handler took it, which was usually the main window's editor: text composed
    /// in another window's field went into the main window instead, and the field
    /// the user was looking at stayed empty. The window the user is typing into is
    /// the active one.
    pub(crate) fn handle_ime(&self, command: super::inputmethod::ImeCommand) {
        let windows = self.windows();
        let active = windows.iter().find(|window| window.is_active());
        // Falling back to the first window is what sent text composed in another
        // window into the main one. The input method's own window is the only
        // other legitimate target, and with neither there is nowhere to put the
        // text at all.
        let owner = super::inputmethod::owner();
        let target = active.or_else(|| {
            owner
                .as_deref()
                .and_then(|owner| windows.iter().find(|window| window.id() == owner))
        });
        let Some(window) = target else {
            super::vk::log("[gpui_ohos] ime command with no window to take it");
            return;
        };
        super::vk::log(&format!(
            "[gpui_ohos] ime -> {} ({})",
            window.id(),
            if active.is_some() { "active" } else { "owner" }
        ));
        window.handle_ime(&command);
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

/// Split a per-window host event into its surface id and payload.
fn split_window_event(arg: &str) -> (&str, &str) {
    match arg.split_once('\t') {
        Some((id, value)) => (id, value),
        None => ("", arg),
    }
}

/// How the host should open the window.
///
/// An editor window is a real top-level window that the taskbar lists and that
/// can be maximized on its own. The application's auxiliary windows - about,
/// settings, notifications - are marked with their own kinds, and they belong to
/// the window that opened them: they float above it and can be closed on their
/// own, which a window of a whole ability instance cannot be.
/// Surface names, handed out once and never reused: a name that came back would
/// meet whatever the tables still hold for the window that had it.
static NEXT_SURFACE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

fn window_kind_name(kind: &WindowKind) -> &'static str {
    match kind {
        WindowKind::Normal => "normal",
        WindowKind::Floating => "floating",
        WindowKind::PopUp | WindowKind::AnchoredPopup(_) => "popup",
        WindowKind::Dialog => "dialog",
        // LayerShell only exists when gpui is built with its wayland feature,
        // which this crate does not turn on, so a host here never opens one. The
        // arm is unreachable in that configuration and required in the other one.
        #[allow(unreachable_patterns)]
        _ => "normal",
    }
}

/// Payload for CREATE_WINDOW:
/// "<id>\t<left>,<top>,<width>,<height>\t<title>\t<resizable>\t<kind>".
fn create_window_payload(
    id: &str,
    params: &WindowParams,
    fallback_width: u32,
    fallback_height: u32,
) -> String {
    let scale = super::window::SCALE;
    let bounds = params.bounds;
    let left = (bounds.origin.x.as_f32() * scale).round() as i32;
    let top = (bounds.origin.y.as_f32() * scale).round() as i32;
    let width = (bounds.size.width.as_f32() * scale).round().max(0.0) as u32;
    let height = (bounds.size.height.as_f32() * scale).round().max(0.0) as u32;
    let (width, height) = if width == 0 || height == 0 {
        (fallback_width, fallback_height)
    } else {
        (width, height)
    };
    let title = params
        .titlebar
        .as_ref()
        .and_then(|titlebar| titlebar.title.as_ref())
        .map(|title| title.to_string())
        .unwrap_or_default();
    let resizable = if params.is_resizable { "1" } else { "0" };
    let kind = window_kind_name(&params.kind);
    format!("{id}\t{left},{top},{width},{height}\t{title}\t{resizable}\t{kind}")
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

/// Encode a raw picker answer as the handle GPUI passes around: "f|fd|name"
/// becomes "fd:<fd>" and "d|path|uri" becomes the plain sandbox path, because a
/// project opened from a tagged handle would otherwise show that tag as its
/// location in the UI.
fn picked_handle(line: &str) -> PathBuf {
    if let Some(rest) = line.strip_prefix("f|") {
        let mut parts = rest.split('|');
        let fd = parts.next().unwrap_or_default();
        let _name = parts.next();
        // The host sends the sandbox path after the name, because a path of the
        // form "fd:8" is relative as far as the editor is concerned and it
        // refuses to trust a project containing one.
        if let Some(path) = parts.next().filter(|path| path.starts_with('/')) {
            return PathBuf::from(path);
        }
        if !fd.is_empty() {
            return PathBuf::from(format!("fd:{fd}"));
        }
    }
    if let Some(rest) = line.strip_prefix("d|") {
        if let Some((path, _uri)) = rest.split_once('|') {
            return PathBuf::from(path);
        }
        return PathBuf::from(rest);
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

    fn quit(&self) {
        // The application asks to quit when it has no windows left, and the host
        // is what can end it. Leaving this empty meant the process was only ever
        // killed by the platform, so the application never got to run its
        // shutdown - which is where it writes the session, and why closed windows
        // kept coming back on the next start.
        //
        // The windows the application drops on the way out must not also ask the
        // host to close themselves: the host is ending the whole application, and
        // doing both left white windows behind.
        super::window::QUITTING.store(true, std::sync::atomic::Ordering::Relaxed);
        // The host ends the process, but not from here: the shutdown work this
        // returns into - the session write among it - is queued on this same
        // thread, and leaving before it drains loses it. The loop sends the quit
        // once the queue it was called in is empty, which is the acknowledgement
        // the host needed a fixed delay for.
        super::request_quit();
    }

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
            let surfaces = self.registry.surfaces.borrow();
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
        self.registry
            .handles
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
        // Windows the application has already closed are left out. The session
        // stores this list, and a closed window kept here came back the next time
        // the application started - which is exactly what the user saw.
        Some(
            self.registry
                .handles
                .borrow()
                .iter()
                .filter(|(_, shared)| shared.upgrade().map_or(false, |shared| !shared.is_closed()))
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
        let (id, surface) = self.surface_for_window(&options);
        let shared = WindowShared::new(id, surface, options, self.foreground_executor.clone());
        // The window holding the primary surface is the main one. The platform
        // tracks that; nothing compares ids by name.
        if self
            .registry
            .surfaces
            .borrow()
            .first()
            .map_or(false, |(first, _)| *first == shared.id())
        {
            shared.mark_primary();
        }
        self.registry.windows.borrow_mut().push(shared.clone());
        self.registry
            .handles
            .borrow_mut()
            .push((handle, Rc::downgrade(&shared)));
        Ok(Box::new(OhosWindow::new(shared)))
    }

    /// Zed reports which way round its theme is so that the native window chrome
    /// matches it. On this platform that chrome is drawn by the system, so the
    /// answer has to reach the host: it is what decides how the window buttons
    /// are drawn, and a light theme needs dark buttons and the other way round.
    fn set_window_appearance(&self, appearance: Option<WindowAppearance>) {
        let value = match appearance {
            Some(WindowAppearance::Dark) | Some(WindowAppearance::VibrantDark) => "dark",
            Some(WindowAppearance::Light) | Some(WindowAppearance::VibrantLight) => "light",
            None => "system",
        };
        // Remembered because Zed reports this before any window exists, and each
        // window is told again when it is created.
        self.theme_appearance.set(value);
        for window in self.registry.windows.borrow().iter() {
            host::window_op_for(window.id(), host::op::SET_APPEARANCE, value);
        }
    }

    fn window_appearance(&self) -> WindowAppearance {
        self.appearance.get()
    }

    fn open_url(&self, url: &str) {
        host::window_op(host::op::OPEN_URL, url);
    }

    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>) {
        // Only remember the listener; the folder is delivered from tick once a
        // window has drawn, so the startup window is not left behind.
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
        self.pending_picks
            .borrow_mut()
            .retain(|_, sender| !sender.is_canceled());
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
        // Cursor updates follow the mouse, so logging each one would drown the
        // log the same way a frame request would.
        host::window_op_quiet(host::op::SET_CURSOR, cursor_style_name(style));
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
