//! Lays out and draws the whole picture.
//!
//! The spectrogram sits in a rounded well on a dark gradient, under a header
//! (file name, folder, and the stream's details as small pills) and a
//! loudness strip. Grid lines run through the plot at the labelled values;
//! each unit is written once. Loud components can bloom softly, and a
//! scanline marks progress while the analysis runs.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::analysis::Spectrogram;
use crate::canvas::{Canvas, RoundRect, mix};
use crate::font::{Text, Weight};
use crate::math;
use crate::palette::{Lut, Palette, quantize};

pub const MIN_DB: i32 = -140;
pub const MAX_DB: i32 = 0;
pub const DEFAULT_UPPER_DB: i32 = 0;
pub const DEFAULT_LOWER_DB: i32 = -120;

const BG_TOP: [f32; 3] = [18.0, 19.0, 27.0];
const BG_BOTTOM: [f32; 3] = [7.0, 7.0, 10.0];
const TEXT: u32 = 0xf1f2f6;
const TEXT_DIM: u32 = 0x8d91a0;
const TEXT_FAINT: u32 = 0x5f6372;
const WELL: u32 = 0x0e0f15;
const WHITE: u32 = 0xffffff;
const CARD: u32 = 0x17181f;

/// How frequency maps to height.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FreqScale {
    /// Equal height per hertz, as in Spek. Best for the top octave, where
    /// encoder cutoffs and resampling show.
    #[default]
    Linear,
    /// Perceptual: close to linear at low frequencies, logarithmic above
    /// about 1 kHz. Music fills the picture; the top octave gets compressed.
    Mel,
    /// Equal height per octave, from 20 Hz.
    Log,
}

const LOG_MIN_HZ: f64 = 20.0;

fn mel(f: f64) -> f64 {
    2595.0 * math::log10(1.0 + f / 700.0)
}

impl FreqScale {
    pub const ALL: [FreqScale; 3] = [FreqScale::Linear, FreqScale::Mel, FreqScale::Log];

    pub fn name(self) -> &'static str {
        match self {
            FreqScale::Mel => "Mel",
            FreqScale::Linear => "Linear",
            FreqScale::Log => "Logarithmic",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            FreqScale::Mel => "mel",
            FreqScale::Linear => "linear",
            FreqScale::Log => "log",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.id() == s)
    }

    pub fn next(self) -> Self {
        crate::cycle(&Self::ALL, self, 1)
    }

    pub fn prev(self) -> Self {
        crate::cycle(&Self::ALL, self, -1)
    }

    /// Frequency at height fraction `u` (0 at the bottom, 1 at the top).
    pub fn freq(self, u: f64, nyquist: f64) -> f64 {
        match self {
            FreqScale::Linear => u * nyquist,
            FreqScale::Mel => 700.0 * (math::exp(u * mel(nyquist) / 2595.0 * core::f64::consts::LN_10) - 1.0),
            FreqScale::Log => {
                let lo = LOG_MIN_HZ.min(nyquist * 0.5);
                lo * math::exp(u * math::ln(nyquist / lo))
            }
        }
    }

    /// Height fraction of frequency `f`; the inverse of [`FreqScale::freq`].
    pub fn frac(self, f: f64, nyquist: f64) -> f64 {
        match self {
            FreqScale::Linear => f / nyquist,
            FreqScale::Mel => mel(f) / mel(nyquist),
            FreqScale::Log => {
                let lo = LOG_MIN_HZ.min(nyquist * 0.5);
                if f <= 0.0 { f64::NEG_INFINITY } else { math::ln(f / lo) / math::ln(nyquist / lo) }
            }
        }
    }
}

/// Everything the renderer needs to know about what to show.
pub struct View<'a> {
    /// Analysed data, if any. Its column count need not match the plot width.
    pub spectrogram: Option<&'a Spectrogram>,
    /// The file path; shown as a large name and a dim folder.
    pub title: &'a str,
    /// Comma-separated details (codec, rate, channel, window), shown as pills.
    pub description: &'a str,
    /// Seconds; 0 hides the time axis.
    pub duration: f64,
    /// Hz; 0 hides the frequency axis.
    pub sample_rate: u32,
    pub palette: Palette,
    pub upper_db: i32,
    pub lower_db: i32,
    /// Device pixels per layout unit (2 on a Retina display).
    pub scale: f32,
    /// Centred message over the plot; a first line, then dimmer lines.
    pub message: Option<&'a str>,
    /// Panel listing `(key, description)` rows, e.g. help or settings.
    pub panel: Option<(&'a str, &'a [(String, String)])>,
    /// Version string for the about panel and the like.
    pub version: &'a str,
    /// Let loud components bloom.
    pub glow: bool,
    pub freq_scale: FreqScale,
    /// Pointer position in device pixels, for the crosshair readout.
    pub cursor: Option<(f32, f32)>,
    /// A selected time and frequency region, e.g. for looped playback.
    pub selection: Option<Selection>,
}

/// A region of the spectrogram in seconds and hertz.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection {
    pub t0: f64,
    pub t1: f64,
    pub f0: f64,
    pub f1: f64,
    /// Seconds into the region where playback is, if it is playing.
    pub playhead: Option<f64>,
}

/// A rectangle in device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plot {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

impl Plot {
    fn round(&self, r: f32) -> RoundRect {
        RoundRect { x: self.x as f32, y: self.y as f32, w: self.w as f32, h: self.h as f32, r }
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x as f32 && y >= self.y as f32 && x < (self.x + self.w) as f32 && y < (self.y + self.h) as f32
    }
}

/// Font sizes and positions, in device pixels.
struct Layout {
    s: f32,
    margin: f32,
    title_y: f32,
    chips_y: f32,
    chip_h: f32,
    envelope: Plot,
    plot: Plot,
    legend_x: f32,
    legend_w: f32,
    radius: f32,
    hair: f32,
    title_px: f32,
    folder_px: f32,
    chip_px: f32,
    label_px: f32,
}

fn layout(width: usize, height: usize, scale: f32) -> Layout {
    let s = scale.max(0.5);
    let (w, h) = (width as f32, height as f32);
    let margin = 28.0 * s;
    let title_y = 20.0 * s;
    let chips_y = title_y + 32.0 * s;
    let chip_h = 22.0 * s;
    let env_y = chips_y + chip_h + 16.0 * s;
    let env_h = 40.0 * s;
    let left = margin + 46.0 * s;
    let legend_w = 8.0 * s;
    let right = margin + 46.0 * s + legend_w + 14.0 * s;
    let plot_y = env_y + env_h + 10.0 * s;
    let bottom = 46.0 * s;
    let pw = (w - left - right).max(0.0);
    let ph = (h - plot_y - bottom).max(0.0);
    Layout {
        s,
        margin,
        title_y,
        chips_y,
        chip_h,
        envelope: Plot { x: left as i64, y: env_y as i64, w: pw as i64, h: env_h as i64 },
        plot: Plot { x: left as i64, y: plot_y as i64, w: pw as i64, h: ph as i64 },
        legend_x: left + pw + 14.0 * s,
        legend_w,
        radius: 9.0 * s,
        hair: s.max(1.0),
        title_px: 19.0 * s,
        folder_px: 12.5 * s,
        chip_px: 11.5 * s,
        label_px: 11.0 * s,
    }
}

/// The plot rectangle for a canvas of the given size, which is also the
/// number of spectrogram columns to compute.
pub fn plot_rect(width: usize, height: usize, scale: f32) -> Plot {
    layout(width, height, scale).plot
}

/// Converts between device pixels and seconds/hertz on the plot.
#[derive(Clone, Copy, Debug)]
pub struct Axes {
    pub plot: Plot,
    duration: f64,
    nyquist: f64,
    scale: FreqScale,
}

impl Axes {
    pub fn new(width: usize, height: usize, ui_scale: f32, duration: f64, sample_rate: u32, scale: FreqScale) -> Self {
        Axes { plot: plot_rect(width, height, ui_scale), duration, nyquist: sample_rate as f64 / 2.0, scale }
    }

    /// Seconds and hertz under `(x, y)`, clamped to the plot.
    pub fn to_data(&self, x: f32, y: f32) -> (f64, f64) {
        let p = self.plot;
        let u = ((x - p.x as f32) / p.w.max(1) as f32).clamp(0.0, 1.0) as f64;
        let v = (((p.y + p.h) as f32 - y) / p.h.max(1) as f32).clamp(0.0, 1.0) as f64;
        (u * self.duration, self.scale.freq(v, self.nyquist).max(0.0))
    }

    /// Device pixel of seconds `t` and hertz `f`.
    pub fn to_px(&self, t: f64, f: f64) -> (f32, f32) {
        let p = self.plot;
        let u = if self.duration > 0.0 { t / self.duration } else { 0.0 };
        let v = self.scale.frac(f.max(1e-9), self.nyquist).clamp(0.0, 1.0);
        (p.x as f32 + (u * p.w as f64) as f32, (p.y + p.h) as f32 - (v * p.h as f64) as f32)
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.plot.contains(x, y)
    }
}

fn format_freq(f: f64) -> String {
    if f < 1000.0 { format!("{f:.0} Hz") } else { format!("{:.2} kHz", f / 1000.0) }
}

fn background(y: usize, height: usize) -> [f32; 3] {
    let t = y as f32 / height.max(1) as f32;
    let t = t * t * (3.0 - 2.0 * t);
    [0, 1, 2].map(|i| BG_TOP[i] + (BG_BOTTOM[i] - BG_TOP[i]) * t)
}

/// Picks evenly spaced, round axis values between `min` and `max` that are
/// at least `gap` pixels apart at `px` pixels per unit.
fn ticks(min: f64, max: f64, px: f64, gap: f64, steps: &[f64]) -> (f64, Vec<f64>) {
    let step = steps.iter().copied().find(|s| s * px >= gap).unwrap_or(steps[steps.len() - 1]);
    let mut out = Vec::new();
    let mut k = math::ceil(min / step - 1e-9);
    while k * step <= max + 1e-9 {
        out.push(k * step);
        k += 1.0;
    }
    (step, out)
}

fn format_time(t: f64, fine: bool) -> String {
    let cs = math::round(t.max(0.0) * 100.0) as u64;
    let (h, m, s, frac) = (cs / 360_000, cs / 6000 % 60, cs / 100 % 60, cs % 100);
    let base = if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") };
    if fine { format!("{base}.{frac:02}") } else { base }
}

/// Splits a path into its folder (with the trailing separator) and name.
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind(['/', '\\']) {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    }
}

/// Three-pass box blur (close to a Gaussian) of an RGB buffer.
fn blur(buf: &mut [[f32; 3]], tmp: &mut Vec<[f32; 3]>, w: usize, h: usize, r: usize) {
    if w == 0 || h == 0 || r == 0 {
        return;
    }
    tmp.clear();
    tmp.resize(w * h, [0.0; 3]);
    let norm = 1.0 / (2 * r + 1) as f32;
    for _ in 0..3 {
        for (n, stride, lines, line_stride) in [(w, 1, h, w), (h, w, w, 1)] {
            for line in 0..lines {
                let base = line * line_stride;
                let at = |i: isize| base + (i.clamp(0, n as isize - 1) as usize) * stride;
                let mut acc = [0.0f32; 3];
                for i in -(r as isize)..=(r as isize) {
                    let v = buf[at(i)];
                    for c in 0..3 {
                        acc[c] += v[c];
                    }
                }
                for i in 0..n as isize {
                    tmp[at(i)] = acc.map(|a| a * norm);
                    let (add, sub) = (buf[at(i + r as isize + 1)], buf[at(i - r as isize)]);
                    for c in 0..3 {
                        acc[c] += add[c] - sub[c];
                    }
                }
            }
            buf.copy_from_slice(tmp);
        }
    }
}

pub struct Renderer {
    text: Text,
    lut: Lut,
    levels: Vec<f32>,
    glow: Vec<[f32; 3]>,
    scratch: Vec<[f32; 3]>,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    pub fn new() -> Self {
        Renderer {
            text: Text::new(),
            lut: Lut::new(Palette::default()),
            levels: Vec::new(),
            glow: Vec::new(),
            scratch: Vec::new(),
        }
    }

    /// Draws the whole picture, including the crosshair.
    pub fn render(&mut self, c: &mut Canvas, v: &View) {
        self.render_base(c, v);
        self.overlay(c, v);
    }

    /// Draws the pointer crosshair and readout over a finished picture, so
    /// a front end can cache the picture and redraw only this on movement.
    pub fn overlay(&mut self, c: &mut Canvas, v: &View) {
        let l = layout(c.width(), c.height(), v.scale);
        let spec = v.spectrogram.filter(|s| s.columns() > 0 && s.bands() > 1);
        if l.plot.w <= 0 || l.plot.h <= 0 {
            return;
        }
        if let Some(sel) = v.selection.filter(|_| v.duration > 0.0 && v.sample_rate > 0) {
            self.selection(c, &l, v, sel);
        }
        if let (Some(s), Some(cur)) = (spec, v.cursor) {
            self.crosshair(c, &l, s, v, cur);
        }
    }

    /// Draws everything except the crosshair.
    pub fn render_base(&mut self, c: &mut Canvas, v: &View) {
        if self.lut.palette() != v.palette {
            self.lut = Lut::new(v.palette);
        }
        let l = layout(c.width(), c.height(), v.scale);
        let (cw, ch) = (c.width(), c.height());
        for y in 0..ch {
            let bg = background(y, ch);
            for (x, p) in c.row_mut(y).iter_mut().enumerate() {
                *p = quantize(bg, x, y);
            }
        }
        let bg_at = move |x: usize, y: usize| quantize(background(y, ch), x, y);

        self.header(c, &l, v, cw as f32);
        let p = l.plot;
        let spec = v.spectrogram.filter(|s| s.columns() > 0 && s.bands() > 1);
        if p.w > 0 && p.h > 0 {
            self.envelope(c, &l, spec);
            c.clip_round(l.envelope.round(l.radius * 0.7), bg_at);
            match spec {
                Some(s) if s.filled() > 0 => self.spectrogram(c, &l, s, v),
                _ => c.fill(p.x, p.y, p.w, p.h, WELL, 1.0),
            }
            self.grid_and_axes(c, &l, v);
            c.clip_round(p.round(l.radius), bg_at);
            c.stroke_round(p.round(l.radius), l.hair, WHITE, 0.07);
            self.legend(c, &l, v);
        }
        if let Some(msg) = v.message {
            self.message(c, &l, msg);
        }
        if let Some((title, rows)) = v.panel {
            self.panel(c, &l, title, rows);
        }
    }

    fn header(&mut self, c: &mut Canvas, l: &Layout, v: &View, w: f32) {
        let s = l.s;
        let mark = "spectro";
        let mw = self.text.width(Weight::Semibold, 13.0 * s, mark);
        let mark_x = w - l.margin - mw;
        let title_base = l.title_y + self.text.ascent(Weight::Semibold, l.title_px);
        self.text.draw(
            c,
            Weight::Semibold,
            13.0 * s,
            (mark_x, title_base - self.text.ascent(Weight::Semibold, 13.0 * s)),
            TEXT_FAINT,
            mark,
        );

        let x0 = l.plot.x as f32;
        let room = mark_x - x0 - 24.0 * s;
        if !v.title.is_empty() && room > 0.0 {
            let (folder, name) = split_path(v.title);
            let name = self.text.fit(Weight::Semibold, l.title_px, name, room, true);
            let nw = self.text.draw(c, Weight::Semibold, l.title_px, (x0, l.title_y), TEXT, &name);
            let fx = x0 + nw + 10.0 * s;
            if !folder.is_empty() && room - nw - 10.0 * s > 40.0 * s {
                let folder = self.text.fit(Weight::Regular, l.folder_px, folder, room - nw - 10.0 * s, false);
                let fy = title_base - self.text.ascent(Weight::Regular, l.folder_px);
                self.text.draw(c, Weight::Regular, l.folder_px, (fx, fy), TEXT_FAINT, &folder);
            }
        }

        // Details as pills.
        let mut chips: Vec<&str> = Vec::new();
        for part in v.description.split(", ").filter(|p| !p.is_empty()) {
            match part.split_once(": ") {
                Some((a, b)) if chips.is_empty() => {
                    chips.push(a);
                    chips.push(b);
                }
                _ => chips.push(part),
            }
        }
        let pad = 9.0 * s;
        let lh = self.text.line_height(Weight::Regular, l.chip_px);
        let mut x = x0;
        let right = (l.plot.x + l.plot.w) as f32;
        for (i, chip) in chips.iter().enumerate() {
            let weight = if i == 0 { Weight::Semibold } else { Weight::Regular };
            let tw = self.text.width(weight, l.chip_px, chip);
            if x + tw + 2.0 * pad > right {
                break;
            }
            let rr = RoundRect { x, y: l.chips_y, w: tw + 2.0 * pad, h: l.chip_h, r: l.chip_h / 2.0 };
            c.fill_round(rr, WHITE, if i == 0 { 0.1 } else { 0.055 });
            let color = if i == 0 { TEXT } else { TEXT_DIM };
            self.text.draw(c, weight, l.chip_px, (x + pad, l.chips_y + (l.chip_h - lh) / 2.0), color, chip);
            x += tw + 2.0 * pad + 6.0 * s;
        }
    }

    fn envelope(&mut self, c: &mut Canvas, l: &Layout, spec: Option<&Spectrogram>) {
        let e = l.envelope;
        c.fill(e.x, e.y, e.w, e.h, WELL, 1.0);
        let mid = e.y as f32 + e.h as f32 / 2.0;
        c.fill(e.x, mid as i64, e.w, l.hair as i64, WHITE, 0.05);
        let Some(s) = spec else { return };
        let peak_color = self.lut.sample(0.55);
        let rms_color = self.lut.sample(0.85);
        let half = e.h as f32 / 2.0 - 4.0 * l.s;
        let height = |a: f32| a.clamp(0.0, 1.0) * half;
        let cols = s.columns();
        for x in 0..e.w as usize {
            let ci = x * cols / e.w as usize;
            if ci >= s.filled() {
                break;
            }
            let env = s.envelope(ci);
            let (hp, hr) = (height(env.peak), height(env.rms));
            let top = (mid - hp - 1.0).max(e.y as f32) as usize;
            let bottom = ((mid + hp + 1.0) as usize).min((e.y + e.h) as usize);
            for y in top..bottom {
                let d = (y as f32 + 0.5 - mid).abs();
                let px = c.get(e.x as usize + x, y);
                let peak = (hp - d + 0.5).clamp(0.0, 1.0) * 0.45;
                let rms = (hr - d + 0.5).clamp(0.0, 1.0) * 0.9;
                let mut col = mix(px, quantize(peak_color, x, y), peak);
                col = mix(col, quantize(rms_color, x, y), rms);
                c.set(e.x + x as i64, y as i64, col);
            }
        }
    }

    fn spectrogram(&mut self, c: &mut Canvas, l: &Layout, s: &Spectrogram, v: &View) {
        let p = l.plot;
        let (pw, ph) = (p.w as usize, p.h as usize);
        let bands = s.bands();
        let cols = s.columns();
        let filled_px = (s.filled() * pw).div_ceil(cols).min(pw);
        // Each pixel row covers a band interval (low frequencies at the
        // bottom). Rows spanning several bands take the loudest, so thin
        // tones survive; rows within one band interpolate smoothly.
        let (scale, nyquist) = match v.sample_rate {
            0 => (FreqScale::Linear, 1.0),
            r => (v.freq_scale, r as f64 / 2.0),
        };
        let band_at =
            |u: f64| (scale.freq(u, nyquist) / nyquist * (bands - 1) as f64).clamp(0.0, (bands - 1) as f64) as f32;
        let rows: Vec<(f32, usize, usize)> = (0..ph)
            .map(|y| {
                let top = band_at((ph - y) as f64 / ph as f64);
                let bottom = band_at((ph - y - 1) as f64 / ph as f64);
                let lo = math::ceilf(bottom) as usize;
                let hi = (math::floorf(top) as usize).min(bands - 1);
                (0.5 * (top + bottom), lo, hi)
            })
            .collect();
        let range = (v.upper_db - v.lower_db) as f32;
        let lower = v.lower_db as f32;
        self.levels.clear();
        self.levels.resize(pw * ph, 0.0);
        for x in 0..filled_px {
            let col = s.column(x * cols / pw);
            for (y, &(mid, lo, hi)) in rows.iter().enumerate() {
                let db = if hi > lo {
                    col[lo..=hi].iter().copied().fold(f32::MIN, f32::max)
                } else {
                    // Catmull-Rom, clamped to its neighbours to avoid ringing.
                    let i = (mid as usize).min(bands - 2);
                    let t = mid - i as f32;
                    let g = |k: isize| col[(i as isize + k).clamp(0, bands as isize - 1) as usize];
                    let (a, b, cc, d) = (g(-1), g(0), g(1), g(2));
                    let y = 0.5
                        * (2.0 * b
                            + (cc - a) * t
                            + (2.0 * a - 5.0 * b + 4.0 * cc - d) * t * t
                            + (3.0 * b - a - 3.0 * cc + d) * t * t * t);
                    y.clamp(b.min(cc), b.max(cc))
                };
                self.levels[y * pw + x] = (db - lower) / range;
            }
        }

        // Bloom: bright parts, blurred at half resolution, screened back on.
        let (gw, gh) = (pw.div_ceil(2), ph.div_ceil(2));
        let glow = v.glow && filled_px > 0;
        if glow {
            self.glow.clear();
            self.glow.resize(gw * gh, [0.0; 3]);
            for y in 0..ph {
                for x in 0..filled_px {
                    let col = self.lut.sample(self.levels[y * pw + x]);
                    let lum = (0.2126 * col[0] + 0.7152 * col[1] + 0.0722 * col[2]) / 255.0;
                    let k = ((lum - 0.42) / 0.58).clamp(0.0, 1.0);
                    let k = 0.25 * k * k;
                    let g = &mut self.glow[(y / 2) * gw + x / 2];
                    for i in 0..3 {
                        g[i] += col[i] * k;
                    }
                }
            }
            let r = (3.5 * l.s) as usize + 1;
            blur(&mut self.glow, &mut self.scratch, gw, gh, r);
        }
        let strength = 1.1;
        for y in 0..ph {
            let row = c.row_mut(p.y as usize + y);
            for x in 0..pw {
                let out = &mut row[p.x as usize + x];
                if x >= filled_px {
                    *out = WELL;
                    continue;
                }
                let mut col = self.lut.sample(self.levels[y * pw + x]);
                if glow {
                    // Bilinear sample of the half-resolution glow.
                    let gx = ((x as f32 - 0.5) / 2.0).max(0.0);
                    let gy = ((y as f32 - 0.5) / 2.0).max(0.0);
                    let (ix, iy) = ((gx as usize).min(gw - 1), (gy as usize).min(gh - 1));
                    let (jx, jy) = ((ix + 1).min(gw - 1), (iy + 1).min(gh - 1));
                    let (fx, fy) = (gx - ix as f32, gy - iy as f32);
                    let (g00, g10) = (self.glow[iy * gw + ix], self.glow[iy * gw + jx]);
                    let (g01, g11) = (self.glow[jy * gw + ix], self.glow[jy * gw + jx]);
                    for (i, ci) in col.iter_mut().enumerate() {
                        let a = g00[i] * (1.0 - fx) + g10[i] * fx;
                        let b = g01[i] * (1.0 - fx) + g11[i] * fx;
                        let g = ((a * (1.0 - fy) + b * fy) * strength).min(255.0);
                        *ci = 255.0 - (255.0 - *ci) * (255.0 - g) / 255.0;
                    }
                }
                *out = quantize(col, x, y);
            }
        }

        // Progress scanline at the analysis front.
        if filled_px < pw {
            let accent = self.lut.sample(0.92);
            let fade = 28.0 * l.s;
            for y in 0..ph {
                for d in 0..fade as usize {
                    let x = filled_px as i64 - 1 - d as i64;
                    if x < 0 {
                        break;
                    }
                    let k = 1.0 - d as f32 / fade;
                    let a = 0.4 * k * k;
                    c.blend(p.x + x, p.y + y as i64, quantize(accent, x as usize, y), a);
                }
            }
            let t = (1.5 * l.s) as i64;
            c.fill(p.x + filled_px as i64 - t, p.y, t, p.h, quantize(accent, 0, 0), 0.9);
        }
    }

    fn grid_and_axes(&mut self, c: &mut Canvas, l: &Layout, v: &View) {
        let p = l.plot;
        let s = l.s;
        let lh = self.text.line_height(Weight::Regular, l.label_px);
        let hair = l.hair as i64;
        if v.sample_rate > 0 {
            let nyquist = v.sample_rate as f64 / 2.0;
            let scale = v.freq_scale;
            let gap = (lh * 2.4) as f64;
            let values = match scale {
                FreqScale::Linear => {
                    let steps = [100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0, 50000.0];
                    ticks(0.0, nyquist, p.h as f64 / nyquist, gap, &steps).1
                }
                _ => {
                    // Round values, kept where they do not crowd the one below.
                    let mut out: Vec<f64> = Vec::new();
                    if scale == FreqScale::Mel {
                        out.push(0.0);
                    }
                    for e in [10.0, 100.0, 1000.0, 10000.0, 100000.0] {
                        for m in [1.0, 2.0, 5.0] {
                            let f = m * e;
                            let fr = scale.frac(f, nyquist);
                            if !(0.0..=1.0).contains(&fr) {
                                continue;
                            }
                            let far = out.last().is_none_or(|&last| {
                                let lf = scale.frac(last, nyquist).max(0.0);
                                (fr - lf) * p.h as f64 >= gap
                            });
                            if far {
                                out.push(f);
                            }
                        }
                    }
                    out
                }
            };
            // The top edge is always labelled with the true maximum (the
            // Nyquist frequency), so the full range is visible at a glance.
            let mut values: Vec<f64> =
                values.into_iter().filter(|&f| (1.0 - scale.frac(f, nyquist)) * p.h as f64 >= gap).collect();
            values.push(nyquist);
            let top = nyquist;
            for &f in &values {
                let y = (p.y + p.h) as f64 - scale.frac(f, nyquist).max(0.0) * p.h as f64;
                if f > 0.0 && f < nyquist {
                    c.fill(p.x, math::round(y) as i64, p.w, hair, WHITE, 0.07);
                }
                let khz = f / 1000.0;
                let mut label = if khz == math::floor(khz) {
                    format!("{}", khz as i64)
                } else {
                    let t = format!("{khz:.2}");
                    String::from(t.trim_end_matches('0'))
                };
                if f == top {
                    label.push_str(" kHz");
                }
                let tw = self.text.width(Weight::Regular, l.label_px, &label);
                let ly = (y as f32 - lh / 2.0).max(p.y as f32 - lh * 0.3);
                self.text.draw(c, Weight::Regular, l.label_px, (p.x as f32 - 10.0 * s - tw, ly), TEXT_DIM, &label);
            }
        }
        if v.duration > 0.0 {
            let px = p.w as f64 / v.duration;
            let sample = self.text.width(Weight::Regular, l.label_px, "00:00");
            let steps = [
                0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0,
                7200.0,
            ];
            let (step, values) = ticks(0.0, v.duration, px, (sample * 2.4) as f64, &steps);
            let y = (p.y + p.h) as f32 + 9.0 * s;
            for &t in &values {
                let x = p.x as f64 + t * px;
                if t > 0.0 && t < v.duration {
                    c.fill(math::round(x) as i64, p.y, hair, p.h, WHITE, 0.07);
                }
                let mut label = format_time(t, step < 1.0);
                if step < 1.0 {
                    label.pop(); // steps are tenths, so show one decimal
                }
                let tw = self.text.width(Weight::Regular, l.label_px, &label);
                let lx = (x as f32 - tw / 2.0).max(p.x as f32);
                if lx + tw > (p.x + p.w) as f32 + 2.0 * s {
                    continue;
                }
                self.text.draw(c, Weight::Regular, l.label_px, (lx, y), TEXT_DIM, &label);
            }
        }
    }

    fn legend(&mut self, c: &mut Canvas, l: &Layout, v: &View) {
        let p = l.plot;
        let lx = l.legend_x;
        let rr = RoundRect { x: lx, y: p.y as f32, w: l.legend_w, h: p.h as f32, r: l.legend_w / 2.0 };
        for y in 0..p.h {
            let level = 1.0 - y as f32 / (p.h - 1).max(1) as f32;
            for x in lx as i64..(lx + l.legend_w) as i64 + 1 {
                c.set(x, p.y + y, self.lut.color(level, x as usize, y as usize));
            }
        }
        let ch = c.height();
        c.clip_round(rr, move |x, y| quantize(background(y, ch), x, y));
        c.stroke_round(rr, l.hair, WHITE, 0.08);

        let lh = self.text.line_height(Weight::Regular, l.label_px);
        let (upper, lower) = (v.upper_db as f64, v.lower_db as f64);
        let px = p.h as f64 / (upper - lower);
        let (_, values) =
            ticks(lower, upper, px, (lh * 2.4) as f64, &[1.0, 2.0, 5.0, 10.0, 20.0, 30.0, 40.0, 50.0, 100.0]);
        let top = values.iter().copied().fold(f64::MIN, f64::max);
        for &d in values.iter().rev() {
            let y = p.y as f64 + (upper - d) * px;
            let mut label = format!("{}", d as i64).replace('-', "\u{2212}");
            if d == top {
                label.push_str(" dB");
            }
            let ly = (y as f32 - lh / 2.0).clamp(p.y as f32 - lh * 0.3, (p.y + p.h) as f32 - lh * 0.7);
            self.text.draw(c, Weight::Regular, l.label_px, (lx + l.legend_w + 8.0 * l.s, ly), TEXT_DIM, &label);
        }
    }

    fn selection(&mut self, c: &mut Canvas, l: &Layout, v: &View, sel: Selection) {
        let p = l.plot;
        let axes = Axes { plot: p, duration: v.duration, nyquist: v.sample_rate as f64 / 2.0, scale: v.freq_scale };
        let (xa, ya) = axes.to_px(sel.t0.min(sel.t1), sel.f0.max(sel.f1));
        let (xb, yb) = axes.to_px(sel.t0.max(sel.t1), sel.f0.min(sel.f1));
        let (x0, y0, x1, y1) = (xa as i64, ya as i64, (xb as i64).max(xa as i64 + 1), (yb as i64).max(ya as i64 + 1));
        // Spotlight: dim everything around the box.
        let dim = 0.55;
        c.fill(p.x, p.y, p.w, y0 - p.y, 0x000000, dim);
        c.fill(p.x, y1, p.w, p.y + p.h - y1, 0x000000, dim);
        c.fill(p.x, y0, x0 - p.x, y1 - y0, 0x000000, dim);
        c.fill(x1, y0, p.x + p.w - x1, y1 - y0, 0x000000, dim);
        let accent = quantize(self.lut.sample(0.93), 0, 0);
        let box_rr = RoundRect { x: x0 as f32, y: y0 as f32, w: (x1 - x0) as f32, h: (y1 - y0) as f32, r: 4.0 * l.s };
        c.stroke_round(box_rr, 1.5 * l.s, accent, 0.95);

        if let Some(pos) = sel.playhead {
            let (px, _) = axes.to_px(sel.t0.min(sel.t1) + pos, 0.0);
            let w = (1.5 * l.s).max(1.0) as i64;
            c.fill(px as i64, y0, w, y1 - y0, accent, 0.9);
            c.fill(px as i64, l.envelope.y, w, l.envelope.h, accent, 0.8);
        }

        let (t0, t1) = (sel.t0.min(sel.t1), sel.t0.max(sel.t1));
        let (f0, f1) = (sel.f0.min(sel.f1), sel.f0.max(sel.f1));
        let mut text = format!(
            "{} \u{2013} {}   {} \u{2013} {}",
            format_time(t0, true),
            format_time(t1, true),
            format_freq(f0),
            format_freq(f1)
        );
        if sel.playhead.is_some() {
            text.push_str("   Looping");
        }
        let px = l.label_px * 1.05;
        let tw = self.text.width(Weight::Semibold, px, &text);
        let lh = self.text.line_height(Weight::Semibold, px);
        let (pad, h) = (9.0 * l.s, lh + 10.0 * l.s);
        let w = tw + 2.0 * pad;
        let bx = (x0 as f32).clamp(p.x as f32, ((p.x + p.w) as f32 - w).max(p.x as f32));
        let above = y0 as f32 - h - 6.0 * l.s;
        let by = if above >= p.y as f32 { above } else { y0 as f32 + 6.0 * l.s };
        let rr = RoundRect { x: bx, y: by, w, h, r: h / 2.0 };
        c.fill_round(rr, accent, 0.95);
        self.text.draw(c, Weight::Semibold, px, (bx + pad, by + (h - lh) / 2.0), 0x101014, &text);
    }

    fn crosshair(&mut self, c: &mut Canvas, l: &Layout, s: &Spectrogram, v: &View, (cx, cy): (f32, f32)) {
        let p = l.plot;
        if !p.contains(cx, cy) {
            return;
        }
        let x = cx as i64;
        let y = cy as i64;
        let hair = l.hair as i64;
        c.fill(x, l.envelope.y, hair, l.envelope.h, WHITE, 0.35);
        c.fill(x, p.y, hair, p.h, WHITE, 0.3);
        c.fill(p.x, y, p.w, hair, WHITE, 0.3);

        let fx = (cx - p.x as f32) / p.w as f32;
        let fy = ((p.y + p.h) as f32 - cy) / p.h as f32;
        let ci = ((fx * s.columns() as f32) as usize).min(s.columns() - 1);
        let mut parts = Vec::new();
        if v.duration > 0.0 {
            parts.push(format_time(fx as f64 * v.duration, true));
        }
        let nyquist = v.sample_rate as f64 / 2.0;
        let f = if v.sample_rate > 0 { v.freq_scale.freq(fy as f64, nyquist) } else { 0.0 };
        if v.sample_rate > 0 {
            let f = f as f32;
            parts.push(format_freq(f as f64));
        }
        if ci < s.filled() {
            let at = if v.sample_rate > 0 { f / nyquist } else { fy as f64 };
            let band = math::round(at * (s.bands() - 1) as f64) as usize;
            let db = s.column(ci)[band.min(s.bands() - 1)];
            parts.push(format!("{db:.1} dB").replace('-', "\u{2212}"));
        }
        let text = parts.join("   ");
        let px = l.label_px * 1.05;
        let tw = self.text.width(Weight::Semibold, px, &text);
        let lh = self.text.line_height(Weight::Semibold, px);
        let (pad, h) = (9.0 * l.s, lh + 10.0 * l.s);
        let w = tw + 2.0 * pad;
        let mut bx = cx + 14.0 * l.s;
        let mut by = cy + 14.0 * l.s;
        if bx + w > (p.x + p.w) as f32 {
            bx = cx - 14.0 * l.s - w;
        }
        if by + h > (p.y + p.h) as f32 {
            by = cy - 14.0 * l.s - h;
        }
        let rr = RoundRect { x: bx, y: by, w, h, r: h / 2.0 };
        c.fill_round(rr, CARD, 0.92);
        c.stroke_round(rr, l.hair, WHITE, 0.12);
        self.text.draw(c, Weight::Semibold, px, (bx + pad, by + (h - lh) / 2.0), TEXT, &text);
    }

    fn message(&mut self, c: &mut Canvas, l: &Layout, msg: &str) {
        let p = l.plot;
        let mut lines = msg.lines();
        let first = lines.next().unwrap_or("");
        let rest: Vec<&str> = lines.collect();
        let big = 17.0 * l.s;
        let small = 12.5 * l.s;
        let bh = self.text.line_height(Weight::Semibold, big);
        let sh = self.text.line_height(Weight::Regular, small) * 1.2;
        let total = bh + sh * rest.len() as f32 + if rest.is_empty() { 0.0 } else { 4.0 * l.s };
        let cx = p.x as f32 + p.w as f32 / 2.0;
        let mut y = p.y as f32 + (p.h as f32 - total) / 2.0;
        let room = (p.w as f32 - 24.0 * l.s).max(0.0);
        let first = self.text.fit(Weight::Semibold, big, first, room, true);
        let fw = self.text.width(Weight::Semibold, big, &first);
        self.text.draw(c, Weight::Semibold, big, (cx - fw / 2.0, y), TEXT, &first);
        y += bh + 4.0 * l.s;
        for line in rest {
            let line = self.text.fit(Weight::Regular, small, line, room, true);
            let w = self.text.width(Weight::Regular, small, &line);
            self.text.draw(c, Weight::Regular, small, (cx - w / 2.0, y), TEXT_DIM, &line);
            y += sh;
        }
    }

    fn panel(&mut self, c: &mut Canvas, l: &Layout, title: &str, rows: &[(String, String)]) {
        let s = l.s;
        let size = 13.0 * s;
        let lh = self.text.line_height(Weight::Regular, size) * 1.45;
        let key_w = rows.iter().map(|(k, _)| self.text.width(Weight::Semibold, size, k)).fold(0.0, f32::max);
        let val_w = rows.iter().map(|(_, d)| self.text.width(Weight::Regular, size, d)).fold(0.0, f32::max);
        let title_px = 16.0 * s;
        let title_h = self.text.line_height(Weight::Semibold, title_px) * 1.8;
        let pad = 24.0 * s;
        let pw =
            (key_w + val_w + 18.0 * s + 2.0 * pad).max(self.text.width(Weight::Semibold, title_px, title) + 2.0 * pad);
        let ph = title_h + lh * rows.len() as f32 + 2.0 * pad - 6.0 * s;
        let (w, h) = (c.width() as f32, c.height() as f32);
        let px = ((w - pw) / 2.0).max(0.0);
        let py = ((h - ph) / 2.0).max(0.0);
        c.fill(0, 0, c.width() as i64, c.height() as i64, 0x000000, 0.5);
        for k in 1..=6 {
            let e = k as f32 * 3.0 * s;
            let rr = RoundRect { x: px - e, y: py - e + 6.0 * s, w: pw + 2.0 * e, h: ph + 2.0 * e, r: 14.0 * s + e };
            c.fill_round(rr, 0x000000, 0.07);
        }
        let card = RoundRect { x: px, y: py, w: pw, h: ph, r: 14.0 * s };
        c.fill_round(card, CARD, 1.0);
        c.stroke_round(card, l.hair, WHITE, 0.09);
        self.text.draw(c, Weight::Semibold, title_px, (px + pad, py + pad - 4.0 * s), TEXT, title);
        let mut y = py + pad - 4.0 * s + title_h;
        for (k, d) in rows {
            self.text.draw(c, Weight::Semibold, size, (px + pad, y), TEXT, k);
            self.text.draw(c, Weight::Regular, size, (px + pad + key_w + 18.0 * s, y), TEXT_DIM, d);
            y += lh;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn ticks_are_round_and_spaced() {
        let (step, v) = ticks(0.0, 22050.0, 600.0 / 22050.0, 30.0, &[1000.0, 2000.0, 5000.0]);
        assert_eq!(step, 2000.0);
        assert_eq!(v.first(), Some(&0.0));
        assert_eq!(v.last(), Some(&22000.0));
        let (_, d) = ticks(-120.0, 0.0, 5.0, 30.0, &[1.0, 2.0, 5.0, 10.0, 20.0]);
        assert_eq!(d, [-120.0, -110.0, -100.0, -90.0, -80.0, -70.0, -60.0, -50.0, -40.0, -30.0, -20.0, -10.0, 0.0]);
    }

    #[test]
    fn freq_scales_invert() {
        for scale in FreqScale::ALL {
            for f in [30.0, 440.0, 1000.0, 9000.0, 22050.0] {
                let u = scale.frac(f, 22050.0);
                assert!((0.0..=1.0 + 1e-9).contains(&u), "{scale:?} {f}");
                assert!((scale.freq(u, 22050.0) - f).abs() < 1e-6 * f, "{scale:?} {f}");
            }
        }
        // Mel gives music room: 0-4 kHz takes over half the height.
        assert!(FreqScale::Mel.frac(4000.0, 22050.0) > 0.5);
    }

    #[test]
    fn axes_round_trip() {
        for scale in FreqScale::ALL {
            let a = Axes::new(1200, 700, 1.0, 30.0, 44100, scale);
            let (x, y) = a.to_px(12.5, 3000.0);
            assert!(a.contains(x, y));
            let (t, f) = a.to_data(x, y);
            assert!((t - 12.5).abs() < 0.05, "{scale:?} {t}");
            assert!((f - 3000.0).abs() < 60.0, "{scale:?} {f}");
        }
    }

    #[test]
    fn formats() {
        assert_eq!(format_time(83.456, true), "1:23.46");
        assert_eq!(format_time(3725.0, false), "1:02:05");
        assert_eq!(split_path("/a/b/song.flac"), ("/a/b/", "song.flac"));
        assert_eq!(split_path("song.flac"), ("", "song.flac"));
    }

    #[test]
    fn blur_keeps_energy() {
        let (w, h) = (40, 40);
        let mut buf = vec![[0.0f32; 3]; w * h];
        buf[20 * w + 20] = [90.0, 0.0, 0.0];
        blur(&mut buf, &mut Vec::new(), w, h, 2);
        let total: f32 = buf.iter().map(|p| p[0]).sum();
        assert!((total - 90.0).abs() < 0.5, "{total}");
    }
}
