// SPDX-License-Identifier: AGPL-3.0-or-later
//! The glyph atlas: an R8 texture of rasterized glyphs.

use std::collections::HashMap;

use dawkey_text::TextSystem;
use dawkey_text::cosmic_text::CacheKey;
use etagere::{AtlasAllocator, size2};

use crate::Gpu;

const SIZE: u32 = 2048;

/// The atlas ran out of space for the current frame.
#[derive(Debug)]
pub(crate) struct AtlasFull;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Slot {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) left: i32,
    pub(crate) top: i32,
}

pub(crate) struct Atlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    allocator: AtlasAllocator,
    /// `None` for glyphs without pixels (spaces).
    glyphs: HashMap<CacheKey, Option<Slot>>,
}

impl Atlas {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dawkey-render glyph atlas"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            allocator: AtlasAllocator::new(size2(SIZE as i32, SIZE as i32)),
            glyphs: HashMap::new(),
        }
    }

    pub(crate) fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub(crate) fn clear(&mut self) {
        self.allocator.clear();
        self.glyphs.clear();
    }

    /// Returns the glyph's slot, rasterizing and uploading it if new.
    pub(crate) fn get(
        &mut self,
        gpu: &Gpu,
        text: &mut TextSystem,
        key: CacheKey,
    ) -> Result<Option<Slot>, AtlasFull> {
        if let Some(slot) = self.glyphs.get(&key) {
            return Ok(*slot);
        }
        let Some(image) = text.rasterize(key) else {
            self.glyphs.insert(key, None);
            return Ok(None);
        };
        // One texel of padding keeps neighbors apart.
        let allocation = self
            .allocator
            .allocate(size2(image.width as i32 + 1, image.height as i32 + 1))
            .ok_or(AtlasFull)?;
        let slot = Slot {
            x: allocation.rectangle.min.x as u32,
            y: allocation.rectangle.min.y as u32,
            width: image.width,
            height: image.height,
            left: image.left,
            top: image.top,
        };
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: slot.x,
                    y: slot.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &image.alpha,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.width),
                rows_per_image: Some(image.height),
            },
            wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
        );
        self.glyphs.insert(key, Some(slot));
        Ok(Some(slot))
    }
}
