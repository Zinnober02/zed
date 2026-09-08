use std::fmt::Debug;

use uuid::Uuid;

use crate::{Bounds, DisplayId, Pixels, PlatformDisplay, Result, point, px, size};

/// The OHOS display.
///
/// The system-reported default display is unreliable on this 2in1 device (it
/// reports a portrait 1260x2719 panel while the real panel is 3120x2080), so we
/// derive the display bounds from the XComponent surface size instead.
#[derive(Clone)]
pub(crate) struct OhosDisplay {
    id: DisplayId,
    bounds: Bounds<Pixels>,
}

impl OhosDisplay {
    pub(crate) fn new(width_px: u32, height_px: u32, scale: f32) -> Self {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let bounds = if width_px > 0 && height_px > 0 {
            Bounds::new(
                point(px(0.0), px(0.0)),
                size(px(width_px as f32 / scale), px(height_px as f32 / scale)),
            )
        } else {
            Bounds::new(point(px(0.0), px(0.0)), size(px(3120.0), px(2080.0)))
        };
        Self {
            id: DisplayId::new(0),
            bounds,
        }
    }
}

impl Debug for OhosDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OhosDisplay")
            .field("id", &self.id)
            .field("bounds", &self.bounds)
            .finish()
    }
}

impl PlatformDisplay for OhosDisplay {
    fn id(&self) -> DisplayId {
        self.id
    }

    fn uuid(&self) -> Result<Uuid> {
        Ok(Uuid::from_bytes([
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x01,
        ]))
    }

    fn bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    fn visible_bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }
}
