use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    ptr::NonNull,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use anyhow::Result;
use futures::channel::oneshot;
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, OhosNdkWindowHandle,
    RawWindowHandle, WindowHandle,
};

use crate::{
    AtlasTextureKind, Bounds, Capslock, ContentMask, Decorations, DispatchEventResult,
    ForegroundExecutor, GpuSpecs, Modifiers, MonochromeSprite, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Path, Pixels, PlatformAtlas, PlatformDisplay, PlatformInput,
    PlatformInputHandler, PlatformWindow, Point, PolychromeSprite, PrimitiveBatch, PromptButton,
    PromptLevel, Quad, RequestFrameOptions, ResizeEdge, ScaledPixels, Scene, ScrollDelta,
    ScrollWheelEvent, Size, TouchPhase, Underline, WindowAppearance, WindowBackgroundAppearance,
    WindowBounds, WindowControlArea, WindowControls, WindowParams, point, px,
};

use super::atlas::OhosAtlas;
use super::display::OhosDisplay;
use super::host;
use super::platform::SurfaceState;
use super::vk::{DrawBatch, DrawPipeline, VkRenderer};

/// Clear color used until the GPUI scene renderer lands (M2+).
/// Teal so it is unambiguous versus the old probe blue.
const CLEAR_COLOR: [f32; 4] = [0.0, 0.55, 0.45, 1.0];

/// Logical-to-device pixel scale. The 2in1 panel is 3120x2080 at a high DPI,
/// so a logical pixel maps to ~3 device pixels (otherwise 16px text is tiny).
pub(crate) const SCALE: f32 = 3.0;

/// Frames left in the forced-redraw benchmark; zero means the normal
/// on-demand rendering path. The host enables it when the app is launched
/// with the `gpuiBenchmark` parameter so frame-rate measurements present on
/// every vsync instead of only when the scene changes.
pub(crate) static BENCHMARK_FRAMES: AtomicU32 = AtomicU32::new(0);

pub(crate) fn benchmark_enabled() -> bool {
    BENCHMARK_FRAMES.load(Ordering::Relaxed) > 0
}

/// Consume one benchmark frame, returning true while the benchmark runs.
fn take_benchmark_frame() -> bool {
    let remaining = BENCHMARK_FRAMES.load(Ordering::Relaxed);
    if remaining == 0 {
        return false;
    }
    if remaining == 1 {
        super::vk::log("[gpui_ohos] benchmark finished");
    }
    BENCHMARK_FRAMES.store(remaining - 1, Ordering::Relaxed);
    true
}

pub(crate) struct WindowCallbacks {
    request_frame: Option<Box<dyn FnMut(RequestFrameOptions)>>,
    input: Option<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>,
    active_status_change: Option<Box<dyn FnMut(bool)>>,
    hover_status_change: Option<Box<dyn FnMut(bool)>>,
    resize: Option<Box<dyn FnMut(Size<Pixels>, f32)>>,
    moved: Option<Box<dyn FnMut()>>,
    should_close: Option<Box<dyn FnMut() -> bool>>,
    close: Option<Box<dyn FnOnce()>>,
    appearance_changed: Option<Box<dyn FnMut()>>,
    hit_test_window_control: Option<Box<dyn FnMut() -> Option<WindowControlArea>>>,
}

impl Default for WindowCallbacks {
    fn default() -> Self {
        Self {
            request_frame: None,
            input: None,
            active_status_change: None,
            hover_status_change: None,
            resize: None,
            moved: None,
            should_close: None,
            close: None,
            appearance_changed: None,
            hit_test_window_control: None,
        }
    }
}

/// State shared between the boxed PlatformWindow owned by GPUI and the
/// OhosPlatform, which needs to drive frames and surface changes.
pub(crate) struct WindowShared {
    /// Host-side window id ("gpui_surface" or "gpui_surface_N").
    id: String,
    /// Whether this is the window bound to the primary surface.
    ///
    /// The main window is a state the platform tracks. It used to be a name
    /// comparison against the host's primary id, which broke the moment anything
    /// else carried that name.
    is_primary: Cell<bool>,
    surface: Rc<RefCell<SurfaceState>>,
    bounds: RefCell<Bounds<Pixels>>,
    scale: Cell<f32>,
    display: OhosDisplay,
    input_handler: RefCell<Option<PlatformInputHandler>>,
    callbacks: RefCell<WindowCallbacks>,
    renderer: RefCell<Option<VkRenderer>>,
    /// Native window the current renderer was built for, as an address.
    ///
    /// A rebuilt surface hands over a different native window while the renderer
    /// stays bound to the old one, so the renderer has to be rebuilt too; the
    /// pointer is kept as an address because it is only ever compared.
    renderer_window: Cell<usize>,
    pointer_position: Cell<Point<Pixels>>,
    pressed_button: Cell<Option<MouseButton>>,
    modifiers: Cell<Modifiers>,
    active: Cell<bool>,
    hovered: Cell<bool>,
    fullscreen: Cell<bool>,
    appearance: Cell<WindowAppearance>,
    /// Caret rectangle in logical window coordinates, for the IME panel.
    ime_cursor: Cell<Bounds<Pixels>>,
    render_failure_logged: Cell<bool>,
    frame_count: Cell<u64>,
    /// Set when the host destroyed this window's surface; the platform then
    /// stops ticking it and drops its reference.
    closed: Cell<bool>,
    /// The last visibility GPUI asked for, so repeated requests are a no-op.
    virtual_keyboard_visible: Cell<bool>,
    atlas: Arc<OhosAtlas>,
    last_scene_hash: Cell<Option<u64>>,
    #[allow(dead_code)]
    foreground_executor: ForegroundExecutor,
}

impl WindowShared {
    pub(crate) fn new(
        id: String,
        surface: Rc<RefCell<SurfaceState>>,
        _params: WindowParams,
        foreground_executor: ForegroundExecutor,
    ) -> Rc<Self> {
        let (w, h) = {
            let s = surface.borrow();
            (s.width, s.height)
        };
        let scale = SCALE;
        let bounds = Bounds::new(
            point(px(0.0), px(0.0)),
            Size {
                width: px(w as f32 / scale),
                height: px(h as f32 / scale),
            },
        );
        Rc::new(Self {
            id,
            is_primary: Cell::new(false),
            surface,
            bounds: RefCell::new(bounds),
            scale: Cell::new(scale),
            display: OhosDisplay::new(w, h, scale),
            input_handler: RefCell::new(None),
            callbacks: RefCell::new(WindowCallbacks::default()),
            renderer: RefCell::new(None),
            renderer_window: Cell::new(0),
            pointer_position: Cell::new(point(px(0.0), px(0.0))),
            pressed_button: Cell::new(None),
            modifiers: Cell::new(Modifiers::default()),
            active: Cell::new(true),
            hovered: Cell::new(true),
            fullscreen: Cell::new(false),
            appearance: Cell::new(WindowAppearance::Light),
            ime_cursor: Cell::new(Bounds::new(
                point(px(0.0), px(0.0)),
                Size {
                    width: px(2.0),
                    height: px(20.0),
                },
            )),
            render_failure_logged: Cell::new(false),
            frame_count: Cell::new(0),
            closed: Cell::new(false),
            virtual_keyboard_visible: Cell::new(false),
            atlas: Arc::new(OhosAtlas::new()),
            last_scene_hash: Cell::new(None),
            foreground_executor,
        })
    }

    /// The host-side id, used to route commands to this window.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Record that this window is the one the primary surface belongs to.
    pub(crate) fn mark_primary(&self) {
        self.is_primary.set(true);
    }

    /// Whether this is the main window. Its close belongs to the system's own
    /// close button, which asks the application first.
    pub(crate) fn is_primary(&self) -> bool {
        self.is_primary.get()
    }

    pub(crate) fn mark_closed(&self) {
        self.closed.set(true);
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.get()
    }

    pub(crate) fn request_frame(&self) {
        let mut callback = self.callbacks.borrow_mut().request_frame.take();
        if let Some(cb) = callback.as_mut() {
            cb(RequestFrameOptions {
                require_presentation: true,
                force_render: benchmark_enabled(),
            });
        }
        self.callbacks.borrow_mut().request_frame = callback;
    }

    /// Current editor text, caret (UTF-16) and caret rect, mirrored to the
    /// input method.
    pub(crate) fn ime_context(&self) -> Option<(String, usize, Bounds<Pixels>)> {
        let cursor = self.ime_cursor.get();
        let mut guard = self.input_handler.borrow_mut();
        let handler = guard.as_mut()?;
        let mut adjusted = None;
        let text = handler.text_for_range(0..usize::MAX, &mut adjusted)?;
        let selection = handler.selected_text_range(true)?;
        Some((text, selection.range.end, cursor))
    }

    pub(crate) fn shares_surface(&self, other: &Rc<RefCell<SurfaceState>>) -> bool {
        Rc::ptr_eq(&self.surface, other)
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.get()
    }

    /// Whether this window has drawn at least one frame.
    pub(crate) fn has_rendered(&self) -> bool {
        self.frame_count.get() > 0
    }

    pub(crate) fn set_active(&self, active: bool) {
        if self.active.get() == active {
            return;
        }
        self.active.set(active);
        let mut callback = self.callbacks.borrow_mut().active_status_change.take();
        if let Some(callback) = callback.as_mut() {
            callback(active);
        }
        self.callbacks.borrow_mut().active_status_change = callback;
    }

    pub(crate) fn set_fullscreen(&self, fullscreen: bool) {
        self.fullscreen.set(fullscreen);
    }

    pub(crate) fn set_appearance(&self, appearance: WindowAppearance) {
        if self.appearance.get() == appearance {
            return;
        }
        self.appearance.set(appearance);
        let mut callback = self.callbacks.borrow_mut().appearance_changed.take();
        if let Some(callback) = callback.as_mut() {
            callback();
        }
        self.callbacks.borrow_mut().appearance_changed = callback;
    }

    /// Ask the application whether this window may close.
    ///
    /// The application answers here when it needs to save something first, and
    /// closes the window itself once it is done; saying yes means the platform
    /// may close it straight away.
    pub(crate) fn should_close(&self) -> bool {
        let mut callback = self.callbacks.borrow_mut().should_close.take();
        let allowed = match callback.as_mut() {
            Some(callback) => callback(),
            None => true,
        };
        self.callbacks.borrow_mut().should_close = callback;
        allowed
    }

    /// Tell the application the platform window is gone.
    ///
    /// Without this the application only ever cleared a window it closed itself,
    /// so a window closed from the system stayed in the session and came back the
    /// next time it started.
    pub(crate) fn notify_closed(&self) {
        // A window that closes never reports a blur, so the surface the host
        // believes is focused would keep naming it and the next focus request
        // would look redundant when it is not.
        super::note_focus(&self.id, false);
        let close = self.callbacks.borrow_mut().close.take();
        super::vk::log(&format!(
            "[gpui_ohos] window {} closed: callback {}",
            self.id,
            if close.is_some() { "found" } else { "missing" }
        ));
        if let Some(close) = close {
            close();
        }
    }

    /// Apply an IME command to the focused input handler.
    pub(crate) fn handle_ime(&self, command: &super::inputmethod::ImeCommand) {
        use super::inputmethod::ImeCommand;
        if let ImeCommand::MoveCursor(direction) = command {
            let key = match direction {
                1 => "up",
                2 => "down",
                3 => "left",
                4 => "right",
                _ => return,
            };
            self.dispatch_input(PlatformInput::KeyDown(crate::KeyDownEvent {
                keystroke: crate::Keystroke {
                    modifiers: Default::default(),
                    key: key.to_string(),
                    key_char: None,
                },
                is_held: false,
                prefer_character_input: false,
            }));
            return;
        }
        let mut guard = self.input_handler.borrow_mut();
        let Some(handler) = guard.as_mut() else {
            // Silent before, which left "the composition showed and nothing
            // happened" with no trace at all.
            super::vk::log(&format!(
                "[gpui_ohos] ime dropped for {}: no input handler",
                self.id
            ));
            return;
        };
        match command {
            ImeCommand::Commit(text) => {
                // Not None: that means "replace the marked range", and the input
                // method may finish the preview before it reports the confirmation -
                // the mark is gone by then and the replacement lands nowhere, which
                // is how a confirmed composition was lost. The marked range is used
                // when it is still there, and the caret (an empty selection) when it
                // is not.
                let range = handler.marked_text_range().or_else(|| {
                    handler
                        .selected_text_range(false)
                        .map(|selection| selection.range)
                });
                handler.replace_text_in_range(range, text);
                handler.unmark_text();
            }
            ImeCommand::Backspace(count) => {
                if let Some(selection) = handler.selected_text_range(true) {
                    let end = selection.range.end;
                    let start = end.saturating_sub(*count);
                    handler.replace_text_in_range(Some(start..end), "");
                }
            }
            ImeCommand::DeleteForward(count) => {
                if let Some(selection) = handler.selected_text_range(true) {
                    let start = selection.range.end;
                    let end = start + *count;
                    handler.replace_text_in_range(Some(start..end), "");
                }
            }
            ImeCommand::Preview { text, start, end } => {
                // start/end are UTF-16 offsets; -1/-1 means "the whole preview".
                let range = if *start < 0 || *end < 0 {
                    handler.marked_text_range()
                } else {
                    Some(*start as usize..*end as usize)
                };
                handler.replace_and_mark_text_in_range(range, text, None);
            }
            ImeCommand::ClearPreview => {
                handler.unmark_text();
            }
            ImeCommand::MoveCursor(_) => {}
        }
    }

    pub(crate) fn dispatch_input(&self, input: PlatformInput) -> DispatchEventResult {
        let mut callback = self.callbacks.borrow_mut().input.take();
        let mut result = DispatchEventResult::default();
        if let Some(cb) = callback.as_mut() {
            result = cb(input);
        } else {
            super::vk::log("[gpui_ohos] input dropped: no handler installed");
        }
        self.callbacks.borrow_mut().input = callback;
        result
    }

    /// Called by the platform when the XComponent surface changes size.
    pub(crate) fn on_surface_resized(&self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let scale = self.scale.get();
        let new_bounds = Bounds::new(
            point(px(0.0), px(0.0)),
            Size {
                width: px(width as f32 / scale),
                height: px(height as f32 / scale),
            },
        );
        *self.bounds.borrow_mut() = new_bounds;
        if let Some(renderer) = self.renderer.borrow_mut().as_mut() {
            if let Err(error) = renderer.resize(width, height) {
                super::vk::log(&format!("[gpui_ohos] swapchain resize failed: {error}"));
            }
        }
        let mut callback = self.callbacks.borrow_mut().resize.take();
        if let Some(cb) = callback.as_mut() {
            cb(new_bounds.size, scale);
        }
        self.callbacks.borrow_mut().resize = callback;
    }

    fn ensure_renderer(&self) -> bool {
        let (window, width, height, valid) = {
            let s = self.surface.borrow();
            (s.window, s.width, s.height, s.valid)
        };
        if !valid || window.is_null() {
            return false;
        }
        if self.renderer.borrow().is_some() {
            if self.renderer_window.get() == window as usize {
                return true;
            }
            // Keeping a renderer built for the previous native window would draw
            // into a window that no longer exists; drop it first so its own
            // teardown runs before a replacement is created.
            super::vk::log("[gpui_ohos] native window changed, rebuilding the renderer");
            *self.renderer.borrow_mut() = None;
        }
        match VkRenderer::new(window, width, height) {
            Ok(renderer) => {
                super::vk::log(&format!(
                    "[gpui_ohos] Vulkan renderer ready: {}x{}",
                    renderer.size().0,
                    renderer.size().1
                ));
                if let Some(specs) = renderer.gpu_specs() {
                    super::vk::log(&format!(
                        "[gpui_ohos] GPU: {} | {} | {}",
                        specs.device_name, specs.driver_name, specs.driver_info
                    ));
                }
                *self.renderer.borrow_mut() = Some(renderer);
                self.renderer_window.set(window as usize);
                true
            }
            Err(error) => {
                if !self.render_failure_logged.get() {
                    self.render_failure_logged.set(true);
                    super::vk::log(&format!("[gpui_ohos] Vulkan init failed: {error}"));
                }
                false
            }
        }
    }

    pub(crate) fn render(&self, scene: &Scene) {
        if self.closed.get() || !self.ensure_renderer() {
            return;
        }
        // The benchmark forces redraws of an unchanged scene, so it must not pay
        // for hashing one.
        let hash_started = std::time::Instant::now();
        let benchmark = take_benchmark_frame();
        let hash = if benchmark { 0 } else { scene_hash(scene) };
        if !benchmark
            && self.last_scene_hash.get() == Some(hash)
            && !self.atlas.has_pending_uploads()
        {
            return;
        }
        let frame = self.frame_count.get() + 1;
        self.frame_count.set(frame);
        if frame % 120 == 1 {
            super::vk::log(&format!(
                "[gpui_ohos] frame {frame} quads={} glyphs={}",
                scene.quads.len(),
                scene.monochrome_sprites.len()
            ));
        }
        let work_started = std::time::Instant::now();
        let uploads = self.atlas.take_uploads();
        let (atlas_w, atlas_h) = self.atlas.texture_size(AtlasTextureKind::Monochrome);
        let (sprite_w, sprite_h) = self.atlas.texture_size(AtlasTextureKind::Polychrome);
        let presented = if let Some(renderer) = self.renderer.borrow_mut().as_mut() {
            let (width, height) = renderer.size();
            let (quads, paths, glyphs, sprites, batches) =
                build_draw_list(scene, width, height, atlas_w, atlas_h, sprite_w, sprite_h);
            match renderer.render_scene(
                CLEAR_COLOR,
                &quads,
                &paths,
                &glyphs,
                &sprites,
                &batches,
                &uploads,
            ) {
                Ok(presented) => presented,
                Err(error) => {
                    super::vk::log(&format!("[gpui_ohos] render failed: {error}"));
                    true
                }
            }
        } else {
            false
        };
        // What a frame costs on the CPU decides how fast it could go on a display
        // that asks for more, since presenting itself is paced by the display.
        static CPU_SAMPLE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let sample = CPU_SAMPLE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if sample % 60 == 0 {
            super::vk::log(&format!(
                // The first figure is everything after the hash - scene building
                // and presenting, which includes waiting for the compositor - and
                // the second is the hash itself.
                "[gpui_ohos] frame cpu {}us after hashing ({}us hashing)",
                work_started.elapsed().as_micros(),
                work_started
                    .saturating_duration_since(hash_started)
                    .as_micros()
            ));
        }
        // A frame that never reached the display must be drawn again, so the hash
        // is only remembered once it has been presented.
        if !benchmark && presented {
            self.last_scene_hash.set(Some(hash));
        }
    }

    pub(crate) fn pointer_down(&self, button: MouseButton, position: Point<Pixels>) {
        self.pointer_position.set(position);
        self.pressed_button.set(Some(button));
        self.dispatch_input(PlatformInput::MouseDown(MouseDownEvent {
            button,
            position,
            modifiers: self.modifiers.get(),
            click_count: 1,
            first_mouse: false,
        }));
    }

    pub(crate) fn pointer_up(&self, button: MouseButton, position: Point<Pixels>) {
        self.pointer_position.set(position);
        self.pressed_button.set(None);
        self.dispatch_input(PlatformInput::MouseUp(MouseUpEvent {
            button,
            position,
            modifiers: self.modifiers.get(),
            click_count: 1,
        }));
    }

    pub(crate) fn pointer_move(&self, position: Point<Pixels>) {
        self.pointer_position.set(position);
        self.dispatch_input(PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: self.pressed_button.get(),
            modifiers: self.modifiers.get(),
        }));
    }

    pub(crate) fn scroll(&self, position: Point<Pixels>, delta: Point<Pixels>, phase: TouchPhase) {
        self.pointer_position.set(position);
        self.dispatch_input(PlatformInput::ScrollWheel(ScrollWheelEvent {
            position,
            delta: ScrollDelta::Pixels(delta),
            modifiers: self.modifiers.get(),
            touch_phase: phase,
        }));
    }
}

pub(crate) struct OhosWindow {
    shared: Rc<WindowShared>,
}

/// Whether closing a window from the application also closes the platform window.
///
/// On, except for the main window. Ending the ability behind a window was measured
/// to take the whole application down, and the main window belongs to the entry
/// ability, so it is excluded: only the system's own close button should end it.
/// The other windows are closed by the host - a sub-window directly, a window of
/// its own ability instance by ending that instance, which is what the system's
/// close does for them too.
const CLOSE_WINDOW_WITH_APP: bool = true;

/// Set once the application has asked to quit.
///
/// Windows dropped as part of that quit must not each ask the host to close
/// their own window: the host is already ending the whole application, and doing
/// both left white windows behind, because the host destroyed the surfaces while
/// the window's own instance was going away.
pub(crate) static QUITTING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

impl Drop for OhosWindow {
    fn drop(&mut self) {
        // The application drops this whenever it closes a window itself, and that
        // is the only notice this side gets of it. Without marking the window
        // closed the frame loop kept asking a window the application no longer
        // had to draw, and every frame came back "window not found".
        super::vk::log(&format!(
            "[gpui_ohos] dropped by the application: {} (primary={})",
            self.shared.id(),
            self.shared.is_primary()
        ));
        self.shared.mark_closed();
        super::inputmethod::forget(self.shared.id());
        // Never for the main window. It belongs to the entry ability, and asking
        // the host to close it takes that ability - and with it the whole user
        // interface - down, which is what left every other window blank. The
        // system's own close button is the only thing that should end it, and that
        // path asks the application first.
        let is_main = self.shared.is_primary();
        if CLOSE_WINDOW_WITH_APP && !is_main && !QUITTING.load(std::sync::atomic::Ordering::Relaxed)
        {
            host::window_op_for(self.shared.id(), host::op::CLOSE_WINDOW, "");
        }
    }
}

impl OhosWindow {
    pub(crate) fn new(shared: Rc<WindowShared>) -> Self {
        Self { shared }
    }
}

impl HasWindowHandle for OhosWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let window: *mut c_void = self.shared.surface.borrow().window;
        let native_window = NonNull::new(window).ok_or(HandleError::Unavailable)?;
        let raw = RawWindowHandle::OhosNdk(OhosNdkWindowHandle::new(native_window));
        // SAFETY: the OHNativeWindow outlives this window while the surface is alive.
        unsafe { Ok(WindowHandle::borrow_raw(raw)) }
    }
}

impl HasDisplayHandle for OhosWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(DisplayHandle::ohos())
    }
}

impl PlatformWindow for OhosWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        *self.shared.bounds.borrow()
    }

    fn is_maximized(&self) -> bool {
        false
    }

    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Windowed(*self.shared.bounds.borrow())
    }

    fn content_size(&self) -> Size<Pixels> {
        self.shared.bounds.borrow().size
    }

    fn resize(&mut self, size: Size<Pixels>) {
        let origin = self.shared.bounds.borrow().origin;
        *self.shared.bounds.borrow_mut() = Bounds::new(origin, size);
    }

    fn scale_factor(&self) -> f32 {
        self.shared.scale.get()
    }

    fn appearance(&self) -> WindowAppearance {
        self.shared.appearance.get()
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(Rc::new(self.shared.display.clone()) as Rc<dyn PlatformDisplay>)
    }

    fn mouse_position(&self) -> Point<Pixels> {
        self.shared.pointer_position.get()
    }

    fn modifiers(&self) -> Modifiers {
        self.shared.modifiers.get()
    }

    fn capslock(&self) -> Capslock {
        Capslock::default()
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        *self.shared.input_handler.borrow_mut() = Some(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.shared.input_handler.borrow_mut().take()
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<oneshot::Receiver<usize>> {
        None
    }

    fn activate(&self) {
        self.shared.active.set(true);
        let mut callback = self
            .shared
            .callbacks
            .borrow_mut()
            .active_status_change
            .take();
        if let Some(cb) = callback.as_mut() {
            cb(true);
        }
        self.shared.callbacks.borrow_mut().active_status_change = callback;
    }

    fn is_active(&self) -> bool {
        self.shared.active.get()
    }

    fn is_hovered(&self) -> bool {
        self.shared.hovered.get()
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Opaque
    }

    fn set_title(&mut self, title: &str) {
        host::window_op_for(self.shared.id(), host::op::SET_TITLE, title);
    }

    fn set_background_appearance(&self, appearance: WindowBackgroundAppearance) {
        // Driven by the theme: a theme with a transparent or blurred background
        // needs the window itself to be see-through, and leaving this empty made
        // every theme opaque whatever it asked for.
        let value = match appearance {
            WindowBackgroundAppearance::Opaque => "opaque",
            WindowBackgroundAppearance::Transparent => "transparent",
            WindowBackgroundAppearance::Blurred => "blurred",
            // The Mica backdrops are a Windows idea; a blur is the closest the
            // platform here has, and it is free to ignore it.
            WindowBackgroundAppearance::MicaBackdrop
            | WindowBackgroundAppearance::MicaAltBackdrop => "blurred",
        };
        host::window_op_for(self.shared.id(), host::op::SET_BACKGROUND, value);
    }

    fn minimize(&self) {
        host::window_op_for(self.shared.id(), host::op::MINIMIZE, "");
    }

    fn zoom(&self) {
        host::window_op_for(self.shared.id(), host::op::MAXIMIZE, "");
    }

    fn toggle_fullscreen(&self) {
        let target = if self.shared.fullscreen.get() {
            "0"
        } else {
            "1"
        };
        host::window_op_for(self.shared.id(), host::op::SET_FULLSCREEN, target);
    }

    fn is_fullscreen(&self) -> bool {
        self.shared.fullscreen.get()
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.shared.callbacks.borrow_mut().request_frame = Some(callback);
    }

    /// gpui tells the platform that it wants a frame drawn. The host draws on
    /// every refresh anyway, so this only has to bring the next frame forward
    /// for changes that arrive between refreshes, such as background work
    /// finishing.
    fn schedule_frame(&self) {
        super::host::request_frame();
    }

    fn frame_waker(&self) -> Option<std::rc::Rc<dyn Fn()>> {
        Some(std::rc::Rc::new(super::host::request_frame))
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        self.shared.callbacks.borrow_mut().input = Some(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.shared.callbacks.borrow_mut().active_status_change = Some(callback);
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.shared.callbacks.borrow_mut().hover_status_change = Some(callback);
    }

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.shared.callbacks.borrow_mut().resize = Some(callback);
    }

    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.shared.callbacks.borrow_mut().moved = Some(callback);
    }

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.shared.callbacks.borrow_mut().should_close = Some(callback);
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.shared.callbacks.borrow_mut().close = Some(callback);
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.shared.callbacks.borrow_mut().appearance_changed = Some(callback);
    }

    fn on_hit_test_window_control(&self, callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
        self.shared.callbacks.borrow_mut().hit_test_window_control = Some(callback);
    }

    fn draw(&self, scene: &Scene) {
        self.shared.render(scene);
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.shared.atlas.clone()
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        false
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.shared
            .renderer
            .borrow()
            .as_ref()
            .and_then(|renderer| renderer.gpu_specs())
    }

    fn update_ime_position(&self, bounds: Bounds<Pixels>) {
        self.shared.ime_cursor.set(bounds);
    }

    fn set_virtual_keyboard_visible(&self, visible: bool) {
        // GPUI reports this on every focus change, and showing an already
        // visible input method resets a composing (Chinese) pre-edit.
        if self.shared.virtual_keyboard_visible.replace(visible) == visible {
            return;
        }
        // The request names its window: one input method client serves the whole
        // process, and which window it belongs to decides whether the system
        // accepts showing it at all.
        if visible {
            super::inputmethod::show(self.shared.id());
        } else {
            super::inputmethod::hide(self.shared.id());
        }
    }

    fn window_decorations(&self) -> Decorations {
        Decorations::Server
    }

    fn window_controls(&self) -> WindowControls {
        WindowControls {
            fullscreen: true,
            maximize: true,
            // Windows are self decorated, so the app has to draw the minimise
            // control itself; the host hides a sub window when it is pressed.
            minimize: true,
            window_menu: false,
        }
    }

    fn start_window_resize(&self, edge: ResizeEdge) {
        let value = match edge {
            ResizeEdge::Top => "top",
            ResizeEdge::TopRight => "top-right",
            ResizeEdge::Right => "right",
            ResizeEdge::BottomRight => "bottom-right",
            ResizeEdge::Bottom => "bottom",
            ResizeEdge::BottomLeft => "bottom-left",
            ResizeEdge::Left => "left",
            ResizeEdge::TopLeft => "top-left",
        };
        host::window_op_for(self.shared.id(), host::op::START_RESIZE, value);
    }

    fn request_decorations(&self, decorations: crate::WindowDecorations) {
        // Hiding the system title bar is what lets the app blend its own into the
        // window, but it also removes the system controls, so it only happens
        // when the app says it draws client side decorations itself.
        let value = match decorations {
            crate::WindowDecorations::Client => "client",
            crate::WindowDecorations::Server => "server",
        };
        host::window_op_for(self.shared.id(), host::op::SET_DECOR, value);
    }

    fn show_window_menu(&self, _position: Point<Pixels>) {}

    fn start_window_move(&self) {
        host::window_op_for(self.shared.id(), host::op::START_MOVE, "");
    }
}

/// Push one rectangle as two triangles.
#[allow(clippy::too_many_arguments)]
fn push_quad(
    vertices: &mut Vec<super::vk::QuadVertex>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    screen_width: f32,
    screen_height: f32,
    fill: [f32; 4],
    radii: [f32; 4],
    border: f32,
    border_color: [f32; 4],
) {
    let half_width = width / 2.0;
    let half_height = height / 2.0;
    let center_x = x + half_width;
    let center_y = y + half_height;
    let corners = [
        (x, y),
        (x + width, y),
        (x + width, y + height),
        (x, y + height),
    ];
    for index in [0usize, 1, 2, 0, 2, 3] {
        let (px, py) = corners[index];
        vertices.push(super::vk::QuadVertex {
            x: px / screen_width * 2.0 - 1.0,
            y: 1.0 - py / screen_height * 2.0,
            local_x: px - center_x,
            local_y: py - center_y,
            half_width,
            half_height,
            radius_tl: radii[0],
            radius_tr: radii[1],
            radius_br: radii[2],
            radius_bl: radii[3],
            border,
            pad: 0.0,
            r: fill[0],
            g: fill[1],
            b: fill[2],
            a: fill[3],
            border_r: border_color[0],
            border_g: border_color[1],
            border_b: border_color[2],
            border_a: border_color[3],
        });
    }
}

/// Clip rect for one primitive, in device pixels and clamped to the surface.
///
/// GPUI intersects nested content masks while building the scene and snaps the
/// result to whole device pixels, so one rectangle per primitive is exact.
/// Returns None when the primitive is clipped away entirely.
fn scissor_for(
    mask: &ContentMask<ScaledPixels>,
    width: u32,
    height: u32,
) -> Option<(i32, i32, u32, u32)> {
    let left = mask.bounds.origin.x.as_f32().floor().max(0.0);
    let top = mask.bounds.origin.y.as_f32().floor().max(0.0);
    let right = (mask.bounds.origin.x.as_f32() + mask.bounds.size.width.as_f32())
        .ceil()
        .min(width as f32);
    let bottom = (mask.bounds.origin.y.as_f32() + mask.bounds.size.height.as_f32())
        .ceil()
        .min(height as f32);
    let extent_x = (right - left).max(0.0) as u32;
    let extent_y = (bottom - top).max(0.0) as u32;
    if extent_x == 0 || extent_y == 0 {
        // Fully clipped: the caller skips the draw instead of emitting an empty
        // scissor, which Vulkan rejects.
        None
    } else {
        Some((left as i32, top as i32, extent_x, extent_y))
    }
}

/// Emits one quad, reporting whether anything was emitted.
fn push_quad_primitive(
    vertices: &mut Vec<super::vk::QuadVertex>,
    quad: &Quad,
    screen_width: f32,
    screen_height: f32,
) -> bool {
    let Some(fill) = quad.background.as_solid() else {
        // Gradients and patterns are not rendered yet.
        return false;
    };
    let x = quad.bounds.origin.x.as_f32();
    let y = quad.bounds.origin.y.as_f32();
    let quad_width = quad.bounds.size.width.as_f32();
    let quad_height = quad.bounds.size.height.as_f32();
    if quad_width <= 0.0 || quad_height <= 0.0 {
        return false;
    }
    let fill = fill.to_rgb();
    let border_color = quad.border_color.to_rgb();
    let border = quad
        .border_widths
        .top
        .as_f32()
        .max(quad.border_widths.right.as_f32())
        .max(quad.border_widths.bottom.as_f32())
        .max(quad.border_widths.left.as_f32())
        .max(0.0);
    let radii = [
        quad.corner_radii.top_left.as_f32(),
        quad.corner_radii.top_right.as_f32(),
        quad.corner_radii.bottom_right.as_f32(),
        quad.corner_radii.bottom_left.as_f32(),
    ];
    push_quad(
        vertices,
        x,
        y,
        quad_width,
        quad_height,
        screen_width,
        screen_height,
        [fill.r, fill.g, fill.b, fill.a],
        radii,
        border,
        [
            border_color.r,
            border_color.g,
            border_color.b,
            border_color.a,
        ],
    );
    true
}

/// Emits one underline. Wavy underlines are still drawn straight.
fn push_underline_primitive(
    vertices: &mut Vec<super::vk::QuadVertex>,
    underline: &Underline,
    screen_width: f32,
    screen_height: f32,
) -> bool {
    let bounds = underline.bounds;
    let x = bounds.origin.x.as_f32();
    let y = bounds.origin.y.as_f32();
    let underline_width = bounds.size.width.as_f32();
    let underline_height = bounds.size.height.as_f32();
    if underline_width <= 0.0 || underline_height <= 0.0 {
        return false;
    }
    let color = underline.color.to_rgb();
    push_quad(
        vertices,
        x,
        y,
        underline_width,
        underline_height,
        screen_width,
        screen_height,
        [color.r, color.g, color.b, color.a],
        [0.0; 4],
        0.0,
        [0.0; 4],
    );
    true
}

/// Emits one path. GPUI hands us a triangle list whose st coordinate encodes
/// quadratic curves, so compute each triangle screen-space st gradient here.
fn push_path_primitive(
    vertices: &mut Vec<super::vk::PathVertex>,
    path: &Path<ScaledPixels>,
    screen_width: f32,
    screen_height: f32,
) -> bool {
    let Some(fill) = path.color.as_solid() else {
        return false;
    };
    let fill = fill.to_rgb();
    let before = vertices.len();
    for triangle in path.vertices.chunks_exact(3) {
        let points: [(f32, f32); 3] = std::array::from_fn(|index| {
            (
                triangle[index].xy_position.x.as_f32(),
                triangle[index].xy_position.y.as_f32(),
            )
        });
        let st: [(f32, f32); 3] = std::array::from_fn(|index| {
            (triangle[index].st_position.x, triangle[index].st_position.y)
        });
        let dp1 = (points[1].0 - points[0].0, points[1].1 - points[0].1);
        let dp2 = (points[2].0 - points[0].0, points[2].1 - points[0].1);
        let ds1 = (st[1].0 - st[0].0, st[1].1 - st[0].1);
        let ds2 = (st[2].0 - st[0].0, st[2].1 - st[0].1);
        let determinant = dp1.0 * dp2.1 - dp2.0 * dp1.1;
        let (st_dx, st_dy) = if determinant.abs() < 1.0e-6 {
            ((0.0, 0.0), (0.0, 0.0))
        } else {
            let inverse = 1.0 / determinant;
            (
                (
                    (ds1.0 * dp2.1 - ds2.0 * dp1.1) * inverse,
                    (ds1.1 * dp2.1 - ds2.1 * dp1.1) * inverse,
                ),
                (
                    (ds2.0 * dp1.0 - ds1.0 * dp2.0) * inverse,
                    (ds2.1 * dp1.0 - ds1.1 * dp2.0) * inverse,
                ),
            )
        };
        for index in 0..3 {
            vertices.push(super::vk::PathVertex {
                x: points[index].0 / screen_width * 2.0 - 1.0,
                y: 1.0 - points[index].1 / screen_height * 2.0,
                st_x: st[index].0,
                st_y: st[index].1,
                st_dx_x: st_dx.0,
                st_dx_y: st_dx.1,
                st_dy_x: st_dy.0,
                st_dy_y: st_dy.1,
                r: fill.r,
                g: fill.g,
                b: fill.b,
                a: fill.a,
            });
        }
    }
    vertices.len() > before
}

/// Emits one monochrome sprite (a glyph).
fn push_glyph_primitive(
    vertices: &mut Vec<super::vk::GlyphVertex>,
    sprite: &MonochromeSprite,
    screen_width: f32,
    screen_height: f32,
    atlas_width: f32,
    atlas_height: f32,
) -> bool {
    let x = sprite.bounds.origin.x.as_f32();
    let y = sprite.bounds.origin.y.as_f32();
    let sprite_width = sprite.bounds.size.width.as_f32();
    let sprite_height = sprite.bounds.size.height.as_f32();
    if sprite_width <= 0.0 || sprite_height <= 0.0 {
        return false;
    }
    let tile = sprite.tile.bounds;
    let u0 = tile.origin.x.0 as f32 / atlas_width;
    let v0 = tile.origin.y.0 as f32 / atlas_height;
    let u1 = (tile.origin.x.0 + tile.size.width.0) as f32 / atlas_width;
    let v1 = (tile.origin.y.0 + tile.size.height.0) as f32 / atlas_height;
    let color = sprite.color.to_rgb();
    let x0 = x / screen_width * 2.0 - 1.0;
    let y0 = 1.0 - y / screen_height * 2.0;
    let x1 = (x + sprite_width) / screen_width * 2.0 - 1.0;
    let y1 = 1.0 - (y + sprite_height) / screen_height * 2.0;
    let vertex = |px: f32, py: f32, u: f32, v: f32| super::vk::GlyphVertex {
        x: px,
        y: py,
        u,
        v,
        r: color.r,
        g: color.g,
        b: color.b,
        a: color.a,
    };
    vertices.push(vertex(x0, y0, u0, v0));
    vertices.push(vertex(x1, y0, u1, v0));
    vertices.push(vertex(x1, y1, u1, v1));
    vertices.push(vertex(x0, y0, u0, v0));
    vertices.push(vertex(x1, y1, u1, v1));
    vertices.push(vertex(x0, y1, u0, v1));
    true
}

/// Emit one colour image quad. The tile already carries the pixels, so the
/// vertex only scales them by the sprite's opacity.
fn push_polychrome_primitive(
    vertices: &mut Vec<super::vk::GlyphVertex>,
    sprite: &PolychromeSprite,
    screen_width: f32,
    screen_height: f32,
    atlas_width: f32,
    atlas_height: f32,
) -> bool {
    let x = sprite.bounds.origin.x.as_f32();
    let y = sprite.bounds.origin.y.as_f32();
    let sprite_width = sprite.bounds.size.width.as_f32();
    let sprite_height = sprite.bounds.size.height.as_f32();
    if sprite_width <= 0.0 || sprite_height <= 0.0 {
        return false;
    }
    let tile = sprite.tile.bounds;
    let u0 = tile.origin.x.0 as f32 / atlas_width;
    let v0 = tile.origin.y.0 as f32 / atlas_height;
    let u1 = (tile.origin.x.0 + tile.size.width.0) as f32 / atlas_width;
    let v1 = (tile.origin.y.0 + tile.size.height.0) as f32 / atlas_height;
    let x0 = x / screen_width * 2.0 - 1.0;
    let y0 = 1.0 - y / screen_height * 2.0;
    let x1 = (x + sprite_width) / screen_width * 2.0 - 1.0;
    let y1 = 1.0 - (y + sprite_height) / screen_height * 2.0;
    let opacity = sprite.opacity.clamp(0.0, 1.0);
    let vertex = |px: f32, py: f32, u: f32, v: f32| super::vk::GlyphVertex {
        x: px,
        y: py,
        u,
        v,
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: opacity,
    };
    vertices.push(vertex(x0, y0, u0, v0));
    vertices.push(vertex(x1, y0, u1, v0));
    vertices.push(vertex(x1, y1, u1, v1));
    vertices.push(vertex(x0, y0, u0, v0));
    vertices.push(vertex(x1, y1, u1, v1));
    vertices.push(vertex(x0, y1, u0, v1));
    true
}

/// Builds the frame vertex arrays plus the ordered draw list.
///
/// Walking Scene::batches() instead of "all quads, then all paths, then all
/// glyphs" is what keeps painter order: a popup background is a quad while the
/// page text behind it is glyphs, so the old order drew the text on top of the
/// popup. Each emitted primitive carries its clip rect so the renderer can set
/// a scissor per run.
#[allow(clippy::type_complexity)]
pub(super) fn build_draw_list(
    scene: &Scene,
    width: u32,
    height: u32,
    atlas_width: u32,
    atlas_height: u32,
    sprite_width: u32,
    sprite_height: u32,
) -> (
    Vec<super::vk::QuadVertex>,
    Vec<super::vk::PathVertex>,
    Vec<super::vk::GlyphVertex>,
    Vec<super::vk::GlyphVertex>,
    Vec<DrawBatch>,
) {
    let screen_width = width as f32;
    let screen_height = height as f32;
    let atlas_width = atlas_width as f32;
    let atlas_height = atlas_height as f32;
    let sprite_width = sprite_width as f32;
    let sprite_height = sprite_height as f32;
    let mut quads = Vec::new();
    let mut paths = Vec::new();
    let mut glyphs = Vec::new();
    let mut sprites = Vec::new();
    let mut batches = Vec::new();
    let mut skipped = 0usize;
    let mut first_skipped: Option<(f32, f32, f32, f32)> = None;
    let mut unhandled = 0usize;
    let mut shadows = 0usize;
    let mut subpixel = 0usize;
    let mut polychrome = 0usize;
    let mut surfaces = 0usize;

    macro_rules! emit {
        ($list:expr, $pipeline:expr, $mask:expr, $body:expr) => {{
            // A primitive clipped away entirely still appears in the scene, and
            // emitting it with a whole-surface scissor would draw it unclipped.
            if let Some(scissor) = scissor_for($mask, width, height) {
                let first = $list.len();
                if $body {
                    batches.push(DrawBatch {
                        pipeline: $pipeline,
                        first_vertex: first as u32,
                        vertex_count: ($list.len() - first) as u32,
                        scissor,
                    });
                }
            } else {
                skipped += 1;
                if first_skipped.is_none() {
                    let bounds = $mask.bounds;
                    first_skipped = Some((
                        bounds.origin.x.as_f32(),
                        bounds.origin.y.as_f32(),
                        bounds.size.width.as_f32(),
                        bounds.size.height.as_f32(),
                    ));
                }
            }
        }};
    }

    for batch in scene.batches() {
        match batch {
            PrimitiveBatch::Quads(range) => {
                for quad in &scene.quads[range] {
                    emit!(
                        quads,
                        DrawPipeline::Quads,
                        &quad.content_mask,
                        push_quad_primitive(&mut quads, quad, screen_width, screen_height)
                    );
                }
            }
            PrimitiveBatch::Underlines(range) => {
                for underline in &scene.underlines[range] {
                    emit!(
                        quads,
                        DrawPipeline::Quads,
                        &underline.content_mask,
                        push_underline_primitive(
                            &mut quads,
                            underline,
                            screen_width,
                            screen_height
                        )
                    );
                }
            }
            PrimitiveBatch::Paths(range) => {
                for path in &scene.paths[range] {
                    emit!(
                        paths,
                        DrawPipeline::Paths,
                        &path.content_mask,
                        push_path_primitive(&mut paths, path, screen_width, screen_height)
                    );
                }
            }
            PrimitiveBatch::MonochromeSprites { range, .. } => {
                for sprite in &scene.monochrome_sprites[range] {
                    emit!(
                        glyphs,
                        DrawPipeline::Glyphs,
                        &sprite.content_mask,
                        push_glyph_primitive(
                            &mut glyphs,
                            sprite,
                            screen_width,
                            screen_height,
                            atlas_width,
                            atlas_height
                        )
                    );
                }
            }
            // Not rendered yet; skipping them must not disturb painter order.
            PrimitiveBatch::Shadows(range) => {
                shadows += range.len();
                unhandled += range.len();
            }
            PrimitiveBatch::SubpixelSprites { range, .. } => {
                subpixel += range.len();
                unhandled += range.len();
            }
            PrimitiveBatch::PolychromeSprites { range, .. } => {
                polychrome += range.len();
                for sprite in &scene.polychrome_sprites[range] {
                    emit!(
                        sprites,
                        DrawPipeline::Sprites,
                        &sprite.content_mask,
                        push_polychrome_primitive(
                            &mut sprites,
                            sprite,
                            screen_width,
                            screen_height,
                            sprite_width,
                            sprite_height
                        )
                    );
                }
            }
            PrimitiveBatch::Surfaces(range) => {
                surfaces += range.len();
                unhandled += range.len();
            }
        }
    }

    // A scene with primitives that produced no geometry is the signature of the
    // blank frames seen on device, so always report those; sample the rest.
    // Shadows are intentionally skipped; only the batch kinds that hold content
    // we do not draw yet (or a scene that produced no geometry) are anomalies.
    let empty_draw = (!scene.quads.is_empty() && quads.is_empty())
        || subpixel > 0
        || polychrome > 0
        || surfaces > 0;
    // Anomalies are reported once and then only sampled, so a scene that keeps
    // hitting them cannot log a line on every frame.
    static DRAW_SAMPLE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static ANOMALY_REPORTED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    let sample = DRAW_SAMPLE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first_anomaly =
        empty_draw && !ANOMALY_REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed);
    if first_anomaly || sample % 120 == 0 {
        super::vk::log(&format!(
            "[gpui_ohos] draw scene q={} p={} g={} u={} | emitted q={} p={} g={} | skipped={} unhandled={} (shadows={shadows} subpixel={subpixel} polychrome={polychrome} surfaces={surfaces}) first_skip={first_skipped:?}",
            scene.quads.len(),
            scene.paths.len(),
            scene.monochrome_sprites.len(),
            scene.underlines.len(),
            quads.len(),
            paths.len(),
            glyphs.len(),
            skipped,
            unhandled,
        ));
    }
    (quads, paths, glyphs, sprites, batches)
}

/// Cheap identity of the rendered scene, used to skip unchanged frames.
fn scene_hash(scene: &Scene) -> u64 {
    use std::hash::{Hash, Hasher};
    // FxHash rather than the default SipHash: the whole scene is hashed every
    // frame, and SipHash measured about 9 ms for a frame whose other work is
    // under 0.2 ms. This hash only has to notice changes, not resist an
    // adversary choosing collisions.
    let mut hasher = collections::FxHasher::default();
    scene.quads.len().hash(&mut hasher);
    for quad in &scene.quads {
        quad.bounds.origin.x.as_f32().to_bits().hash(&mut hasher);
        quad.bounds.origin.y.as_f32().to_bits().hash(&mut hasher);
        quad.bounds.size.width.as_f32().to_bits().hash(&mut hasher);
        quad.bounds.size.height.as_f32().to_bits().hash(&mut hasher);
        // A frame that only moves a mask, reorders layers, or changes a border or
        // a gradient still looks different, so all of it belongs in the identity.
        quad.order.hash(&mut hasher);
        quad.content_mask
            .bounds
            .origin
            .x
            .as_f32()
            .to_bits()
            .hash(&mut hasher);
        quad.content_mask
            .bounds
            .origin
            .y
            .as_f32()
            .to_bits()
            .hash(&mut hasher);
        quad.content_mask
            .bounds
            .size
            .width
            .as_f32()
            .to_bits()
            .hash(&mut hasher);
        quad.content_mask
            .bounds
            .size
            .height
            .as_f32()
            .to_bits()
            .hash(&mut hasher);
        for width in [
            quad.border_widths.top,
            quad.border_widths.right,
            quad.border_widths.bottom,
            quad.border_widths.left,
        ] {
            width.0.to_bits().hash(&mut hasher);
        }
        for radius in [
            quad.corner_radii.top_left,
            quad.corner_radii.top_right,
            quad.corner_radii.bottom_right,
            quad.corner_radii.bottom_left,
        ] {
            radius.as_f32().to_bits().hash(&mut hasher);
        }
        quad.background.as_solid().is_some().hash(&mut hasher);
        if let Some(color) = quad.background.as_solid() {
            let rgba = color.to_rgb();
            rgba.r.to_bits().hash(&mut hasher);
            rgba.g.to_bits().hash(&mut hasher);
            rgba.b.to_bits().hash(&mut hasher);
            rgba.a.to_bits().hash(&mut hasher);
        }
        // The border is drawn from this colour: changing only it used to leave
        // the previous frame on screen.
        let border = quad.border_color.to_rgb();
        border.r.to_bits().hash(&mut hasher);
        border.g.to_bits().hash(&mut hasher);
        border.b.to_bits().hash(&mut hasher);
        border.a.to_bits().hash(&mut hasher);
    }
    scene.paths.len().hash(&mut hasher);
    for path in &scene.paths {
        path.bounds.origin.x.as_f32().to_bits().hash(&mut hasher);
        path.bounds.origin.y.as_f32().to_bits().hash(&mut hasher);
        path.bounds.size.width.as_f32().to_bits().hash(&mut hasher);
        path.bounds.size.height.as_f32().to_bits().hash(&mut hasher);
        path.vertices.len().hash(&mut hasher);
        // Only the count is hashed, not the vertices themselves: measured on
        // device, hashing a long path's vertices costs about 9 ms per frame,
        // which is more than the whole 120 Hz budget. Covering them properly
        // needs the cheaper two-level fingerprint the fix plan describes; until
        // then a path that keeps its vertex count but moves them can be missed.
        if let Some(color) = path.color.as_solid() {
            let rgba = color.to_rgb();
            rgba.r.to_bits().hash(&mut hasher);
            rgba.g.to_bits().hash(&mut hasher);
            rgba.b.to_bits().hash(&mut hasher);
            rgba.a.to_bits().hash(&mut hasher);
        }
    }
    scene.underlines.len().hash(&mut hasher);
    for underline in &scene.underlines {
        underline
            .bounds
            .origin
            .x
            .as_f32()
            .to_bits()
            .hash(&mut hasher);
        underline
            .bounds
            .origin
            .y
            .as_f32()
            .to_bits()
            .hash(&mut hasher);
        underline
            .bounds
            .size
            .width
            .as_f32()
            .to_bits()
            .hash(&mut hasher);
        underline
            .bounds
            .size
            .height
            .as_f32()
            .to_bits()
            .hash(&mut hasher);
        let underline_rgb = underline.color.to_rgb();
        underline_rgb.r.to_bits().hash(&mut hasher);
        underline_rgb.g.to_bits().hash(&mut hasher);
        underline_rgb.b.to_bits().hash(&mut hasher);
        underline_rgb.a.to_bits().hash(&mut hasher);
    }
    scene.monochrome_sprites.len().hash(&mut hasher);
    for sprite in &scene.monochrome_sprites {
        sprite.bounds.origin.x.as_f32().to_bits().hash(&mut hasher);
        sprite.bounds.origin.y.as_f32().to_bits().hash(&mut hasher);
        sprite
            .bounds
            .size
            .width
            .as_f32()
            .to_bits()
            .hash(&mut hasher);
        sprite
            .bounds
            .size
            .height
            .as_f32()
            .to_bits()
            .hash(&mut hasher);
        sprite.tile.bounds.origin.x.0.hash(&mut hasher);
        sprite.tile.bounds.origin.y.0.hash(&mut hasher);
        sprite.tile.bounds.size.width.0.hash(&mut hasher);
        sprite.tile.bounds.size.height.0.hash(&mut hasher);
        let rgba = sprite.color.to_rgb();
        rgba.r.to_bits().hash(&mut hasher);
        rgba.g.to_bits().hash(&mut hasher);
        rgba.b.to_bits().hash(&mut hasher);
        rgba.a.to_bits().hash(&mut hasher);
    }
    // Polychrome sprites were missing from this hash altogether, so adding,
    // moving, fading or repainting an image never redrew the window: the frame
    // showing the previous one stayed on screen.
    scene.polychrome_sprites.len().hash(&mut hasher);
    for sprite in &scene.polychrome_sprites {
        sprite.bounds.origin.x.as_f32().to_bits().hash(&mut hasher);
        sprite.bounds.origin.y.as_f32().to_bits().hash(&mut hasher);
        sprite
            .bounds
            .size
            .width
            .as_f32()
            .to_bits()
            .hash(&mut hasher);
        sprite
            .bounds
            .size
            .height
            .as_f32()
            .to_bits()
            .hash(&mut hasher);
        sprite.tile.bounds.origin.x.0.hash(&mut hasher);
        sprite.tile.bounds.origin.y.0.hash(&mut hasher);
        sprite.tile.bounds.size.width.0.hash(&mut hasher);
        sprite.tile.bounds.size.height.0.hash(&mut hasher);
        sprite.tile.tile_id.0.hash(&mut hasher);
        sprite.opacity.to_bits().hash(&mut hasher);
        sprite.order.hash(&mut hasher);
    }
    hasher.finish()
}
