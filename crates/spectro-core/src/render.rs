//! Lays out and draws the whole picture: header, spectrogram, rulers and the
//! palette legend. The layout follows Spek's so the two read the same.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::analysis::Spectrogram;
use crate::canvas::Canvas;
use crate::font::{Text, Weight};
use crate::math;
use crate::palette::{Lut, Palette};

pub const MIN_DB: i32 = -140;
pub const MAX_DB: i32 = 0;
pub const DEFAULT_UPPER_DB: i32 = 0;
pub const DEFAULT_LOWER_DB: i32 = -120;

const BACKGROUND: u32 = 0x0b0b0e;
const TEXT: u32 = 0xeceef2;
const TEXT_DIM: u32 = 0x9a9eaa;
const LINE: u32 = 0x5c606c;

/// Everything the renderer needs to know about what to show.
pub struct View<'a> {
    /// Analysed data, if any. Its column count need not match the plot width.
    pub spectrogram: Option<&'a Spectrogram>,
    /// Large header line, usually the file path.
    pub title: &'a str,
    /// Second header line: codec, rate, channel, window.
    pub description: &'a str,
    /// Seconds; 0 hides the time ruler.
    pub duration: f64,
    /// Hz; 0 hides the frequency ruler.
    pub sample_rate: u32,
    pub palette: Palette,
    pub upper_db: i32,
    pub lower_db: i32,
    /// Device pixels per layout unit (2 on a Retina display).
    pub scale: f32,
    /// Centred message shown over the plot area, e.g. a hint or an error.
    pub message: Option<&'a str>,
    /// Panel listing `(key, description)` rows, e.g. help or settings.
    pub panel: Option<(&'a str, &'a [(String, String)])>,
    /// Version string shown next to the application name.
    pub version: &'a str,
}

/// Plot rectangle in device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plot {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

struct Metrics {
    lpad: f32,
    tpad: f32,
    rpad: f32,
    bpad: f32,
    gap: f32,
    ruler: f32,
    tick: f32,
    line: i64,
    small: f32,
    normal: f32,
    large: f32,
}

impl Metrics {
    fn new(scale: f32) -> Self {
        let s = scale.max(0.5);
        Metrics {
            lpad: 64.0 * s,
            tpad: 64.0 * s,
            rpad: 96.0 * s,
            bpad: 42.0 * s,
            gap: 10.0 * s,
            ruler: 10.0 * s,
            tick: 4.0 * s,
            line: (s + 0.25).max(1.0) as i64,
            small: 11.5 * s,
            normal: 13.0 * s,
            large: 14.5 * s,
        }
    }
}

/// The plot rectangle for a canvas of the given size, which is also the
/// number of spectrogram columns to compute.
pub fn plot_rect(width: usize, height: usize, scale: f32) -> Plot {
    let m = Metrics::new(scale);
    let x = m.lpad as i64;
    let y = m.tpad as i64;
    Plot { x, y, w: (width as i64 - x - m.rpad as i64).max(0), h: (height as i64 - y - m.bpad as i64).max(0) }
}

pub struct Renderer {
    text: Text,
    lut: Lut,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a ruler sits relative to the edge it labels.
#[derive(Clone, Copy, PartialEq)]
enum Side {
    Left,
    Right,
    Bottom,
}

struct Ruler<'a> {
    x: f32,
    y: f32,
    side: Side,
    sample: &'a str,
    factors: &'a [i32],
    min: i32,
    max: i32,
    spacing: f32,
    /// Pixels per unit, measured from `offset`.
    scale: f32,
    offset: f32,
    format: &'a dyn Fn(i32) -> String,
}

impl Renderer {
    pub fn new() -> Self {
        Renderer { text: Text::new(), lut: Lut::new(Palette::default()) }
    }

    pub fn render(&mut self, c: &mut Canvas, v: &View) {
        if self.lut.palette() != v.palette {
            self.lut = Lut::new(v.palette);
        }
        let m = Metrics::new(v.scale);
        let (w, h) = (c.width() as f32, c.height() as f32);
        c.fill(0, 0, c.width() as i64, c.height() as i64, BACKGROUND, 1.0);
        let p = plot_rect(c.width(), c.height(), v.scale);
        let normal_h = self.text.line_height(Weight::Regular, m.normal);
        let large_h = self.text.line_height(Weight::Semibold, m.large);

        // Application name and version, top right.
        let name_y = m.tpad - 2.0 * m.gap - normal_h - large_h;
        let ax = w - m.rpad + m.gap;
        let aw = self.text.draw(c, Weight::Semibold, m.large, (ax, name_y), TEXT, "spectro");
        let dy = self.text.ascent(Weight::Semibold, m.large) - self.text.ascent(Weight::Regular, m.small);
        self.text.draw(c, Weight::Regular, m.small, (ax + aw + 0.3 * m.gap, name_y + dy), TEXT_DIM, v.version);

        if p.w > 0 && p.h > 0 {
            if let Some(s) = v.spectrogram {
                self.draw_spectrogram(c, p, s, v);
            }
            let room = p.w as f32;
            let title = self.text.fit(Weight::Semibold, m.large, v.title, room, false);
            self.text.draw(c, Weight::Semibold, m.large, (m.lpad, name_y), TEXT, &title);
            let desc = self.text.fit(Weight::Regular, m.normal, v.description, room, true);
            self.text.draw(c, Weight::Regular, m.normal, (m.lpad, m.tpad - m.gap - normal_h), TEXT_DIM, &desc);

            if v.duration > 0.0 {
                let time = |t: i32| format!("{}:{:02}", t / 60, t % 60);
                self.ruler(
                    c,
                    &m,
                    &Ruler {
                        x: p.x as f32,
                        y: (p.y + p.h) as f32,
                        side: Side::Bottom,
                        sample: "00:00",
                        factors: &[1, 2, 5, 10, 20, 30, 60, 120, 300, 600, 1200, 1800, 3600, 7200],
                        min: 0,
                        max: v.duration as i32,
                        spacing: 1.5,
                        scale: p.w as f32 / v.duration as f32,
                        offset: 0.0,
                        format: &time,
                    },
                );
            }
            if v.sample_rate > 0 {
                let nyquist = (v.sample_rate / 2) as i32;
                let khz = |f: i32| format!("{} kHz", f / 1000);
                self.ruler(
                    c,
                    &m,
                    &Ruler {
                        x: p.x as f32,
                        y: p.y as f32,
                        side: Side::Left,
                        sample: "00 kHz",
                        factors: &[1000, 2000, 5000, 10000, 20000, 50000],
                        min: 0,
                        max: nyquist,
                        spacing: 3.0,
                        scale: -(p.h as f32) / nyquist as f32,
                        offset: p.h as f32,
                        format: &khz,
                    },
                );
            }
        }
        c.frame(p.x, p.y, p.w, p.h, m.line, LINE);

        // Palette legend and the dB ruler beside it.
        if p.h > 0 {
            let lx = (w - m.rpad + m.gap) as i64;
            let lw = m.ruler as i64;
            for y in 0..p.h {
                let level = 1.0 - y as f32 / (p.h - 1).max(1) as f32;
                for x in 0..lw {
                    let col = self.lut.color(level, x as usize, y as usize);
                    c.set(lx + x, p.y + y, col);
                }
            }
            c.frame(lx, p.y, lw, p.h, m.line, LINE);
            let db = |d: i32| format!("{} dB", -d);
            self.ruler(
                c,
                &m,
                &Ruler {
                    x: (lx + lw) as f32,
                    y: p.y as f32,
                    side: Side::Right,
                    sample: "-00 dB",
                    factors: &[1, 2, 5, 10, 20, 50],
                    min: -v.upper_db,
                    max: -v.lower_db,
                    spacing: 3.0,
                    scale: p.h as f32 / (v.upper_db - v.lower_db) as f32,
                    offset: 0.0,
                    format: &db,
                },
            );
        }

        if let Some(msg) = v.message {
            let tw = self.text.width(Weight::Regular, m.normal, msg);
            let cx = p.x as f32 + (p.w as f32 - tw) / 2.0;
            let cy = p.y as f32 + (p.h as f32 - normal_h) / 2.0;
            self.text.draw(c, Weight::Regular, m.normal, (cx.max(m.gap), cy.max(0.0)), TEXT_DIM, msg);
        }
        if let Some((title, rows)) = v.panel {
            self.panel(c, &m, w, h, title, rows);
        }
    }

    fn draw_spectrogram(&mut self, c: &mut Canvas, p: Plot, s: &Spectrogram, v: &View) {
        let bands = s.bands();
        if bands < 2 || s.columns() == 0 || s.filled() == 0 {
            return;
        }
        // For each pixel row, the band interval it covers (low frequency at
        // the bottom). Rows spanning several bands take the loudest, so thin
        // tones survive; rows inside one band interpolate smoothly.
        let rows: Vec<(f32, usize, usize)> = (0..p.h)
            .map(|y| {
                let top = (p.h - y) as f32 / p.h as f32 * (bands - 1) as f32;
                let bottom = (p.h - y - 1) as f32 / p.h as f32 * (bands - 1) as f32;
                let mid = 0.5 * (top + bottom);
                let lo = math::ceilf(bottom) as usize;
                let hi = math::floorf(top) as usize;
                (mid, lo, hi.min(bands - 1))
            })
            .collect();
        let range = (v.upper_db - v.lower_db) as f32;
        let lower = v.lower_db as f32;
        let cols = s.columns();
        for x in 0..p.w as usize {
            let col_index = x * cols / p.w as usize;
            if col_index >= s.filled() {
                break;
            }
            let col = s.column(col_index);
            for (y, &(mid, lo, hi)) in rows.iter().enumerate() {
                let db = if hi > lo {
                    col[lo..=hi].iter().copied().fold(f32::MIN, f32::max)
                } else {
                    let i = (mid as usize).min(bands - 2);
                    let t = mid - i as f32;
                    col[i] + (col[i + 1] - col[i]) * t
                };
                let level = (db - lower) / range;
                let color = self.lut.color(level, x, y);
                c.set(p.x + x as i64, p.y + y as i64, color);
            }
        }
    }

    fn ruler(&mut self, c: &mut Canvas, m: &Metrics, r: &Ruler) {
        let vertical = r.side != Side::Bottom;
        let len = if vertical {
            self.text.line_height(Weight::Regular, m.small)
        } else {
            self.text.width(Weight::Regular, m.small, r.sample)
        };
        let factor = r.factors.iter().copied().find(|&f| (r.scale * f as f32).abs() >= r.spacing * len);
        self.tick(c, m, r, r.min);
        self.tick(c, m, r, r.max);
        if let Some(f) = factor {
            let mut t = (r.min / f + 1) * f;
            while t < r.max {
                if (r.scale * (r.max - t) as f32).abs() < len * 1.2 {
                    break;
                }
                self.tick(c, m, r, t);
                t += f;
            }
        }
    }

    fn tick(&mut self, c: &mut Canvas, m: &Metrics, r: &Ruler, value: i32) {
        let label = (r.format)(value);
        let label = label.replace('-', "\u{2212}");
        let pos = r.offset + r.scale * (value - r.min) as f32;
        let tw = self.text.width(Weight::Regular, m.small, &label);
        let th = self.text.line_height(Weight::Regular, m.small);
        let t = m.line;
        let tick = m.tick as i64;
        let pi = math::round(pos as f64) as i64;
        let (lx, ly) = match r.side {
            Side::Bottom => {
                c.fill(r.x as i64 + pi - t / 2, r.y as i64 + t, t, tick, LINE, 1.0);
                (r.x + pos - tw / 2.0, r.y + m.gap)
            }
            Side::Left => {
                c.fill(r.x as i64 - tick - t, r.y as i64 + pi - t / 2, tick, t, LINE, 1.0);
                (r.x - tw - m.gap, r.y + pos - th / 2.0)
            }
            Side::Right => {
                c.fill(r.x as i64 + t, r.y as i64 + pi - t / 2, tick, t, LINE, 1.0);
                (r.x + m.gap, r.y + pos - th / 2.0)
            }
        };
        self.text.draw(c, Weight::Regular, m.small, (lx, ly), TEXT_DIM, &label);
    }

    fn panel(&mut self, c: &mut Canvas, m: &Metrics, w: f32, h: f32, title: &str, rows: &[(String, String)]) {
        let lh = self.text.line_height(Weight::Regular, m.normal) * 1.35;
        let key_w = rows.iter().map(|(k, _)| self.text.width(Weight::Semibold, m.normal, k)).fold(0.0, f32::max);
        let val_w = rows.iter().map(|(_, d)| self.text.width(Weight::Regular, m.normal, d)).fold(0.0, f32::max);
        let title_h = self.text.line_height(Weight::Semibold, m.large) * 1.6;
        let pw = (key_w + val_w + 5.0 * m.gap).max(self.text.width(Weight::Semibold, m.large, title) + 4.0 * m.gap);
        let ph = title_h + lh * rows.len() as f32 + 3.0 * m.gap;
        let px = ((w - pw) / 2.0).max(0.0);
        let py = ((h - ph) / 2.0).max(0.0);
        c.fill(0, 0, c.width() as i64, c.height() as i64, 0x000000, 0.45);
        c.fill(px as i64, py as i64, pw as i64, ph as i64, 0x16171c, 0.96);
        c.frame(px as i64, py as i64, pw as i64, ph as i64, m.line, 0x3a3d47);
        self.text.draw(c, Weight::Semibold, m.large, (px + 2.0 * m.gap, py + 1.5 * m.gap), TEXT, title);
        let mut y = py + 1.5 * m.gap + title_h;
        for (k, d) in rows {
            self.text.draw(c, Weight::Semibold, m.normal, (px + 2.0 * m.gap, y), TEXT, k);
            self.text.draw(c, Weight::Regular, m.normal, (px + 3.0 * m.gap + key_w, y), TEXT_DIM, d);
            y += lh;
        }
    }
}
