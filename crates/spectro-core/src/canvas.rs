//! A plain `0x00RRGGBB` pixel buffer with the few drawing operations the
//! renderer needs.

use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone)]
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

    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
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

/// Signed distance from `(px, py)` to a rounded rectangle (negative inside).
fn rounded_distance(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let (hw, hh) = (w * 0.5, h * 0.5);
    let r = r.min(hw).min(hh).max(0.0);
    let qx = (px - (x + hw)).abs() - (hw - r);
    let qy = (py - (y + hh)).abs() - (hh - r);
    let (ox, oy) = (qx.max(0.0), qy.max(0.0));
    // Only the corners need the square root.
    let outside = if ox == 0.0 || oy == 0.0 { ox + oy } else { crate::math::sqrt((ox * ox + oy * oy) as f64) as f32 };
    outside + qx.max(qy).min(0.0) - r
}

/// A rounded rectangle in device pixels, possibly fractional.
#[derive(Clone, Copy, Debug)]
pub struct RoundRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub r: f32,
}

impl RoundRect {
    fn distance(&self, px: f32, py: f32) -> f32 {
        rounded_distance(px, py, self.x, self.y, self.w, self.h, self.r)
    }

    fn bounds(&self, pad: f32, c: &Canvas) -> (usize, usize, usize, usize) {
        let x0 = crate::math::floorf(self.x - pad).max(0.0) as usize;
        let y0 = crate::math::floorf(self.y - pad).max(0.0) as usize;
        let x1 = (crate::math::ceilf(self.x + self.w + pad).max(0.0) as usize).min(c.width);
        let y1 = (crate::math::ceilf(self.y + self.h + pad).max(0.0) as usize).min(c.height);
        (x0, y0, x1, y1)
    }
}

impl Canvas {
    /// Fills a rounded rectangle with anti-aliased edges.
    pub fn fill_round(&mut self, rr: RoundRect, color: u32, alpha: f32) {
        let (x0, y0, x1, y1) = rr.bounds(1.0, self);
        for y in y0..y1 {
            for x in x0..x1 {
                let cov = (0.5 - rr.distance(x as f32 + 0.5, y as f32 + 0.5)).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let p = &mut self.pixels[y * self.width + x];
                    *p = mix(*p, color, cov * alpha);
                }
            }
        }
    }

    /// Strokes the inside edge of a rounded rectangle, `t` pixels wide.
    pub fn stroke_round(&mut self, rr: RoundRect, t: f32, color: u32, alpha: f32) {
        let (x0, y0, x1, y1) = rr.bounds(1.0, self);
        // Pixels further than this from every edge are untouched.
        let m = crate::math::ceilf(rr.r + t) as usize + 2;
        for y in y0..y1 {
            let middle = y >= y0 + m && y + m < y1;
            for x in x0..x1 {
                if middle && x >= x0 + m && x + m < x1 {
                    continue;
                }
                let d = rr.distance(x as f32 + 0.5, y as f32 + 0.5);
                let cov = (t * 0.5 + 0.5 - (d + t * 0.5).abs()).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let p = &mut self.pixels[y * self.width + x];
                    *p = mix(*p, color, cov * alpha);
                }
            }
        }
    }

    /// Replaces everything outside the rounded corners of `rr` (within its
    /// bounding box) with `outside(y)`, anti-aliased.
    pub fn clip_round(&mut self, rr: RoundRect, outside: impl Fn(usize, usize) -> u32) {
        let (x0, y0, x1, y1) = rr.bounds(0.0, self);
        let r = crate::math::ceilf(rr.r) as usize + 1;
        for y in y0..y1 {
            let near_y = y < y0 + r || y + r >= y1;
            if !near_y {
                continue;
            }
            for x in x0..x1 {
                if x >= x0 + r && x + r < x1 {
                    continue;
                }
                let inside = (0.5 - rr.distance(x as f32 + 0.5, y as f32 + 0.5)).clamp(0.0, 1.0);
                if inside < 1.0 {
                    let p = &mut self.pixels[y * self.width + x];
                    *p = mix(outside(x, y), *p, inside);
                }
            }
        }
    }
}
