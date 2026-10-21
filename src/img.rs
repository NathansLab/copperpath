//! Tiny RGB canvas with PNG output, a 3x5 digit/letter font for labels and shape drawing.

use crate::geom::*;

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[u8; 3]>,
    pub gs: GridSpec, // world -> pixel mapping (one pixel per cell)
}

impl Canvas {
    pub fn new(gs: GridSpec, bg: [u8; 3]) -> Canvas {
        Canvas {
            w: gs.nx,
            h: gs.ny,
            px: vec![bg; gs.nx * gs.ny],
            gs,
        }
    }
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, c: [u8; 3]) {
        if x < self.w && y < self.h {
            self.px[y * self.w + x] = c;
        }
    }
    #[inline]
    pub fn blend(&mut self, x: usize, y: usize, c: [u8; 3], a: f32) {
        if x < self.w && y < self.h {
            let o = &mut self.px[y * self.w + x];
            for k in 0..3 {
                o[k] = (o[k] as f32 * (1.0 - a) + c[k] as f32 * a) as u8;
            }
        }
    }
    pub fn shape(&mut self, s: &Shape, c: [u8; 3], a: f32) {
        let gs = self.gs;
        let mut cells = Vec::new();
        gs.stamp(s, 0.0, |x, y| cells.push((x, y)));
        for (x, y) in cells {
            self.blend(x, y, c, a);
        }
    }
    pub fn outline(&mut self, pts: &[P], c: [u8; 3]) {
        let n = pts.len();
        for i in 0..n {
            self.line(pts[i], pts[(i + 1) % n], c);
        }
    }
    pub fn line(&mut self, a: P, b: P, c: [u8; 3]) {
        let gs = self.gs;
        let steps = (((b - a).len() / gs.res) * 2.0).ceil().max(1.0) as usize;
        for k in 0..=steps {
            let q = a + (b - a) * (k as f64 / steps as f64);
            if let Some((x, y)) = gs.cell_of(q) {
                self.set(x, y, c);
            }
        }
    }
    /// label text at world position (font 3x5, scale s)
    pub fn text(&mut self, at: P, t: &str, c: [u8; 3], s: usize) {
        let Some((mut x, y)) = self.gs.cell_of(at) else {
            return;
        };
        for ch in t.chars() {
            let g = glyph(ch);
            for (row, bits) in g.iter().enumerate() {
                for col in 0..3 {
                    if bits & (4 >> col) != 0 {
                        for dy in 0..s {
                            for dx in 0..s {
                                self.set(x + col * s + dx, y + row * s + dy, c);
                            }
                        }
                    }
                }
            }
            x += 4 * s;
        }
    }
    pub fn save(&self, path: &str, scale: usize) -> std::io::Result<()> {
        let (w, h) = (self.w * scale, self.h * scale);
        let f = std::fs::File::create(path)?;
        let mut e = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        let mut wr = e.write_header().map_err(std::io::Error::other)?;
        let mut data = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&self.px[(y / scale) * self.w + x / scale]);
            }
        }
        wr.write_image_data(&data).map_err(std::io::Error::other)?;
        Ok(())
    }
}

fn glyph(c: char) -> [u8; 5] {
    match c.to_ascii_uppercase() {
        '0' => [7, 5, 5, 5, 7],
        '1' => [2, 6, 2, 2, 7],
        '2' => [7, 1, 7, 4, 7],
        '3' => [7, 1, 7, 1, 7],
        '4' => [5, 5, 7, 1, 1],
        '5' => [7, 4, 7, 1, 7],
        '6' => [7, 4, 7, 5, 7],
        '7' => [7, 1, 1, 1, 1],
        '8' => [7, 5, 7, 5, 7],
        '9' => [7, 5, 7, 1, 7],
        'A' => [2, 5, 7, 5, 5],
        'B' => [6, 5, 6, 5, 6],
        'C' => [7, 4, 4, 4, 7],
        'D' => [6, 5, 5, 5, 6],
        'E' => [7, 4, 6, 4, 7],
        'F' => [7, 4, 6, 4, 4],
        'G' => [7, 4, 5, 5, 7],
        'H' => [5, 5, 7, 5, 5],
        'I' => [7, 2, 2, 2, 7],
        'K' => [5, 5, 6, 5, 5],
        'L' => [4, 4, 4, 4, 7],
        'M' => [5, 7, 7, 5, 5],
        'N' => [6, 5, 5, 5, 5],
        'O' => [7, 5, 5, 5, 7],
        'P' => [7, 5, 7, 4, 4],
        'Q' => [7, 5, 5, 7, 1],
        'R' => [7, 5, 6, 5, 5],
        'S' => [7, 4, 7, 1, 7],
        'T' => [7, 2, 2, 2, 2],
        'U' => [5, 5, 5, 5, 7],
        'V' => [5, 5, 5, 5, 2],
        'W' => [5, 5, 7, 7, 5],
        'X' => [5, 5, 2, 5, 5],
        'Y' => [5, 5, 2, 2, 2],
        'Z' => [7, 1, 2, 4, 7],
        '.' => [0, 0, 0, 0, 2],
        '-' => [0, 0, 7, 0, 0],
        '/' => [1, 1, 2, 4, 4],
        ':' => [0, 2, 0, 2, 0],
        '%' => [5, 1, 2, 4, 5],
        _ => [0, 0, 0, 0, 0],
    }
}

/// heat colour for x in [0, 1+]: green -> yellow -> red -> magenta (overflow)
pub fn heat(x: f64) -> [u8; 3] {
    if x <= 0.5 {
        let t = x / 0.5;
        [(255.0 * t) as u8, 200, 0]
    } else if x <= 1.0 {
        let t = (x - 0.5) / 0.5;
        [255, (200.0 * (1.0 - t)) as u8, 0]
    } else {
        [255, 0, 255]
    }
}
