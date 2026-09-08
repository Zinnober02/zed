use std::{cell::RefCell, ffi::c_void, path::PathBuf, rc::Rc, sync::Arc};

use anyhow::Result;
use futures::channel::oneshot;

use crate::{
    Action, AnyWindowHandle, BackgroundExecutor, ClipboardItem, CursorStyle, ForegroundExecutor,
    Keymap, Menu, MenuItem, Modifiers, OwnedMenu, PathPromptOptions, Platform, PlatformInput,
    PlatformDisplay, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem,
    PlatformWindow, PriorityQueueReceiver, Result as GpuiResult, RunnableVariant, Task,
    ThermalState, WindowAppearance, WindowParams,
};

use super::dispatcher::OhosDispatcher;
use super::display::OhosDisplay;
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
    surface: Rc<RefCell<SurfaceState>>,
    pending_launch: RefCell<Option<Box<dyn 'static + FnOnce()>>>,
    windows: Rc<RefCell<Vec<Rc<WindowShared>>>>,
}

impl OhosPlatform {
    pub(crate) fn new() -> Result<Self> {
        let (main_sender, main_receiver) = PriorityQueueReceiver::new();
        let dispatcher = Arc::new(OhosDispatcher::new(main_sender));
        let background_executor = BackgroundExecutor::new(dispatcher.clone());
        let foreground_executor = ForegroundExecutor::new(dispatcher.clone());
        Ok(Self {
            dispatcher,
            background_executor,
            foreground_executor,
            text_system: Arc::new(OhosTextSystem::new()),
            main_receiver,
            surface: Rc::new(RefCell::new(SurfaceState::default())),
            pending_launch: RefCell::new(None),
            windows: Rc::new(RefCell::new(Vec::new())),
        })
    }

    pub(crate) fn set_surface(&self, window: *mut c_void, width: u32, height: u32) {
        let mut surface = self.surface.borrow_mut();
        surface.window = window;
        surface.width = width;
        surface.height = height;
        surface.valid = true;
    }

    pub(crate) fn surface_resized(&self, width: u32, height: u32) {
        {
            let mut surface = self.surface.borrow_mut();
            surface.width = width;
            surface.height = height;
            surface.valid = true;
        }
        for window in self.windows.borrow().iter() {
            window.on_surface_resized(width, height);
        }
        self.request_frames();
    }

    pub(crate) fn surface_destroyed(&self) {
        let mut surface = self.surface.borrow_mut();
        surface.valid = false;
        surface.window = std::ptr::null_mut();
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
            window.request_frame();
        }
    }

    pub(crate) fn window_count(&self) -> usize {
        self.windows.borrow().len()
    }

    /// Snapshot of live window state, for input routing and frame ticks.
    pub(crate) fn windows(&self) -> Vec<Rc<WindowShared>> {
        self.windows.borrow().clone()
    }

    /// Forward an IME command to every window's input handler.
    pub(crate) fn handle_ime(&self, command: super::inputmethod::ImeCommand) {
        for window in self.windows() {
            window.handle_ime(&command);
        }
    }

    /// Forward a modifier-state change to every window.
    pub(crate) fn dispatch_modifiers(&self, modifiers: Modifiers) {
        for window in self.windows() {
            window.dispatch_input(PlatformInput::ModifiersChanged(
                crate::ModifiersChangedEvent {
                    modifiers,
                    capslock: crate::Capslock::default(),
                },
            ));
        }
    }

    /// Forward a key press or release to every window.
    pub(crate) fn dispatch_key(&self, down: bool, keystroke: crate::Keystroke) {
        for window in self.windows() {
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
            let surface = self.surface.borrow();
            (surface.width, surface.height)
        };
        vec![Rc::new(OhosDisplay::new(w, h, super::window::SCALE)) as Rc<dyn PlatformDisplay>]
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        self.displays().into_iter().next()
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        None
    }

    fn window_stack(&self) -> Option<Vec<AnyWindowHandle>> {
        None
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
        _handle: AnyWindowHandle,
        options: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        if !self.surface.borrow().valid {
            anyhow::bail!("OHOS surface is not available yet");
        }
        let shared = WindowShared::new(
            self.surface.clone(),
            options,
            self.foreground_executor.clone(),
        );
        self.windows.borrow_mut().push(shared.clone());
        Ok(Box::new(OhosWindow::new(shared)))
    }

    fn window_appearance(&self) -> WindowAppearance {
        WindowAppearance::Light
    }

    fn open_url(&self, _url: &str) {}

    fn on_open_urls(&self, _callback: Box<dyn FnMut(Vec<String>)>) {}

    fn register_url_scheme(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!(
            "URL scheme registration not supported on OHOS"
        )))
    }

    fn prompt_for_paths(
        &self,
        _options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        let (tx, rx) = oneshot::channel();
        tx.send(Ok(None)).ok();
        rx
    }

    fn prompt_for_new_path(
        &self,
        _directory: &std::path::Path,
        _suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        let (tx, rx) = oneshot::channel();
        tx.send(Ok(None)).ok();
        rx
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        false
    }

    fn reveal_path(&self, _path: &std::path::Path) {}

    fn open_with_system(&self, _path: &std::path::Path) {}

    fn on_quit(&self, _callback: Box<dyn FnMut() -> bool>) {}

    fn on_reopen(&self, _callback: Box<dyn FnMut()>) {}

    fn on_system_wake(&self, _callback: Box<dyn FnMut()>) {}

    fn set_menus(&self, _menus: Vec<Menu>, _keymap: &Keymap) {}

    fn get_menus(&self) -> Option<Vec<OwnedMenu>> {
        None
    }

    fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &Keymap) {}

    fn on_app_menu_action(&self, _callback: Box<dyn FnMut(&dyn Action)>) {}

    fn on_will_open_app_menu(&self, _callback: Box<dyn FnMut()>) {}

    fn on_validate_app_menu_command(&self, _callback: Box<dyn FnMut(&dyn Action) -> bool>) {}

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

    fn set_cursor_style(&self, _style: CursorStyle) {}

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
