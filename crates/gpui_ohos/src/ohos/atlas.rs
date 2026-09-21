//! CPU-side glyph atlas for the OHOS backend.
//!
//! GPUI asks the window's PlatformAtlas to place rasterized glyphs and returns
//! an AtlasTile; the renderer later uploads the dirty regions into a Vulkan
//! texture and draws monochrome sprites from it.

use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
};

use anyhow::Result;
use collections::HashMap;
use etagere::{AllocId, BucketedAtlasAllocator, size2};

use crate::{
    AtlasKey, AtlasTextureId, AtlasTextureKind, AtlasTile, Bounds, DevicePixels, PlatformAtlas,
    Size, TileId, point, size,
};

/// Side every atlas texture starts at.
///
/// A window that shows text only rarely needs more, and 2048 square costs 4 MiB
/// as an R8 coverage mask and 16 MiB as RGBA, which a window can afford before
/// anything has been drawn.
const ATLAS_START_DIM: u32 = 2048;

/// Largest side an atlas may grow to, whatever the device allows.
///
/// Growth doubles, so the sides walk 2048, 4096, 8192; a full RGBA8 8192 square
/// is 256 MiB, and one more doubling buys space for pictures that a viewer shows
/// scaled down anyway at four times that memory. The device's own limit is
/// usually 8192 or above, so this is the bound that usually binds.
const ATLAS_MAX_DIM: u32 = 8192;

fn bytes_per_pixel(kind: AtlasTextureKind) -> usize {
    match kind {
        AtlasTextureKind::Monochrome => 1,
        AtlasTextureKind::Polychrome | AtlasTextureKind::Subpixel => 4,
    }
}

/// Whether a picture of `size` is longer than the largest an atlas can become.
fn exceeds_limit(size: Size<DevicePixels>, limit: u32) -> bool {
    size.width.0 > limit as i32 || size.height.0 > limit as i32
}

/// Bytes a four-channel picture of `size` occupies.
fn image_byte_len(size: Size<DevicePixels>) -> usize {
    size.width.0 as usize * size.height.0 as usize * 4
}

/// Shrink a BGRA picture so that its longest side is `longest`, keeping the
/// aspect ratio.
///
/// gpui asks the atlas for a picture at the picture's own pixel size and has no
/// way to name a smaller one, so a picture longer than the largest side the atlas
/// can reach would never be placed and none of it would be drawn. The tile that
/// comes back still covers the same target rectangle, because the frame maps a
/// tile's rectangle by proportion, so only the resolution drops.
///
/// Each destination pixel samples the four source pixels around the centre of the
/// source area it covers, which is bilinear interpolation.
fn scale_bgra_down(
    source_size: Size<DevicePixels>,
    source: &[u8],
    longest: u32,
) -> Option<(Size<DevicePixels>, Vec<u8>)> {
    let source_width = source_size.width.0 as usize;
    let source_height = source_size.height.0 as usize;
    if source.len() < image_byte_len(source_size) {
        // The picture and the size it was handed over with disagree; leaving it
        // alone gives the allocator the mismatch to report.
        return None;
    }
    let (target_width, target_height) = if source_width >= source_height {
        (
            longest,
            (longest as u64 * source_height as u64 / source_width as u64).max(1) as u32,
        )
    } else {
        (
            (longest as u64 * source_width as u64 / source_height as u64).max(1) as u32,
            longest,
        )
    };
    crate::log_line(&format!(
        "atlas scaled {}x{} to {target_width}x{target_height}",
        source_size.width.0, source_size.height.0
    ));

    let mut target = vec![0u8; target_width as usize * target_height as usize * 4];
    for y in 0..target_height as usize {
        // Sampling at the centre of the covered source area keeps the picture from
        // sliding; clamping holds the outermost row and column in range.
        let source_y = ((y as f32 + 0.5) * source_height as f32 / target_height as f32 - 0.5)
            .clamp(0.0, source_height as f32 - 1.0);
        let y0 = source_y.floor() as usize;
        let y1 = (y0 + 1).min(source_height - 1);
        let fy = source_y - y0 as f32;
        for x in 0..target_width as usize {
            let source_x = ((x as f32 + 0.5) * source_width as f32 / target_width as f32 - 0.5)
                .clamp(0.0, source_width as f32 - 1.0);
            let x0 = source_x.floor() as usize;
            let x1 = (x0 + 1).min(source_width - 1);
            let fx = source_x - x0 as f32;
            let top_left = (y0 * source_width + x0) * 4;
            let top_right = (y0 * source_width + x1) * 4;
            let bottom_left = (y1 * source_width + x0) * 4;
            let bottom_right = (y1 * source_width + x1) * 4;
            let destination = (y * target_width as usize + x) * 4;
            for channel in 0..4 {
                let top = source[top_left + channel] as f32 * (1.0 - fx)
                    + source[top_right + channel] as f32 * fx;
                let bottom = source[bottom_left + channel] as f32 * (1.0 - fx)
                    + source[bottom_right + channel] as f32 * fx;
                target[destination + channel] = (top * (1.0 - fy) + bottom * fy + 0.5) as u8;
            }
        }
    }
    Some((
        size(
            DevicePixels(target_width as i32),
            DevicePixels(target_height as i32),
        ),
        target,
    ))
}

pub(crate) struct AtlasTexture {
    /// Part of the atlas identity; only one texture kind is used today.
    #[allow(dead_code)]
    pub kind: AtlasTextureKind,
    pub width: u32,
    pub height: u32,
    pub cpu: Vec<u8>,
}

/// A dirty region to upload into the GPU atlas texture.
pub(crate) struct AtlasUpload {
    /// See AtlasTexture::kind.
    #[allow(dead_code)]
    pub kind: AtlasTextureKind,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

struct AtlasState {
    textures: Vec<AtlasTexture>,
    allocators: Vec<BucketedAtlasAllocator>,
    tiles_by_key: HashMap<AtlasKey, AtlasTile>,
    alloc_ids: HashMap<AtlasKey, AllocId>,
    dirty: Vec<(AtlasTextureKind, Bounds<DevicePixels>)>,
    /// Counts presented frames, one per `take_uploads`. A tile marked with this
    /// number was painted in the frame now being built, so its space is in use.
    frame: u64,
    /// The frame each tile was last painted in. A picture keeps its tile for as
    /// long as it is on screen, and a picture that has left the screen is what
    /// makes room for the next one: without that, opening a few photographs in a
    /// row fills the atlas and the ones after them cannot be drawn at all.
    used: HashMap<AtlasKey, u64>,
}

impl AtlasState {
    /// Double one kind's texture until a region of `needed_size` fits, or the
    /// limit is reached.
    ///
    /// The rectangles the allocator already handed out keep their coordinates, so
    /// every tile stays where it was and the pixels only have to move to the wider
    /// stride. What the texture already held is reported dirty afterwards, because
    /// the renderer recreates its own texture at the new size and has to be given
    /// those pixels again; the space the growth added is empty.
    ///
    /// Returns whether the texture grew.
    ///
    /// `force` doubles it even when the region already fits its size: a region can
    /// be small enough for the atlas and still have no free rectangle left, and the
    /// only way past that, once the tiles that are not on screen have been given
    /// back, is more room.
    fn grow_texture(
        &mut self,
        kind_idx: usize,
        needed_size: Size<DevicePixels>,
        limit: u32,
        force: bool,
    ) -> bool {
        let kind = self.textures[kind_idx].kind;
        let old_width = self.textures[kind_idx].width;
        let old_height = self.textures[kind_idx].height;
        let current = old_width.max(old_height);
        if current >= limit {
            return false;
        }
        let needed_dim = needed_size.width.0.max(needed_size.height.0).max(0) as u32;
        let target = if force {
            needed_dim.max(current.saturating_mul(2))
        } else {
            needed_dim
        };
        let mut grown = current;
        while grown < target && grown < limit {
            grown = (grown * 2).min(limit);
        }
        if grown <= current {
            return false;
        }
        crate::log_line(&format!(
            "atlas {kind:?} grew from {current} to {grown} for a {}x{} region",
            needed_size.width.0, needed_size.height.0
        ));

        let bpp = bytes_per_pixel(kind);
        let old_stride = old_width as usize * bpp;
        let new_stride = grown as usize * bpp;
        let mut cpu = vec![0u8; new_stride * grown as usize];
        for row in 0..old_height as usize {
            let source = row * old_stride;
            let destination = row * new_stride;
            cpu[destination..destination + old_stride]
                .copy_from_slice(&self.textures[kind_idx].cpu[source..source + old_stride]);
        }
        let texture = &mut self.textures[kind_idx];
        texture.cpu = cpu;
        texture.width = grown;
        texture.height = grown;
        self.allocators[kind_idx].grow(size2(grown as i32, grown as i32));
        self.dirty.push((
            kind,
            Bounds {
                origin: point(DevicePixels(0), DevicePixels(0)),
                size: size(
                    DevicePixels(old_width as i32),
                    DevicePixels(old_height as i32),
                ),
            },
        ));
        true
    }

    /// Give back the tile of this kind that has gone longest without being
    /// painted, and answer whether there was one.
    ///
    /// Tiles painted in the frame being built keep their space: their rectangles
    /// are already part of that frame's geometry, and handing one of them to a new
    /// picture would draw the new pixels where the old picture was asked for. Every
    /// other tile in this kind is a picture that is no longer on screen, so giving
    /// its space back costs nothing but re-decoding it if it comes back.
    fn evict_oldest(&mut self, kind_idx: usize, frame: u64) -> bool {
        let kind = self.textures[kind_idx].kind;
        let oldest = self
            .used
            .iter()
            .filter(|(key, last_used)| key.texture_kind() == kind && **last_used < frame)
            .min_by_key(|(_, last_used)| **last_used)
            .map(|(key, last_used)| (key.clone(), *last_used));
        let Some((key, last_used)) = oldest else {
            return false;
        };
        if let Some(id) = self.alloc_ids.remove(&key) {
            self.allocators[kind_idx].deallocate(id);
        }
        self.tiles_by_key.remove(&key);
        self.used.remove(&key);
        crate::log_line(&format!(
            "atlas {kind:?} gave back the tile last painted in frame {last_used}"
        ));
        true
    }
}

pub(crate) struct OhosAtlas {
    state: RefCell<AtlasState>,
    /// Largest side the device can hold in one 2D image, as its properties report
    /// it. Filled in once a renderer exists; until then ATLAS_MAX_DIM stands in,
    /// which is what the device reports anyway.
    device_limit: Cell<u32>,
}

impl OhosAtlas {
    pub(crate) fn new() -> Self {
        let kinds = [
            AtlasTextureKind::Monochrome,
            AtlasTextureKind::Polychrome,
            AtlasTextureKind::Subpixel,
        ];
        let mut textures = Vec::new();
        let mut allocators = Vec::new();
        for kind in kinds {
            let bpp = bytes_per_pixel(kind);
            textures.push(AtlasTexture {
                kind,
                width: ATLAS_START_DIM,
                height: ATLAS_START_DIM,
                cpu: vec![0; ATLAS_START_DIM as usize * ATLAS_START_DIM as usize * bpp],
            });
            allocators.push(BucketedAtlasAllocator::new(size2(
                ATLAS_START_DIM as i32,
                ATLAS_START_DIM as i32,
            )));
        }
        Self {
            state: RefCell::new(AtlasState {
                textures,
                allocators,
                tiles_by_key: HashMap::default(),
                alloc_ids: HashMap::default(),
                dirty: Vec::new(),
                frame: 1,
                used: HashMap::default(),
            }),
            device_limit: Cell::new(ATLAS_MAX_DIM),
        }
    }

    /// Record what the device allows in one image, from the renderer that was just
    /// created for this window's surface. Painting happens before the first
    /// renderer exists, which is why this is not a constructor argument.
    pub(crate) fn set_device_limit(&self, max_image_dimension_2d: u32) {
        if max_image_dimension_2d == 0 {
            // The properties could not be read; the conservative default stays.
            return;
        }
        if self.device_limit.get() != max_image_dimension_2d {
            // The cap on every atlas in this window, and the number to look at when
            // a picture is left out for want of room.
            crate::log_line(&format!(
                "[gpui_ohos] the device allows images up to {max_image_dimension_2d}"
            ));
        }
        self.device_limit.set(max_image_dimension_2d);
    }

    /// Largest side an atlas may grow to.
    fn growth_limit(&self) -> u32 {
        self.device_limit
            .get()
            .min(ATLAS_MAX_DIM)
            .max(ATLAS_START_DIM)
    }

    /// Size of the CPU texture backing a kind. Each kind grows on its own, so a
    /// window showing pictures does not enlarge the coverage mask they never use.
    pub(crate) fn texture_size(&self, kind: AtlasTextureKind) -> (u32, u32) {
        let state = self.state.borrow();
        let texture = &state.textures[kind as usize];
        (texture.width, texture.height)
    }

    /// Whether any dirty region is still waiting to be uploaded.
    ///
    /// Separate from `take_uploads` because the window asks this before deciding
    /// that an unchanged scene can be skipped: when a decoded image reuses an
    /// existing tile the scene stays byte-identical while its pixels change, and
    /// the dirty flag is the only signal that a redraw is needed.
    pub(crate) fn has_pending_uploads(&self) -> bool {
        !self.state.borrow().dirty.is_empty()
    }

    /// Drain dirty regions, cloning their pixel data for upload.
    pub(crate) fn take_uploads(&self) -> Vec<AtlasUpload> {
        let mut state = self.state.borrow_mut();
        // Drained once per frame attempted, which is what makes "painted in the
        // frame being built" a fact the eviction can rely on.
        state.frame += 1;
        let dirty = std::mem::take(&mut state.dirty);
        let mut uploads = Vec::with_capacity(dirty.len());
        for (kind, bounds) in dirty {
            let texture = &state.textures[kind as usize];
            let bpp = bytes_per_pixel(kind);
            let x = bounds.origin.x.0.max(0) as u32;
            let y = bounds.origin.y.0.max(0) as u32;
            let width = bounds.size.width.0.max(0) as u32;
            let height = bounds.size.height.0.max(0) as u32;
            if width == 0 || height == 0 {
                continue;
            }
            let stride = texture.width as usize * bpp;
            let mut data = Vec::with_capacity(width as usize * height as usize * bpp);
            for row in 0..height as usize {
                let start = (y as usize + row) * stride + x as usize * bpp;
                let end = start + width as usize * bpp;
                data.extend_from_slice(&texture.cpu[start..end]);
            }
            uploads.push(AtlasUpload {
                kind,
                x,
                y,
                width,
                height,
                data,
            });
        }
        uploads
    }
}

impl PlatformAtlas for OhosAtlas {
    fn get_or_insert_with<'a>(
        &self,
        key: &AtlasKey,
        build: &mut dyn FnMut() -> Result<Option<(crate::Size<DevicePixels>, Cow<'a, [u8]>)>>,
    ) -> Result<Option<AtlasTile>> {
        let mut state = self.state.borrow_mut();
        if let Some(tile) = state.tiles_by_key.get(key) {
            let tile = *tile;
            let frame = state.frame;
            state.used.insert(key.clone(), frame);
            return Ok(Some(tile));
        }

        let Some((glyph_size, bytes)) = build()? else {
            return Ok(None);
        };
        if glyph_size.width.0 <= 0 || glyph_size.height.0 <= 0 {
            return Ok(None);
        }

        let kind = key.texture_kind();
        let kind_idx = kind as usize;
        let limit = self.growth_limit();

        // Only a picture can arrive longer than the largest an atlas may become; a
        // glyph is rasterised at the size it will be drawn. Shrinking is the last
        // resort, because it costs resolution, so the atlas grows first and this
        // catches only what no atlas may hold.
        let scaled = match key {
            AtlasKey::Image(_) if exceeds_limit(glyph_size, limit) => {
                scale_bgra_down(glyph_size, &bytes, limit)
            }
            _ => None,
        };
        let (glyph_size, bytes): (Size<DevicePixels>, &[u8]) = match &scaled {
            Some((size, pixels)) => (*size, pixels.as_slice()),
            None => (glyph_size, bytes.as_ref()),
        };

        // Place the region, in the order that costs least: a free rectangle, then
        // more room for a region that does not fit the current size, then the space
        // of pictures that are no longer on screen, and only then more room for a
        // region that fits but has none left. All of it happens here, on the insert
        // path, so a frame never changes the size of an atlas it is drawing.
        let frame = state.frame;
        let alloc = loop {
            if let Some(alloc) =
                state.allocators[kind_idx].allocate(size2(glyph_size.width.0, glyph_size.height.0))
            {
                break Some(alloc);
            }
            if state.grow_texture(kind_idx, glyph_size, limit, false) {
                continue;
            }
            if state.evict_oldest(kind_idx, frame) {
                continue;
            }
            if state.grow_texture(kind_idx, glyph_size, limit, true) {
                continue;
            }
            break None;
        };
        let Some(alloc) = alloc else {
            // `Window::paint_image` expects a tile and only turns an error into a
            // log line; answering "nothing to draw" here takes the platform thread
            // down instead of leaving the picture out of the frame.
            let texture = &state.textures[kind_idx];
            return Err(anyhow::anyhow!(
                "the {:?} atlas is {}x{} and has no room left for a {}x{} region",
                kind,
                texture.width,
                texture.height,
                glyph_size.width.0,
                glyph_size.height.0
            ));
        };
        let rect = alloc.rectangle;
        let bounds = Bounds {
            origin: point(DevicePixels(rect.min.x), DevicePixels(rect.min.y)),
            size: size(DevicePixels(rect.width()), DevicePixels(rect.height())),
        };

        let bpp = bytes_per_pixel(kind);
        let texture = &mut state.textures[kind_idx];
        let dst_stride = texture.width as usize * bpp;
        let src_stride = glyph_size.width.0 as usize * bpp;
        let mut skipped_rows = 0usize;
        for row in 0..glyph_size.height.0 as usize {
            let dst = (rect.min.y as usize + row) * dst_stride + rect.min.x as usize * bpp;
            let src = row * src_stride;
            if src + src_stride <= bytes.len() && dst + src_stride <= texture.cpu.len() {
                texture.cpu[dst..dst + src_stride].copy_from_slice(&bytes[src..src + src_stride]);
            } else {
                skipped_rows += 1;
            }
        }
        if skipped_rows > 0 {
            // The glyph's data and the size it was stored with disagree; skipping
            // the rows silently leaves a half-drawn glyph and no explanation.
            crate::log_line(&format!(
                "atlas stored only {} of {} rows for a {}x{} glyph",
                glyph_size.height.0 as usize - skipped_rows,
                glyph_size.height.0,
                glyph_size.width.0,
                glyph_size.height.0
            ));
        }
        state.dirty.push((kind, bounds));

        let tile = AtlasTile {
            texture_id: AtlasTextureId {
                index: kind_idx as u32,
                kind,
            },
            tile_id: TileId(alloc.id.serialize()),
            padding: 0,
            bounds,
        };
        state.used.insert(key.clone(), frame);
        state.tiles_by_key.insert(key.clone(), tile);
        state.alloc_ids.insert(key.clone(), alloc.id);
        Ok(Some(tile))
    }

    fn remove(&self, key: &AtlasKey) {
        let mut state = self.state.borrow_mut();
        if let Some(id) = state.alloc_ids.remove(key) {
            state.allocators[key.texture_kind() as usize].deallocate(id);
        }
        state.tiles_by_key.remove(key);
        state.used.remove(key);
    }
}
