//! CPU cell painter. Glyphs are rasterized once into an atlas and blitted
//! into a software framebuffer. No GPU is involved.

use std::collections::HashMap;
use std::process::Command;

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
            g
        };
        self.glyphs.insert(key, glyph);
        glyph
    }
}

fn load_face(family: &str, extra: &str) -> Font {
    let spec = format!("{family}{extra}");
    let path = Command::new("fc-match")
        .args(["-f", "%{file}", &spec])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .filter(|s| !s.is_empty() && !s.contains('\n'));
    let bytes = path
        .as_deref()
        .and_then(|p| std::fs::read(p).ok())
        .or_else(|| std::fs::read("/usr/share/fonts/TTF/Hack-Regular.ttf").ok())
        .unwrap_or_else(|| panic!("no font found for {spec}"));
    Font::from_bytes(bytes, FontSettings::default()).expect("parse font")
}

#[derive(Clone, Copy)]
pub struct Instance {
    pub rect: [f32; 4],
    pub uv: [f32; 4],
    pub color: [f32; 4],
    pub kind: f32,
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
        self.push(Instance { rect: [x, y, w, h], uv: [0.0; 4], color, kind: 0.0 });
    }

    pub fn glyph(&mut self, x: f32, y: f32, g: Glyph, color: [f32; 4]) {
        if g.size[0] < 0.5 || g.size[1] < 0.5 {
            return;
        }
        self.push(Instance { rect: [x, y, g.size[0], g.size[1]], uv: g.uv, color, kind: 1.0 });
    }
}

pub struct CpuImage {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[u8; 4]>,
}

pub fn apply_image_delta(
    textures: &mut HashMap<egui::TextureId, CpuImage>,
    id: egui::TextureId,
    delta: &egui::epaint::ImageDelta,
) {
    let egui::epaint::ImageData::Color(image) = &delta.image;
    let patch: Vec<[u8; 4]> = image.pixels.iter().map(|c| c.to_array()).collect();
    let [pw, ph] = image.size;
    if let Some([x, y]) = delta.pos {
        let Some(tex) = textures.get_mut(&id) else { return };
        for row in 0..ph {
            let dy = y + row;
            if dy >= tex.h {
                break;
            }
            for col in 0..pw {
                let dx = x + col;
                if dx >= tex.w {
                    break;
                }
                tex.px[dy * tex.w + dx] = patch[row * pw + col];
            }
        }
    } else {
        textures.insert(id, CpuImage { w: pw, h: ph, px: patch });
    }
}

/// Paint terminal cells into an XRGB framebuffer (`0x00RRGGBB`).
pub fn paint_cells(fonts: &Fonts, list: &DrawList, width: u32, height: u32, pixels: &mut [u32]) {
    let width = width as i32;
    let height = height as i32;
    for range in &list.ranges {
        let x0 = range.scissor[0] as i32;
        let y0 = range.scissor[1] as i32;
        let x1 = x0 + range.scissor[2] as i32;
        let y1 = y0 + range.scissor[3] as i32;
        let end = (range.start + range.count) as usize;
        for inst in &list.instances[range.start as usize..end.min(list.instances.len())] {
            if inst.kind < 0.5 {
                fill_rect(pixels, width, height, inst.rect, inst.color, x0, y0, x1, y1);
            } else {
                blit_glyph(fonts, pixels, width, height, inst, x0, y0, x1, y1);
            }
        }
    }
}

pub fn paint_egui(
    jobs: &[egui::ClippedPrimitive],
    textures: &HashMap<egui::TextureId, CpuImage>,
    ppp: f32,
    width: u32,
    height: u32,
    pixels: &mut [u32],
) {
    for job in jobs {
        let egui::epaint::Primitive::Mesh(mesh) = &job.primitive else { continue };
        let clip = job.clip_rect;
        let clip = [
            (clip.min.x * ppp) as i32,
            (clip.min.y * ppp) as i32,
            (clip.max.x * ppp) as i32,
            (clip.max.y * ppp) as i32,
        ];
        let tex = textures.get(&mesh.texture_id);
        let tris = mesh.indices.len() / 3;
        for t in 0..tris {
            let i0 = mesh.indices[t * 3] as usize;
            let i1 = mesh.indices[t * 3 + 1] as usize;
            let i2 = mesh.indices[t * 3 + 2] as usize;
            if i0 >= mesh.vertices.len() || i1 >= mesh.vertices.len() || i2 >= mesh.vertices.len() {
                continue;
            }
            raster_tri(pixels, width, height, ppp, tex, &mesh.vertices[i0], &mesh.vertices[i1], &mesh.vertices[i2], clip);
        }
    }
}

fn fill_rect(pixels: &mut [u32], width: i32, height: i32, rect: [f32; 4], color: [f32; 4], sx0: i32, sy0: i32, sx1: i32, sy1: i32) {
    let x0 = (rect[0].floor() as i32).max(sx0).max(0);
    let y0 = (rect[1].floor() as i32).max(sy0).max(0);
    let x1 = ((rect[0] + rect[2]).ceil() as i32).min(sx1).min(width);
    let y1 = ((rect[1] + rect[3]).ceil() as i32).min(sy1).min(height);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let src = pack(color);
    let a = (color[3] * 255.0) as u32;
    if a >= 250 {
        for y in y0..y1 {
            let row = &mut pixels[(y * width) as usize + x0 as usize..(y * width) as usize + x1 as usize];
            row.fill(src);
        }
        return;
    }
    if a == 0 {
        return;
    }
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * width + x) as usize;
            pixels[i] = blend(pixels[i], src, a);
        }
    }
}

fn blit_glyph(fonts: &Fonts, pixels: &mut [u32], width: i32, height: i32, inst: &Instance, sx0: i32, sy0: i32, sx1: i32, sy1: i32) {
    let gw = inst.uv[2] as i32;
    let gh = inst.uv[3] as i32;
    if gw <= 0 || gh <= 0 {
        return;
    }
    let x0 = inst.rect[0].floor() as i32;
    let y0 = inst.rect[1].floor() as i32;
    let cr = (inst.color[0] * 255.0) as u32;
    let cg = (inst.color[1] * 255.0) as u32;
    let cb = (inst.color[2] * 255.0) as u32;
    let au = inst.uv[0] as i32;
    let av = inst.uv[1] as i32;
    for row in 0..gh {
        let y = y0 + row;
        if y < sy0 || y >= sy1 || y < 0 || y >= height {
            continue;
        }
        let atlas_row = (av + row) as u32;
        if atlas_row >= ATLAS {
            break;
        }
        for col in 0..gw {
            let x = x0 + col;
            if x < sx0 || x >= sx1 || x < 0 || x >= width {
                continue;
            }
            let atlas_col = (au + col) as u32;
            if atlas_col >= ATLAS {
                break;
            }
            let cov = fonts.atlas[(atlas_row * ATLAS + atlas_col) as usize] as u32;
            if cov == 0 {
                continue;
            }
            let src = (cr << 16) | (cg << 8) | cb;
            let i = (y * width + x) as usize;
            pixels[i] = blend(pixels[i], src, cov);
        }
    }
}

fn raster_tri(
    pixels: &mut [u32],
    width: u32,
    height: u32,
    ppp: f32,
    tex: Option<&CpuImage>,
    v0: &egui::epaint::Vertex,
    v1: &egui::epaint::Vertex,
    v2: &egui::epaint::Vertex,
    clip: [i32; 4],
) {
    let p0 = [v0.pos.x * ppp, v0.pos.y * ppp];
    let p1 = [v1.pos.x * ppp, v1.pos.y * ppp];
    let p2 = [v2.pos.x * ppp, v2.pos.y * ppp];
    let min_x = p0[0].min(p1[0]).min(p2[0]).floor() as i32;
    let min_y = p0[1].min(p1[1]).min(p2[1]).floor() as i32;
    let max_x = p0[0].max(p1[0]).max(p2[0]).ceil() as i32;
    let max_y = p0[1].max(p1[1]).max(p2[1]).ceil() as i32;
    let x0 = min_x.max(clip[0]).max(0);
    let y0 = min_y.max(clip[1]).max(0);
    let x1 = max_x.min(clip[2]).min(width as i32);
    let y1 = max_y.min(clip[3]).min(height as i32);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let area = edge(p0, p1, p2);
    if area.abs() < 0.5 {
        return;
    }
    let c0 = v0.color.to_array();
    let c1 = v1.color.to_array();
    let c2 = v2.color.to_array();
    for y in y0..y1 {
        for x in x0..x1 {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let w0 = edge(p1, p2, p) / area;
            let w1 = edge(p2, p0, p) / area;
            let w2 = edge(p0, p1, p) / area;
            // egui does not keep a consistent triangle winding.
            if w0 < -0.001 || w1 < -0.001 || w2 < -0.001 {
                continue;
            }
            let mut r = w0 * c0[0] as f32 + w1 * c1[0] as f32 + w2 * c2[0] as f32;
            let mut g = w0 * c0[1] as f32 + w1 * c1[1] as f32 + w2 * c2[1] as f32;
            let mut b = w0 * c0[2] as f32 + w1 * c1[2] as f32 + w2 * c2[2] as f32;
            let mut a = w0 * c0[3] as f32 + w1 * c1[3] as f32 + w2 * c2[3] as f32;
            if let Some(tex) = tex {
                let u = w0 * v0.uv.x + w1 * v1.uv.x + w2 * v2.uv.x;
                let v = w0 * v0.uv.y + w1 * v1.uv.y + w2 * v2.uv.y;
                let tx = (u * tex.w as f32) as usize;
                let ty = (v * tex.h as f32) as usize;
                if tx < tex.w && ty < tex.h {
                    let sample = tex.px[ty * tex.w + tx];
                    let ta = sample[3] as f32 / 255.0;
                    r *= sample[0] as f32 / 255.0;
                    g *= sample[1] as f32 / 255.0;
                    b *= sample[2] as f32 / 255.0;
                    a *= ta;
                }
            }
            let ai = a as u32;
            if ai == 0 {
                continue;
            }
            let src = ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
            let i = (y as u32 * width + x as u32) as usize;
            if ai >= 250 {
                pixels[i] = src;
            } else {
                pixels[i] = blend(pixels[i], src, ai.min(255));
            }
        }
    }
}

fn edge(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (c[0] - a[0]) * (b[1] - a[1]) - (c[1] - a[1]) * (b[0] - a[0])
}

fn pack(color: [f32; 4]) -> u32 {
    let r = (color[0] * 255.0) as u32;
    let g = (color[1] * 255.0) as u32;
    let b = (color[2] * 255.0) as u32;
    (r << 16) | (g << 8) | b
}

fn blend(dst: u32, src: u32, a: u32) -> u32 {
    let inv = 255 - a;
    let dr = (dst >> 16) & 255;
    let dg = (dst >> 8) & 255;
    let db = dst & 255;
    let sr = (src >> 16) & 255;
    let sg = (src >> 8) & 255;
    let sb = src & 255;
    let r = sr * a / 255 + dr * inv / 255;
    let g = sg * a / 255 + dg * inv / 255;
    let b = sb * a / 255 + db * inv / 255;
    (r << 16) | (g << 8) | b
}
