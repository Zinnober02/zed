use std::{
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
    AtlasTextureKind, Bounds, Capslock, Decorations, DevicePixels, DispatchEventResult, ForegroundExecutor,
    GpuSpecs, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    PlatformAtlas, PlatformDisplay, PlatformInput, PlatformInputHandler, PlatformWindow, Point,
    PromptButton, PromptLevel, RequestFrameOptions, ResizeEdge, Scene, ScrollDelta,
    ScrollWheelEvent, Size, TouchPhase, WindowAppearance, WindowBackgroundAppearance,
    WindowBounds, WindowControlArea, WindowControls, WindowDecorations, WindowParams, point, px,
};

use super::atlas::OhosAtlas;
use super::display::OhosDisplay;
use super::platform::SurfaceState;
use super::vk::VkRenderer;

/// Clear color used until the GPUI scene renderer lands (M2+).
/// Teal so it is unambiguous versus the old probe blue.
const CLEAR_COLOR: [f32; 4] = [0.0, 0.55, 0.45, 1.0];

/// Logical-to-device pixel scale. The 2in1 panel is 3120x2080 at a high DPI,
/// so a logical pixel maps to ~3 device pixels (otherwise 16px text is tiny).
pub(crate) const SCALE: f32 = 3.0;

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
    atlas: Arc<OhosAtlas>,
    framebuffer: RefCell<Vec<u8>>,
    last_scene_hash: Cell<Option<u64>>,
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
        let scale = SCALE;
        let bounds = Bounds::new(
            point(px(0.0), px(0.0)),
            Size {
                width: px(w as f32 / scale),
                height: px(h as f32 / scale),
            },
        );
        Rc::new(Self {
            surface,
            bounds: RefCell::new(bounds),
            scale: Cell::new(scale),
            display: OhosDisplay::new(w, h, scale),
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
            atlas: Arc::new(OhosAtlas::new()),
            framebuffer: RefCell::new(Vec::new()),
            last_scene_hash: Cell::new(None),
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

    /// Current editor text and caret (UTF-16), mirrored to the input method.
    pub(crate) fn ime_context(&self) -> Option<(String, usize)> {
        let mut guard = self.input_handler.borrow_mut();
        let handler = guard.as_mut()?;
        let mut adjusted = None;
        let text = handler.text_for_range(0..usize::MAX, &mut adjusted)?;
        let selection = handler.selected_text_range(true)?;
        Some((text, selection.range.end))
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
            return;
        };
        match command {
            ImeCommand::Commit(text) => {
                handler.replace_text_in_range(None, text);
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
            ImeCommand::Preview(text) => {
                handler.replace_and_mark_text_in_range(None, text, None);
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
        let hash = scene_hash(scene);
        if self.last_scene_hash.get() == Some(hash) {
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
        let rects = collect_clear_rects(scene);
        let uploads = self.atlas.take_uploads();
        let (atlas_w, atlas_h) = self.atlas.texture_size(AtlasTextureKind::Monochrome);
        if let Some(renderer) = self.renderer.borrow_mut().as_mut() {
            let (width, height) = renderer.size();
            let vertices = build_glyph_vertices(scene, width, height, atlas_w, atlas_h);
            if let Err(error) = renderer.render_scene(CLEAR_COLOR, &rects, &vertices, &uploads) {
                super::vk::log(&format!("[gpui_ohos] render failed: {error}"));
            }
        }
        self.last_scene_hash.set(Some(hash));
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
        self.shared.atlas.clone()
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

/// Solid quads become clear rectangles; glyph sprites become textured quads.
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
        let width = quad.bounds.size.width.as_f32();
        let height = quad.bounds.size.height.as_f32();
        if width <= 0.0 || height <= 0.0 {
            continue;
        }
        rects.push(super::vk::ClearRect {
            x: x as i32,
            y: y as i32,
            width: width as u32,
            height: height as u32,
            color: [rgba.r, rgba.g, rgba.b, 1.0],
        });
    }
    rects
}

fn build_glyph_vertices(
    scene: &Scene,
    width: u32,
    height: u32,
    atlas_width: u32,
    atlas_height: u32,
) -> Vec<super::vk::GlyphVertex> {
    let mut vertices = Vec::with_capacity(scene.monochrome_sprites.len() * 6);
    let screen_width = width as f32;
    let screen_height = height as f32;
    let atlas_width = atlas_width as f32;
    let atlas_height = atlas_height as f32;
    for sprite in &scene.monochrome_sprites {
        let x = sprite.bounds.origin.x.as_f32();
        let y = sprite.bounds.origin.y.as_f32();
        let sprite_width = sprite.bounds.size.width.as_f32();
        let sprite_height = sprite.bounds.size.height.as_f32();
        if sprite_width <= 0.0 || sprite_height <= 0.0 {
            continue;
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
    }
    vertices
}

/// Cheap identity of the rendered scene, used to skip unchanged frames.
fn scene_hash(scene: &Scene) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    scene.quads.len().hash(&mut hasher);
    for quad in &scene.quads {
        quad.bounds.origin.x.as_f32().to_bits().hash(&mut hasher);
        quad.bounds.origin.y.as_f32().to_bits().hash(&mut hasher);
        quad.bounds.size.width.as_f32().to_bits().hash(&mut hasher);
        quad.bounds.size.height.as_f32().to_bits().hash(&mut hasher);
        if let Some(color) = quad.background.as_solid() {
            let rgba = color.to_rgb();
            rgba.r.to_bits().hash(&mut hasher);
            rgba.g.to_bits().hash(&mut hasher);
            rgba.b.to_bits().hash(&mut hasher);
            rgba.a.to_bits().hash(&mut hasher);
        }
    }
    scene.monochrome_sprites.len().hash(&mut hasher);
    for sprite in &scene.monochrome_sprites {
        sprite.bounds.origin.x.as_f32().to_bits().hash(&mut hasher);
        sprite.bounds.origin.y.as_f32().to_bits().hash(&mut hasher);
        sprite.bounds.size.width.as_f32().to_bits().hash(&mut hasher);
        sprite.bounds.size.height.as_f32().to_bits().hash(&mut hasher);
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
    hasher.finish()
}

fn fill_rect(
    fb: &mut [u8],
    w: u32,
    h: u32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    rgba: [u8; 4],
    bgra: bool,
) {
    let x0 = x0.max(0);
    let y0 = y0.max(0);
    let x1 = x1.min(w as i32);
    let y1 = y1.min(h as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let px = if bgra {
        [rgba[2], rgba[1], rgba[0], rgba[3]]
    } else {
        rgba
    };
    let stride = w as usize * 4;
    for y in y0..y1 {
        let start = y as usize * stride + x0 as usize * 4;
        let end = y as usize * stride + x1 as usize * 4;
        for chunk in fb[start..end].chunks_exact_mut(4) {
            chunk.copy_from_slice(&px);
        }
    }
}

/// CPU-composite the scene (solid quads then monochrome glyph sprites).
fn composite(scene: &Scene, fb: &mut [u8], w: u32, h: u32, atlas: &OhosAtlas, bgra: bool) {
    fill_rect(
        fb,
        w,
        h,
        0,
        0,
        w as i32,
        h as i32,
        [0x1b, 0x1b, 0x1b, 0xff],
        bgra,
    );

    for quad in &scene.quads {
        let Some(color) = quad.background.as_solid() else {
            continue;
        };
        let rgba = color.to_rgb();
        let a = (rgba.a * 255.0) as u8;
        if a == 0 {
            continue;
        }
        let x0 = quad.bounds.origin.x.as_f32() as i32;
        let y0 = quad.bounds.origin.y.as_f32() as i32;
        let x1 = x0 + quad.bounds.size.width.as_f32() as i32;
        let y1 = y0 + quad.bounds.size.height.as_f32() as i32;
        fill_rect(
            fb,
            w,
            h,
            x0,
            y0,
            x1,
            y1,
            [
                (rgba.r * 255.0) as u8,
                (rgba.g * 255.0) as u8,
                (rgba.b * 255.0) as u8,
                a,
            ],
            bgra,
        );
    }

    if scene.monochrome_sprites.is_empty() {
        return;
    }

    atlas.with_texture(AtlasTextureKind::Monochrome, |data, atlas_w, _atlas_h| {
        let stride = w as usize * 4;
        for sprite in &scene.monochrome_sprites {
            let sx = sprite.bounds.origin.x.as_f32() as i32;
            let sy = sprite.bounds.origin.y.as_f32() as i32;
            let sw = sprite.bounds.size.width.as_f32() as i32;
            let sh = sprite.bounds.size.height.as_f32() as i32;
            if sw <= 0 || sh <= 0 {
                continue;
            }
            let tx = sprite.tile.bounds.origin.x.0;
            let ty = sprite.tile.bounds.origin.y.0;
            let color = sprite.color.to_rgb();
            let cr = (color.r * 255.0) as u32;
            let cg = (color.g * 255.0) as u32;
            let cb = (color.b * 255.0) as u32;
            let ca = (color.a * 255.0) as u32;
            for row in 0..sh {
                let dy = sy + row;
                if dy < 0 || dy >= h as i32 {
                    continue;
                }
                for col in 0..sw {
                    let dx = sx + col;
                    if dx < 0 || dx >= w as i32 {
                        continue;
                    }
                    let ax = tx + col;
                    let ay = ty + row;
                    if ax < 0 || ay < 0 {
                        continue;
                    }
                    let Some(&coverage) = data.get(ay as usize * atlas_w as usize + ax as usize)
                    else {
                        continue;
                    };
                    if coverage == 0 {
                        continue;
                    }
                    let cov = coverage as u32;
                    let inv = 255 - cov;
                    let i = dy as usize * stride + dx as usize * 4;
                    let dr = fb[i] as u32;
                    let dg = fb[i + 1] as u32;
                    let db = fb[i + 2] as u32;
                    let r = (cr * cov * ca / 65025 + dr * inv / 255) as u8;
                    let g = (cg * cov * ca / 65025 + dg * inv / 255) as u8;
                    let b = (cb * cov * ca / 65025 + db * inv / 255) as u8;
                    if bgra {
                        fb[i] = b;
                        fb[i + 1] = g;
                        fb[i + 2] = r;
                    } else {
                        fb[i] = r;
                        fb[i + 1] = g;
                        fb[i + 2] = b;
                    }
                    fb[i + 3] = 255;
                }
            }
        }
    });
}

