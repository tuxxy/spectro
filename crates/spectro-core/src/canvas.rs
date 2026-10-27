//! A plain `0x00RRGGBB` pixel buffer with the few drawing operations the
//! renderer needs.

use alloc::vec;
use alloc::vec::Vec;

pub struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
}

pub fn rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

/// Mixes `fg` over `bg` with coverage `a` in `0..=1`.
pub fn mix(bg: u32, fg: u32, a: f32) -> u32 {
    let a = (a.clamp(0.0, 1.0) * 256.0) as u32;
    let ch = |s: u32| {
        let (b, f) = ((bg >> s) & 0xff, (fg >> s) & 0xff);
        ((b * (256 - a) + f * a) >> 8) << s
    };
    ch(16) | ch(8) | ch(0)
}

impl Canvas {
    pub fn new(width: usize, height: usize, color: u32) -> Self {
        Canvas { width, height, pixels: vec![color; width * height] }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    pub fn into_pixels(self) -> Vec<u32> {
        self.pixels
    }

    pub fn row_mut(&mut self, y: usize) -> &mut [u32] {
        &mut self.pixels[y * self.width..(y + 1) * self.width]
    }

    pub fn get(&self, x: usize, y: usize) -> u32 {
        self.pixels[y * self.width + x]
    }

    pub fn set(&mut self, x: i64, y: i64, color: u32) {
        if x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height {
            self.pixels[y as usize * self.width + x as usize] = color;
        }
    }

    pub fn blend(&mut self, x: i64, y: i64, color: u32, alpha: f32) {
        if alpha > 0.0 && x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height {
            let p = &mut self.pixels[y as usize * self.width + x as usize];
            *p = mix(*p, color, alpha);
        }
    }

    /// Blends a rectangle, clipped to the canvas.
    pub fn fill(&mut self, x: i64, y: i64, w: i64, h: i64, color: u32, alpha: f32) {
        let x0 = x.max(0) as usize;
        let y0 = y.max(0) as usize;
        let x1 = (x + w).clamp(0, self.width as i64) as usize;
        let y1 = (y + h).clamp(0, self.height as i64) as usize;
        for yy in y0..y1 {
            for p in &mut self.pixels[yy * self.width + x0..yy * self.width + x1.max(x0)] {
                *p = if alpha >= 1.0 { color } else { mix(*p, color, alpha) };
            }
        }
    }

    /// A rectangle outline `t` pixels wide drawn just outside `(x, y, w, h)`.
    pub fn frame(&mut self, x: i64, y: i64, w: i64, h: i64, t: i64, color: u32) {
        self.fill(x - t, y - t, w + 2 * t, t, color, 1.0);
        self.fill(x - t, y + h, w + 2 * t, t, color, 1.0);
        self.fill(x - t, y, t, h, color, 1.0);
        self.fill(x + w, y, t, h, color, 1.0);
    }
}
