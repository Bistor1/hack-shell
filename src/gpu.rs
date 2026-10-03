//! Cell renderer. Glyphs are rasterized once into an atlas; every visible cell
//! is an instanced quad drawn by the GPU.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::process::Command;

use bytemuck::{Pod, Zeroable};
use fontdue::{Font, FontSettings};

const ATLAS: u32 = 2048;

#[derive(Clone, Copy)]
pub struct Glyph {
    pub uv: [f32; 4],
    pub bearing: [f32; 2],
    pub size: [f32; 2],
    pub advance: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    style: u8,
    ch: char,
    px: u16,
}

pub struct Fonts {
    fonts: [Font; 4],
    pub cell_w: f32,
    pub cell_h: f32,
    pub ascent: f32,
    size: f32,
    atlas: Vec<u8>,
    cursor_x: u32,
    cursor_y: u32,
    row_h: u32,
    glyphs: HashMap<GlyphKey, Glyph>,
    dirty: bool,
    texture: Option<wgpu::Texture>,
    pub epoch: u64,
}

impl Fonts {
    pub fn load(family: &str, size: f32) -> Self {
        let fonts = [
            load_face(family, ""),
            load_face(family, ":weight=bold"),
            load_face(family, ":slant=italic"),
            load_face(family, ":weight=bold:slant=italic"),
        ];
        let mut fonts = Self {
            fonts,
            cell_w: 8.0,
            cell_h: 16.0,
            ascent: 12.0,
            size,
            atlas: vec![0; (ATLAS * ATLAS) as usize],
            cursor_x: 1,
            cursor_y: 1,
            row_h: 0,
            glyphs: HashMap::new(),
            dirty: true,
            texture: None,
            epoch: 1,
        };
        fonts.measure();
        fonts
    }

    pub fn set_size(&mut self, size: f32) {
        if (self.size - size).abs() < 0.05 {
            return;
        }
        self.size = size.clamp(6.0, 48.0);
        self.glyphs.clear();
        self.atlas.fill(0);
        self.cursor_x = 1;
        self.cursor_y = 1;
        self.row_h = 0;
        self.dirty = true;
        self.measure();
    }

    pub fn size(&self) -> f32 {
        self.size
    }

    fn measure(&mut self) {
        let font = &self.fonts[0];
        let metrics = font.horizontal_line_metrics(self.size).unwrap_or(fontdue::LineMetrics {
            ascent: self.size * 0.8,
            descent: -self.size * 0.2,
            line_gap: 0.0,
            new_line_size: self.size,
        });
        self.ascent = metrics.ascent;
        self.cell_h = (metrics.ascent - metrics.descent + metrics.line_gap).ceil().max(8.0);
        let (gm, _) = font.rasterize('M', self.size);
        self.cell_w = gm.advance_width.ceil().max(4.0);
    }

    pub fn glyph(&mut self, style: u8, ch: char) -> Glyph {
        let px = (self.size * 64.0) as u16;
        let key = GlyphKey { style, ch, px };
        if let Some(g) = self.glyphs.get(&key) {
            return *g;
        }
        let font = &self.fonts[style.min(3) as usize];
        let (metrics, bitmap) = font.rasterize(ch, self.size);
        let gw = metrics.width as u32;
        let gh = metrics.height as u32;
        let glyph = if gw == 0 || gh == 0 {
            Glyph {
                uv: [0.0; 4],
                bearing: [metrics.xmin as f32, metrics.ymin as f32],
                size: [0.0, 0.0],
                advance: metrics.advance_width,
            }
        } else {
            if self.cursor_x + gw + 1 >= ATLAS {
                self.cursor_x = 1;
                self.cursor_y += self.row_h + 1;
                self.row_h = 0;
            }
            if self.cursor_y + gh + 1 >= ATLAS {
                // Atlas exhausted: reset and keep going. Rare for a terminal.
                self.atlas.fill(0);
                self.glyphs.clear();
                self.cursor_x = 1;
                self.cursor_y = 1;
                self.row_h = 0;
            }
            for row in 0..gh {
                let dst = ((self.cursor_y + row) * ATLAS + self.cursor_x) as usize;
                let src = (row * gw) as usize;
                self.atlas[dst..dst + gw as usize].copy_from_slice(&bitmap[src..src + gw as usize]);
            }
            let g = Glyph {
                uv: [self.cursor_x as f32, self.cursor_y as f32, gw as f32, gh as f32],
                bearing: [metrics.xmin as f32, metrics.ymin as f32],
                size: [gw as f32, gh as f32],
                advance: metrics.advance_width,
            };
            self.cursor_x += gw + 1;
            self.row_h = self.row_h.max(gh);
            self.dirty = true;
            g
        };
        self.glyphs.insert(key, glyph);
        glyph
    }

    pub fn texture(&self) -> &wgpu::Texture {
        self.texture.as_ref().expect("atlas uploaded")
    }

    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> u64 {
        if self.texture.is_none() {
            self.epoch += 1;
            self.texture = Some(device.create_texture(&wgpu::TextureDescriptor {
                label: Some("glyph-atlas"),
                size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            }));
            self.dirty = true;
        }
        if self.dirty {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: self.texture.as_ref().unwrap(),
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &self.atlas,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(ATLAS),
                    rows_per_image: Some(ATLAS),
                },
                wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            );
            self.dirty = false;
        }
        self.epoch
    }
}

fn load_face(family: &str, extra: &str) -> Font {
    let spec = format!("{family}{extra}");
    let path = Command::new("fc-match")
        .args(["-f", "%{file}", &spec])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .filter(|s| !s.is_empty() && !s.contains("\n"));
    let bytes = path
        .as_deref()
        .and_then(|p| std::fs::read(p).ok())
        .or_else(|| std::fs::read("/usr/share/fonts/TTF/Hack-Regular.ttf").ok())
        .unwrap_or_else(|| panic!("no font found for {spec}"));
    Font::from_bytes(bytes, FontSettings::default()).expect("parse font")
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Instance {
    pub rect: [f32; 4],
    pub uv: [f32; 4],
    pub color: [f32; 4],
    pub kind: f32,
    pub _pad: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    screen: [f32; 2],
    atlas: [f32; 2],
    premultiply: f32,
    _pad: f32,
}

pub struct DrawList {
    pub instances: Vec<Instance>,
    pub ranges: Vec<DrawRange>,
}

pub struct DrawRange {
    pub start: u32,
    pub count: u32,
    pub scissor: [u32; 4],
}

impl DrawList {
    pub fn new() -> Self {
        Self { instances: Vec::with_capacity(4096), ranges: Vec::new() }
    }

    pub fn begin_pane(&mut self, scissor: [u32; 4]) {
        self.ranges.push(DrawRange { start: self.instances.len() as u32, count: 0, scissor });
    }

    pub fn push(&mut self, inst: Instance) {
        self.instances.push(inst);
        if let Some(range) = self.ranges.last_mut() {
            range.count += 1;
        }
    }

    pub fn solid(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        if w < 0.5 || h < 0.5 {
            return;
        }
        self.push(Instance {
            rect: [x, y, w, h],
            uv: [0.0; 4],
            color,
            kind: 0.0,
            _pad: [0.0; 3],
        });
    }

    pub fn glyph(&mut self, x: f32, y: f32, g: Glyph, color: [f32; 4]) {
        if g.size[0] < 0.5 || g.size[1] < 0.5 {
            return;
        }
        self.push(Instance {
            rect: [x, y, g.size[0], g.size[1]],
            uv: g.uv,
            color,
            kind: 1.0,
            _pad: [0.0; 3],
        });
    }
}

pub struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buf: wgpu::Buffer,
    instance_buf: wgpu::Buffer,
    instance_cap: usize,
    bind_group: Option<wgpu::BindGroup>,
    atlas_epoch: u64,
}

impl Pipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cells"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cells"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(std::mem::size_of::<Uniforms>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cells"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cells"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x4,
                        1 => Float32x4,
                        2 => Float32x4,
                        3 => Float32
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instance_cap = 8192;
        let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (instance_cap * std::mem::size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
            uniform_buf,
            instance_buf,
            instance_cap,
            bind_group: None,
            atlas_epoch: 0,
        }
    }

    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'_>,
        atlas_epoch: u64,
        atlas: &wgpu::Texture,
        screen: [f32; 2],
        premultiply: bool,
        list: &DrawList,
    ) {
        if self.bind_group.is_none() || self.atlas_epoch != atlas_epoch {
            let view = atlas.create_view(&wgpu::TextureViewDescriptor::default());
            self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("cells"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.uniform_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            }));
            self.atlas_epoch = atlas_epoch;
        }
        queue.write_buffer(
            &self.uniform_buf,
            0,
            bytemuck::bytes_of(&Uniforms {
                screen,
                atlas: [ATLAS as f32, ATLAS as f32],
                premultiply: if premultiply { 1.0 } else { 0.0 },
                _pad: 0.0,
            }),
        );
        if list.instances.is_empty() {
            return;
        }
        if list.instances.len() > self.instance_cap {
            self.instance_cap = list.instances.len().next_power_of_two();
            self.instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("instances"),
                size: (self.instance_cap * std::mem::size_of::<Instance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(&list.instances));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, self.bind_group.as_ref().unwrap(), &[]);
        pass.set_vertex_buffer(0, self.instance_buf.slice(..));
        for range in &list.ranges {
            if range.count == 0 || range.scissor[2] == 0 || range.scissor[3] == 0 {
                continue;
            }
            pass.set_scissor_rect(range.scissor[0], range.scissor[1], range.scissor[2], range.scissor[3]);
            pass.draw(0..6, range.start..range.start + range.count);
        }
    }
}

const SHADER: &str = r#"
struct Uniforms {
    screen: vec2<f32>,
    atlas: vec2<f32>,
    premultiply: f32,
    _pad: f32,
};
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) kind: f32,
};
struct Instance {
    @location(0) rect: vec4<f32>,
    @location(1) uv: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) kind: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas_tex: texture_2d<f32>;
@group(0) @binding(2) var atlas_samp: sampler;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0)
    );
    let c = corners[vi];
    let px = inst.rect.xy + c * inst.rect.zw;
    let ndc = vec2<f32>(px.x / u.screen.x * 2.0 - 1.0, 1.0 - px.y / u.screen.y * 2.0);
    let uv = inst.uv.xy + c * inst.uv.zw;
    return VsOut(vec4<f32>(ndc, 0.0, 1.0), uv, inst.color, inst.kind);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var color = in.color;
    if (in.kind > 0.5) {
        let cov = textureSample(atlas_tex, atlas_samp, in.uv / u.atlas).r;
        color = vec4<f32>(in.color.rgb, in.color.a * cov);
    }
    if (u.premultiply > 0.5) {
        color = vec4<f32>(color.rgb * color.a, color.a);
    }
    return color;
}
"#;
