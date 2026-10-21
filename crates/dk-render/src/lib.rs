// SPDX-License-Identifier: AGPL-3.0-or-later
//! Draws [`dk_ui::Scene`]s with wgpu (Vulkan), to a window surface or to an
//! image for screenshots.
//!
//! Every primitive is one instanced quad; the fragment shader evaluates a
//! signed distance per pixel (rounded rects, borders, shadows, lines,
//! triangles) or samples the glyph atlas. Colors are written as given, without
//! sRGB conversion, so render targets use non-sRGB formats.

#![forbid(unsafe_code)]

mod atlas;

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use dk_text::TextSystem;
use dk_ui::{Color, Point, Primitive, Rect, Scene, TextAlign};

use crate::atlas::{Atlas, AtlasFull};

pub use wgpu;

/// Format of offscreen images.
pub const IMAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("no suitable GPU adapter: {0}")]
    NoAdapter(String),
    #[error("could not open the GPU device: {0}")]
    Device(String),
    #[error("reading back the rendered image failed: {0}")]
    Readback(String),
}

/// Which adapter to use when there is no window to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuPreference {
    /// The CPU rasterizer (lavapipe/llvmpipe). Slow but identical
    /// everywhere, so screenshots are reproducible.
    Software,
    Hardware,
}

/// A GPU device and queue.
#[derive(Debug)]
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    pub fn new_instance() -> wgpu::Instance {
        wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            flags: wgpu::InstanceFlags::default(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        })
    }

    /// Opens a device with no window.
    pub fn headless(preference: GpuPreference) -> Result<Self, RenderError> {
        Self::open(Self::new_instance(), None, preference)
    }

    /// Opens a device for `instance`, compatible with `surface` if given.
    pub fn open(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
        preference: GpuPreference,
    ) -> Result<Self, RenderError> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: preference == GpuPreference::Software,
            compatible_surface: surface,
            ..Default::default()
        }))
        .map_err(|e| RenderError::NoAdapter(e.to_string()))?;
        let info = adapter.get_info();
        tracing::info!(
            "GPU: {} ({:?}, driver {} {})",
            info.name,
            info.device_type,
            info.driver,
            info.driver_info
        );
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("dawkey"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| RenderError::Device(e.to_string()))?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    pub fn adapter_name(&self) -> String {
        self.adapter.get_info().name
    }
}

/// An RGBA8 image (straight alpha, sRGB values).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header to a Vec");
        writer
            .write_image_data(&self.rgba)
            .expect("PNG data to a Vec");
        writer.finish().expect("PNG finish to a Vec");
        out
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Pod, Zeroable)]
struct Globals {
    viewport: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Pod, Zeroable)]
struct Instance {
    bounds: [f32; 4],
    color: [f32; 4],
    border_color: [f32; 4],
    radius: [f32; 4],
    shape: [f32; 4],
    extra: [f32; 4],
    params: [f32; 4],
    clip: [f32; 4],
}

const KIND_RECT: f32 = 0.0;
const KIND_SHADOW: f32 = 1.0;
const KIND_LINE: f32 = 2.0;
const KIND_TRIANGLE: f32 = 3.0;
const KIND_GLYPH: f32 = 4.0;

const NO_CLIP: [f32; 4] = [-1e9, -1e9, 1e9, 1e9];

/// Draws scenes. One renderer can target several formats (a window and
/// screenshots) and shares its glyph atlas between them.
pub struct Renderer {
    pipeline_layout: wgpu::PipelineLayout,
    shader: wgpu::ShaderModule,
    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    globals: wgpu::Buffer,
    instances: wgpu::Buffer,
    instance_capacity: usize,
    atlas: Atlas,
    bind_group: wgpu::BindGroup,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("formats", &self.pipelines.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Renderer {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dk-render"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dk-render"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dk-render"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dk-render globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instance_capacity = 1024;
        let instances = create_instance_buffer(device, instance_capacity);
        let atlas = Atlas::new(device);
        let bind_group = create_bind_group(device, &layout, &globals, &atlas);
        Self {
            pipeline_layout,
            shader,
            pipelines: HashMap::new(),
            globals,
            instances,
            instance_capacity,
            atlas,
            bind_group,
        }
    }

    fn pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> &wgpu::RenderPipeline {
        self.pipelines.entry(format).or_insert_with(|| {
            const ATTRS: [wgpu::VertexAttribute; 8] = wgpu::vertex_attr_array![
                0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4,
                4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4,
            ];
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("dk-render"),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Instance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &ATTRS,
                    })],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        })
    }

    /// Draws `scene` into `target` (of `size` physical pixels), clearing it to
    /// `clear` first. `scale` maps the scene's logical pixels to physical.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        gpu: &Gpu,
        text: &mut TextSystem,
        scene: &Scene,
        scale: f32,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        size: (u32, u32),
        clear: Color,
    ) {
        let instances = match self.build(gpu, text, scene, scale) {
            Ok(instances) => instances,
            Err(AtlasFull) => {
                // Start the atlas over and lay this frame out again.
                self.atlas.clear();
                self.build(gpu, text, scene, scale).unwrap_or_else(|_| {
                    tracing::warn!("glyph atlas too small for one frame; text is missing");
                    Vec::new()
                })
            }
        };
        let device = &gpu.device;
        if instances.len() > self.instance_capacity {
            self.instance_capacity = instances.len().next_power_of_two();
            self.instances = create_instance_buffer(device, self.instance_capacity);
        }
        gpu.queue
            .write_buffer(&self.instances, 0, bytemuck::cast_slice(&instances));
        gpu.queue.write_buffer(
            &self.globals,
            0,
            bytemuck::bytes_of(&Globals {
                viewport: [size.0 as f32, size.1 as f32],
                _pad: [0.0; 2],
            }),
        );

        let pipeline = self.pipeline(device, format).clone();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dk-render"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(clear.r * clear.a),
                            g: f64::from(clear.g * clear.a),
                            b: f64::from(clear.b * clear.a),
                            a: f64::from(clear.a),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if !instances.is_empty() {
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.instances.slice(..));
                pass.draw(0..6, 0..instances.len() as u32);
            }
        }
        gpu.queue.submit([encoder.finish()]);
    }

    /// Renders `scene` offscreen and reads it back.
    pub fn render_image(
        &mut self,
        gpu: &Gpu,
        text: &mut TextSystem,
        scene: &Scene,
        scale: f32,
        clear: Color,
    ) -> Result<Image, RenderError> {
        let width = ((scene.size.w * scale).round() as u32).max(1);
        let height = ((scene.size.h * scale).round() as u32).max(1);
        let device = &gpu.device;
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dk-render image"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: IMAGE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.render(
            gpu,
            text,
            scene,
            scale,
            &view,
            IMAGE_FORMAT,
            (width, height),
            clear,
        );

        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let bytes_per_row = (width * 4).div_ceil(align) * align;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dk-render readback"),
            size: u64::from(bytes_per_row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dk-render readback"),
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            extent,
        );
        let index = gpu.queue.submit([encoder.finish()]);
        readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            })
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        {
            let data = readback
                .slice(..)
                .get_mapped_range()
                .map_err(|e| RenderError::Readback(e.to_string()))?;
            for row in 0..height {
                let start = (row * bytes_per_row) as usize;
                rgba.extend_from_slice(&data[start..start + (width * 4) as usize]);
            }
        }
        readback.unmap();
        unpremultiply(&mut rgba);
        Ok(Image {
            width,
            height,
            rgba,
        })
    }

    /// Converts the scene to instances, uploading any new glyphs.
    fn build(
        &mut self,
        gpu: &Gpu,
        text: &mut TextSystem,
        scene: &Scene,
        scale: f32,
    ) -> Result<Vec<Instance>, AtlasFull> {
        let s = |v: f32| v * scale;
        let rect = |r: Rect| [s(r.x), s(r.y), s(r.w), s(r.h)];
        let point = |p: Point| [s(p.x), s(p.y)];
        let clip = |c: &Option<Rect>| match c {
            Some(c) => [s(c.x), s(c.y), s(c.right()), s(c.bottom())],
            None => NO_CLIP,
        };
        let color = |c: Color| [c.r, c.g, c.b, c.a];

        let mut out = Vec::with_capacity(scene.primitives.len());
        for primitive in &scene.primitives {
            match primitive {
                Primitive::Rect {
                    rect: r,
                    color: c,
                    radius,
                    border,
                    clip: cl,
                } => {
                    let (bw, bc) = border.map_or((0.0, Color::TRANSPARENT), |(w, c)| (s(w), c));
                    let shape = rect(*r);
                    out.push(Instance {
                        bounds: expand(shape, 1.0),
                        color: color(*c),
                        border_color: color(bc),
                        radius: radius.map(s),
                        shape,
                        params: [KIND_RECT, bw, 0.0, 0.0],
                        clip: clip(cl),
                        ..Default::default()
                    });
                }
                Primitive::Shadow {
                    rect: r,
                    radius,
                    blur,
                    color: c,
                } => {
                    let shape = rect(*r);
                    out.push(Instance {
                        bounds: expand(shape, s(*blur) * 2.0 + 1.0),
                        color: color(*c),
                        radius: [s(*radius); 4],
                        shape,
                        params: [KIND_SHADOW, 0.0, s(*blur), 0.0],
                        clip: NO_CLIP,
                        ..Default::default()
                    });
                }
                Primitive::Line {
                    from,
                    to,
                    width,
                    color: c,
                    clip: cl,
                } => {
                    let (a, b) = (point(*from), point(*to));
                    let half = s(*width) / 2.0;
                    out.push(Instance {
                        bounds: expand(bbox(&[a, b]), half + 1.0),
                        color: color(*c),
                        shape: [a[0], a[1], b[0], b[1]],
                        extra: [half, 0.0, 0.0, 0.0],
                        params: [KIND_LINE, 0.0, 0.0, 0.0],
                        clip: clip(cl),
                        ..Default::default()
                    });
                }
                Primitive::Triangle {
                    points,
                    color: c,
                    clip: cl,
                } => {
                    let [a, b, cc] = points.map(point);
                    out.push(Instance {
                        bounds: expand(bbox(&[a, b, cc]), 1.0),
                        color: color(*c),
                        shape: [a[0], a[1], b[0], b[1]],
                        extra: [cc[0], cc[1], 0.0, 0.0],
                        params: [KIND_TRIANGLE, 0.0, 0.0, 0.0],
                        clip: clip(cl),
                        ..Default::default()
                    });
                }
                Primitive::Text {
                    rect: r,
                    text: string,
                    style,
                    color: c,
                    align,
                    clip: cl,
                } => {
                    let line = text.shape(string, style, scale);
                    let x = match align {
                        TextAlign::Start => r.x,
                        TextAlign::Center => r.x + (r.w - line.width) / 2.0,
                        TextAlign::End => r.right() - line.width,
                    };
                    let y = r.y + (r.h - line.height) / 2.0;
                    let (ox, oy) = (s(x).round() as i32, s(y).round() as i32);
                    for glyph in &line.glyphs {
                        let Some(slot) = self.atlas.get(gpu, text, glyph.key)? else {
                            continue;
                        };
                        let gx = (ox + glyph.x + slot.left) as f32;
                        let gy = (oy + glyph.y - slot.top) as f32;
                        out.push(Instance {
                            bounds: [gx, gy, slot.width as f32, slot.height as f32],
                            color: color(*c),
                            shape: [slot.x as f32, slot.y as f32, 0.0, 0.0],
                            params: [KIND_GLYPH, 0.0, 0.0, 0.0],
                            clip: clip(cl),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        Ok(out)
    }
}

fn expand(r: [f32; 4], by: f32) -> [f32; 4] {
    [r[0] - by, r[1] - by, r[2] + 2.0 * by, r[3] + 2.0 * by]
}

fn bbox(points: &[[f32; 2]]) -> [f32; 4] {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in points {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    [x0, y0, x1 - x0, y1 - y0]
}

/// The GPU blends in premultiplied alpha; PNGs want straight alpha.
fn unpremultiply(rgba: &mut [u8]) {
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3];
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((u16::from(*c) * 255 + u16::from(a) / 2) / u16::from(a)).min(255) as u8;
            }
        }
    }
}

fn create_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("dk-render instances"),
        size: (capacity * std::mem::size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    globals: &wgpu::Buffer,
    atlas: &Atlas,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("dk-render"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(atlas.view()),
            },
        ],
    })
}

#[cfg(test)]
mod tests;
