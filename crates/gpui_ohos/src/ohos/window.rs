use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    ffi::c_void,
    ptr::NonNull,
    rc::Rc,
    sync::Arc,
};

use anyhow::Result;
use futures::channel::oneshot;
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, OhosNdkWindowHandle,
    RawWindowHandle, WindowHandle,
};

use crate::{
    AtlasKey, AtlasTile, Bounds, Capslock, Decorations, DevicePixels, DispatchEventResult, ForegroundExecutor,
    GpuSpecs, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    PlatformAtlas, PlatformDisplay, PlatformInput, PlatformInputHandler, PlatformWindow, Point,
    PromptButton, PromptLevel, RequestFrameOptions, ResizeEdge, Scene, ScrollDelta,
    ScrollWheelEvent, Size, TouchPhase, WindowAppearance, WindowBackgroundAppearance,
    WindowBounds, WindowControlArea, WindowControls, WindowDecorations, WindowParams, point, px,
};

use super::display::OhosDisplay;
use super::platform::SurfaceState;
use super::vk::VkRenderer;

/// Clear color used until the GPUI scene renderer lands (M2+).
/// Teal so it is unambiguous versus the old probe blue.
const CLEAR_COLOR: [f32; 4] = [0.0, 0.55, 0.45, 1.0];

/// Placeholder atlas. Returns None for every request; good enough while the
/// scene renderer is not implemented yet.
struct EmptyAtlas;

impl PlatformAtlas for EmptyAtlas {
    fn get_or_insert_with<'a>(
        &self,
        _key: &AtlasKey,
        _build: &mut dyn FnMut() -> Result<Option<(Size<DevicePixels>, Cow<'a, [u8]>)>>,
    ) -> Result<Option<AtlasTile>> {
        Ok(None)
    }

    fn remove(&self, _key: &AtlasKey) {}
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
    surface: Rc<RefCell<SurfaceState>>,
    bounds: RefCell<Bounds<Pixels>>,
    scale: Cell<f32>,
    display: OhosDisplay,
    input_handler: RefCell<Option<PlatformInputHandler>>,
    callbacks: RefCell<WindowCallbacks>,
    renderer: RefCell<Option<VkRenderer>>,
    pointer_position: Cell<Point<Pixels>>,
    pressed_button: Cell<Option<MouseButton>>,
    modifiers: Cell<Modifiers>,
    active: Cell<bool>,
    hovered: Cell<bool>,
    render_failure_logged: Cell<bool>,
    frame_count: Cell<u64>,
    #[allow(dead_code)]
    foreground_executor: ForegroundExecutor,
}

impl WindowShared {
    pub(crate) fn new(
        surface: Rc<RefCell<SurfaceState>>,
        params: WindowParams,
        foreground_executor: ForegroundExecutor,
    ) -> Rc<Self> {
        let (w, h) = {
            let s = surface.borrow();
            (s.width, s.height)
        };
        Rc::new(Self {
            surface,
            bounds: RefCell::new(params.bounds),
            scale: Cell::new(1.0),
            display: OhosDisplay::new(w, h, 1.0),
            input_handler: RefCell::new(None),
            callbacks: RefCell::new(WindowCallbacks::default()),
            renderer: RefCell::new(None),
            pointer_position: Cell::new(point(px(0.0), px(0.0))),
            pressed_button: Cell::new(None),
            modifiers: Cell::new(Modifiers::default()),
            active: Cell::new(true),
            hovered: Cell::new(true),
            render_failure_logged: Cell::new(false),
            frame_count: Cell::new(0),
            foreground_executor,
        })
    }

    pub(crate) fn request_frame(&self) {
        let mut callback = self.callbacks.borrow_mut().request_frame.take();
        if let Some(cb) = callback.as_mut() {
            cb(RequestFrameOptions {
                require_presentation: true,
                force_render: false,
            });
        }
        self.callbacks.borrow_mut().request_frame = callback;
    }

    pub(crate) fn dispatch_input(&self, input: PlatformInput) -> DispatchEventResult {
        let mut callback = self.callbacks.borrow_mut().input.take();
        let mut result = DispatchEventResult::default();
        if let Some(cb) = callback.as_mut() {
            result = cb(input);
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
        if self.renderer.borrow().is_some() {
            return true;
        }
        let (window, width, height, valid) = {
            let s = self.surface.borrow();
            (s.window, s.width, s.height, s.valid)
        };
        if !valid || window.is_null() {
            return false;
        }
        match VkRenderer::new(window, width, height) {
            Ok(renderer) => {
                super::vk::log(&format!(
                    "[gpui_ohos] Vulkan renderer ready: {}x{}",
                    renderer.size().0,
                    renderer.size().1
                ));
                *self.renderer.borrow_mut() = Some(renderer);
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
        if !self.ensure_renderer() {
            return;
        }
        let frame = self.frame_count.get() + 1;
        self.frame_count.set(frame);
        if frame % 120 == 1 {
            super::vk::log(&format!(
                "[gpui_ohos] frame {frame} quads={}",
                scene.quads.len()
            ));
        }
        let rects = collect_clear_rects(scene);
        if let Some(renderer) = self.renderer.borrow_mut().as_mut() {
            if let Err(error) = renderer.render_frame(CLEAR_COLOR, &rects) {
                super::vk::log(&format!("[gpui_ohos] render failed: {error}"));
            }
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

    pub(crate) fn set_modifiers(&self, modifiers: Modifiers) {
        self.modifiers.set(modifiers);
        self.dispatch_input(PlatformInput::ModifiersChanged(
            crate::ModifiersChangedEvent {
                modifiers,
                capslock: Capslock::default(),
            },
        ));
    }
}

pub(crate) struct OhosWindow {
    shared: Rc<WindowShared>,
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
        WindowAppearance::Light
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
        let mut callback = self.shared.callbacks.borrow_mut().active_status_change.take();
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

    fn set_title(&mut self, _title: &str) {}

    fn set_background_appearance(&self, _appearance: WindowBackgroundAppearance) {}

    fn minimize(&self) {}

    fn zoom(&self) {}

    fn toggle_fullscreen(&self) {}

    fn is_fullscreen(&self) -> bool {
        true
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.shared.callbacks.borrow_mut().request_frame = Some(callback);
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

    fn on_hit_test_window_control(
        &self,
        callback: Box<dyn FnMut() -> Option<WindowControlArea>>,
    ) {
        self.shared.callbacks.borrow_mut().hit_test_window_control = Some(callback);
    }

    fn draw(&self, scene: &Scene) {
        self.shared.render(scene);
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        Arc::new(EmptyAtlas)
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        false
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        None
    }

    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {}

    fn window_decorations(&self) -> Decorations {
        Decorations::Server
    }

    fn window_controls(&self) -> WindowControls {
        WindowControls {
            fullscreen: false,
            maximize: false,
            minimize: false,
            window_menu: false,
        }
    }

    fn start_window_resize(&self, _edge: ResizeEdge) {}

    fn request_decorations(&self, _decorations: crate::WindowDecorations) {}

    fn show_window_menu(&self, _position: Point<Pixels>) {}

    fn start_window_move(&self) {}
}

/// Translate GPUI scene quads into solid clear rectangles, in paint order.
/// M1 handles solid backgrounds only; borders, gradients, paths and glyphs
/// are layered on in later milestones.
fn collect_clear_rects(scene: &Scene) -> Vec<super::vk::ClearRect> {
    let mut rects = Vec::with_capacity(scene.quads.len());
    for quad in &scene.quads {
        let Some(color) = quad.background.as_solid() else {
            continue;
        };
        let rgba = color.to_rgb();
        if rgba.a < 0.996 {
            continue;
        }
        let x = quad.bounds.origin.x.as_f32();
        let y = quad.bounds.origin.y.as_f32();
        let w = quad.bounds.size.width.as_f32();
        let h = quad.bounds.size.height.as_f32();
        if w <= 0.0 || h <= 0.0 {
            continue;
        }
        rects.push(super::vk::ClearRect {
            x: x as i32,
            y: y as i32,
            width: w as u32,
            height: h as u32,
            color: [rgba.r, rgba.g, rgba.b, 1.0],
        });
    }
    rects
}
