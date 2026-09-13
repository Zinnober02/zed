//! CPU-side glyph atlas for the OHOS backend.
//!
//! GPUI asks the window's PlatformAtlas to place rasterized glyphs and returns
//! an AtlasTile; the renderer later uploads the dirty regions into a Vulkan
//! texture and draws monochrome sprites from it.

use std::{borrow::Cow, cell::RefCell};

use anyhow::Result;
use collections::HashMap;
use etagere::{AllocId, BucketedAtlasAllocator, size2};

use crate::{
    AtlasKey, AtlasTextureId, AtlasTextureKind, AtlasTile, Bounds, DevicePixels, PlatformAtlas,
    TileId, point, size,
};

const ATLAS_DIM: u32 = 2048;

fn bytes_per_pixel(kind: AtlasTextureKind) -> usize {
    match kind {
        AtlasTextureKind::Monochrome => 1,
        AtlasTextureKind::Polychrome | AtlasTextureKind::Subpixel => 4,
    }
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
}

pub(crate) struct OhosAtlas {
    state: RefCell<AtlasState>,
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
                width: ATLAS_DIM,
                height: ATLAS_DIM,
                cpu: vec![0; ATLAS_DIM as usize * ATLAS_DIM as usize * bpp],
            });
            allocators.push(BucketedAtlasAllocator::new(size2(
                ATLAS_DIM as i32,
                ATLAS_DIM as i32,
            )));
        }
        Self {
            state: RefCell::new(AtlasState {
                textures,
                allocators,
                tiles_by_key: HashMap::default(),
                alloc_ids: HashMap::default(),
                dirty: Vec::new(),
            }),
        }
    }

    /// Size of the CPU texture backing a kind (all kinds share ATLAS_DIM).
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
            return Ok(Some(*tile));
        }

        let Some((glyph_size, bytes)) = build()? else {
            return Ok(None);
        };
        if glyph_size.width.0 <= 0 || glyph_size.height.0 <= 0 {
            return Ok(None);
        }

        let kind = key.texture_kind();
        let kind_idx = kind as usize;
        let Some(alloc) =
            state.allocators[kind_idx].allocate(size2(glyph_size.width.0, glyph_size.height.0))
        else {
            // A quiet `None` from here is indistinguishable from "nothing to draw",
            // and the glyph simply stops appearing with no trace of why.
            crate::log_line(&format!(
                "atlas has no room left for {:?}: a {}x{} glyph was dropped",
                key.texture_kind(),
                glyph_size.width.0,
                glyph_size.height.0
            ));
            return Ok(None);
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
    }
}
