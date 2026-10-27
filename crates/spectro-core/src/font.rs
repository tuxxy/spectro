//! Anti-aliased text from embedded glyph outlines.
//!
//! The outlines come from Source Sans 3 (SIL Open Font License 1.1, see
//! `assets/OFL-SourceSans3.md`), extracted by `tools/fontgen.py`. Glyphs are
//! rasterised with exact-area coverage (signed area accumulation) at
//! quarter-pixel horizontal positions and cached.

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use crate::canvas::Canvas;
use crate::math::{ceilf, floorf};

static REGULAR: &[u8] = include_bytes!("../assets/sans-regular.bin");
static SEMIBOLD: &[u8] = include_bytes!("../assets/sans-semibold.bin");

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Weight {
    Regular,
    Semibold,
}

struct Glyph {
    advance: u16,
    /// Contours as `(x, y, on_curve)` in font units, y up.
    contours: Vec<Vec<(f32, f32, bool)>>,
}

struct Face {
    upem: f32,
    ascender: f32,
    descender: f32,
    glyphs: Vec<(u32, Glyph)>,
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        out.copy_from_slice(&self.data[self.pos..self.pos + N]);
        self.pos += N;
        out
    }
    fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }
    fn i16(&mut self) -> i16 {
        i16::from_le_bytes(self.take())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
}

impl Face {
    fn parse(data: &[u8]) -> Face {
        let mut r = Reader { data, pos: 0 };
        assert_eq!(&r.take::<4>(), b"SPF1", "bad font data");
        let upem = r.u16() as f32;
        let ascender = r.i16() as f32;
        let descender = r.i16() as f32;
        let count = r.u16() as usize;
        let mut glyphs = Vec::with_capacity(count);
        for _ in 0..count {
            let cp = r.u32();
            let advance = r.u16();
            let nc = r.u16() as usize;
            let contours = (0..nc)
                .map(|_| {
                    let np = r.u16() as usize;
                    (0..np).map(|_| (r.i16() as f32, r.i16() as f32, r.u8() & 1 == 1)).collect()
                })
                .collect();
            glyphs.push((cp, Glyph { advance, contours }));
        }
        glyphs.sort_by_key(|g| g.0);
        Face { upem, ascender, descender, glyphs }
    }

    fn glyph(&self, c: char) -> Option<&Glyph> {
        let find = |cp: u32| self.glyphs.binary_search_by_key(&cp, |g| g.0).ok();
        find(c as u32).or_else(|| find('?' as u32)).map(|i| &self.glyphs[i].1)
    }
}

/// A rendered glyph: coverage values and the offset of its top-left corner
/// from the pen position on the baseline.
struct Bitmap {
    left: i32,
    top: i32,
    width: usize,
    height: usize,
    coverage: Vec<u8>,
}

/// Signed-area accumulation rasteriser, after the one in Raph Levien's
/// font-rs (Apache-2.0).
struct Raster {
    w: usize,
    h: usize,
    acc: Vec<f32>,
}

impl Raster {
    fn new(w: usize, h: usize) -> Self {
        Raster { w, h, acc: vec![0.0; w * h + 4] }
    }

    fn add(&mut self, i: isize, v: f32) {
        if i >= 0 && (i as usize) < self.acc.len() {
            self.acc[i as usize] += v;
        }
    }

    fn line(&mut self, (x0, y0): (f32, f32), (x1, y1): (f32, f32)) {
        if (y0 - y1).abs() < 1e-6 {
            return;
        }
        let (dir, (x0, y0), (x1, y1)) = if y0 < y1 { (1.0, (x0, y0), (x1, y1)) } else { (-1.0, (x1, y1), (x0, y0)) };
        let dxdy = (x1 - x0) / (y1 - y0);
        let mut x = x0;
        if y0 < 0.0 {
            x -= y0 * dxdy;
        }
        let ystart = y0.max(0.0) as usize;
        let yend = (ceilf(y1).max(0.0) as usize).min(self.h);
        for y in ystart..yend {
            let row = (y * self.w) as isize;
            let dy = ((y + 1) as f32).min(y1) - (y as f32).max(y0);
            let xnext = x + dxdy * dy;
            let d = dy * dir;
            let (xa, xb) = if x < xnext { (x, xnext) } else { (xnext, x) };
            let xa_floor = floorf(xa);
            let xai = xa_floor as isize;
            let xbi = ceilf(xb) as isize;
            if xbi <= xai + 1 {
                let xm = 0.5 * (x + xnext) - xa_floor;
                self.add(row + xai, d - d * xm);
                self.add(row + xai + 1, d * xm);
            } else {
                let s = 1.0 / (xb - xa);
                let xaf = xa - xa_floor;
                let a0 = 0.5 * s * (1.0 - xaf) * (1.0 - xaf);
                let xbf = xb - ceilf(xb) + 1.0;
                let am = 0.5 * s * xbf * xbf;
                self.add(row + xai, d * a0);
                if xbi == xai + 2 {
                    self.add(row + xai + 1, d * (1.0 - a0 - am));
                } else {
                    let a1 = s * (1.5 - xaf);
                    self.add(row + xai + 1, d * (a1 - a0));
                    for xi in xai + 2..xbi - 1 {
                        self.add(row + xi, d * s);
                    }
                    let a2 = a1 + (xbi - xai - 3) as f32 * s;
                    self.add(row + xbi - 1, d * (1.0 - a2 - am));
                }
                self.add(row + xbi, d * am);
            }
            x = xnext;
        }
    }

    fn quad(&mut self, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)) {
        let dev = ((p0.0 - 2.0 * p1.0 + p2.0).abs() + (p0.1 - 2.0 * p1.1 + p2.1).abs()) * 0.5;
        let n = (1 + (crate::math::sqrt(dev as f64 * 4.0) as usize)).min(32);
        let mut prev = p0;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let p =
                (u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0, u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1);
            self.line(prev, p);
            prev = p;
        }
    }

    fn coverage(self) -> Vec<u8> {
        let mut sum = 0.0;
        self.acc[..self.w * self.h]
            .iter()
            .map(|a| {
                sum += a;
                // A light gamma lift keeps thin strokes of light-on-dark text full.
                let c = sum.abs().min(1.0);
                let c = c * (1.25 - 0.25 * c);
                (c * 255.0 + 0.5) as u8
            })
            .collect()
    }
}

fn rasterize(face: &Face, glyph: &Glyph, px: f32, dx: f32) -> Bitmap {
    let s = px / face.upem;
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for c in &glyph.contours {
        for &(x, y, _) in c {
            x0 = x0.min(x * s + dx);
            x1 = x1.max(x * s + dx);
            y0 = y0.min(-y * s);
            y1 = y1.max(-y * s);
        }
    }
    if x0 > x1 {
        return Bitmap { left: 0, top: 0, width: 0, height: 0, coverage: Vec::new() };
    }
    let left = floorf(x0) as i32 - 1;
    let top = floorf(y0) as i32 - 1;
    let width = (ceilf(x1) as i32 - left + 2) as usize;
    let height = (ceilf(y1) as i32 - top + 2) as usize;
    let mut r = Raster::new(width, height);
    let map = |(x, y, on): (f32, f32, bool)| ((x * s + dx - left as f32, -y * s - top as f32), on);
    for c in &glyph.contours {
        if c.len() < 2 {
            continue;
        }
        let pts: Vec<((f32, f32), bool)> = c.iter().map(|&p| map(p)).collect();
        let mid = |a: (f32, f32), b: (f32, f32)| ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
        // Start on an on-curve point (or an implied one between two off-curve points).
        let n = pts.len();
        let first = pts.iter().position(|p| p.1);
        let (start, rot) = match first {
            Some(i) => (pts[i].0, i),
            None => (mid(pts[0].0, pts[1].0), 0),
        };
        let mut pen = start;
        let mut ctrl: Option<(f32, f32)> = None;
        for k in 1..=n {
            let (p, on) = pts[(rot + k) % n];
            match (on, ctrl) {
                (true, None) => {
                    r.line(pen, p);
                    pen = p;
                }
                (true, Some(c)) => {
                    r.quad(pen, c, p);
                    pen = p;
                    ctrl = None;
                }
                (false, None) => ctrl = Some(p),
                (false, Some(c)) => {
                    let m = mid(c, p);
                    r.quad(pen, c, m);
                    pen = m;
                    ctrl = Some(p);
                }
            }
        }
        match ctrl {
            Some(c) => r.quad(pen, c, start),
            None => r.line(pen, start),
        }
    }
    Bitmap { left, top, width, height, coverage: r.coverage() }
}

/// Text layout and drawing with a glyph cache.
pub struct Text {
    faces: [Face; 2],
    cache: BTreeMap<(Weight, u32, u32, u8), Bitmap>,
}

impl Default for Text {
    fn default() -> Self {
        Self::new()
    }
}

impl Text {
    pub fn new() -> Self {
        Text { faces: [Face::parse(REGULAR), Face::parse(SEMIBOLD)], cache: BTreeMap::new() }
    }

    fn face(&self, w: Weight) -> &Face {
        &self.faces[w as usize]
    }

    /// Distance from the top of a line to its baseline.
    pub fn ascent(&self, w: Weight, px: f32) -> f32 {
        self.face(w).ascender * px / self.face(w).upem
    }

    /// Height of a line of text.
    pub fn line_height(&self, w: Weight, px: f32) -> f32 {
        let f = self.face(w);
        (f.ascender - f.descender) * px / f.upem
    }

    pub fn width(&self, w: Weight, px: f32, s: &str) -> f32 {
        let f = self.face(w);
        s.chars().filter_map(|c| f.glyph(c)).map(|g| g.advance as f32 * px / f.upem).sum()
    }

    /// Draws `s` with the top of its line box at `(x, y)`; returns the width.
    pub fn draw(&mut self, canvas: &mut Canvas, w: Weight, px: f32, (x, y): (f32, f32), color: u32, s: &str) -> f32 {
        let baseline = (y + self.ascent(w, px) + 0.5) as i64;
        let size_key = (px * 64.0) as u32;
        let mut pen = x;
        for ch in s.chars() {
            let face = &self.faces[w as usize];
            let Some(glyph) = face.glyph(ch) else { continue };
            let advance = glyph.advance as f32 * px / face.upem;
            let ix = floorf(pen);
            let sub = ((pen - ix) * 4.0) as u8 & 3;
            let key = (w, ch as u32, size_key, sub);
            if !self.cache.contains_key(&key) {
                let face = &self.faces[w as usize];
                let bm = rasterize(face, face.glyph(ch).unwrap(), px, sub as f32 / 4.0);
                self.cache.insert(key, bm);
            }
            let bm = &self.cache[&key];
            for gy in 0..bm.height {
                for gx in 0..bm.width {
                    let a = bm.coverage[gy * bm.width + gx];
                    if a > 0 {
                        canvas.blend(
                            ix as i64 + bm.left as i64 + gx as i64,
                            baseline + bm.top as i64 + gy as i64,
                            color,
                            a as f32 / 255.0,
                        );
                    }
                }
            }
            pen += advance;
        }
        pen - x
    }

    /// Shortens `s` with an ellipsis so it fits in `max` pixels, keeping the
    /// start (`keep_start`) or the end.
    pub fn fit(&self, w: Weight, px: f32, s: &str, max: f32, keep_start: bool) -> alloc::string::String {
        use alloc::string::String;
        if self.width(w, px, s) <= max {
            return String::from(s);
        }
        let chars: Vec<char> = s.chars().collect();
        let build = |k: usize| -> String {
            if keep_start {
                chars[..k].iter().chain(['\u{2026}'].iter()).collect()
            } else {
                core::iter::once(&'\u{2026}').chain(chars[chars.len() - k..].iter()).collect()
            }
        };
        let (mut lo, mut hi) = (0, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.width(w, px, &build(mid)) <= max { lo = mid } else { hi = mid - 1 }
        }
        if lo == 0 && self.width(w, px, "\u{2026}") > max { String::new() } else { build(lo) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_cover_expected_area() {
        let t = Text::new();
        let f = t.face(Weight::Regular);
        // An 'l' is a vertical bar: its coverage should sum to roughly
        // stem width times height.
        let bm = rasterize(f, f.glyph('l').unwrap(), 40.0, 0.0);
        let total: f32 = bm.coverage.iter().map(|&c| c as f32 / 255.0).sum();
        assert!(total > 50.0 && total < 200.0, "{total}");
        // Coverage of a box glyph never exceeds one per pixel and is filled inside.
        let o = rasterize(f, f.glyph('o').unwrap(), 40.0, 0.25);
        assert!(o.coverage.contains(&255));
        assert!(t.width(Weight::Regular, 13.0, "00 kHz") > 20.0);
    }

    #[test]
    fn fit_trims_with_ellipsis() {
        let t = Text::new();
        let s = "a very long file name that will not fit.flac";
        let short = t.fit(Weight::Regular, 13.0, s, 80.0, false);
        assert!(short.starts_with('\u{2026}') && short.ends_with(".flac"));
        assert!(t.width(Weight::Regular, 13.0, &short) <= 80.0);
        assert_eq!(t.fit(Weight::Regular, 13.0, "ok", 80.0, true), "ok");
    }
}
