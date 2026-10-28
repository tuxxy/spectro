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

use crate::analysis::{Envelope, Spectrogram};
use crate::canvas::{Canvas, RoundRect, mix};
use crate::font::{Text, Weight};
use crate::math;
use crate::palette::{Lut, Palette, quantize};

pub const MIN_DB: i32 = -140;
pub const MAX_DB: i32 = 0;
pub const DEFAULT_UPPER_DB: i32 = 0;
pub const DEFAULT_LOWER_DB: i32 = -120;

/// Colours of everything but the spectrogram.
#[derive(Clone, Copy)]
struct Theme {
    bg_top: [f32; 3],
    bg_bottom: [f32; 3],
    text: u32,
    dim: u32,
    faint: u32,
    well: u32,
    /// Hairlines, grid and highlights, drawn at low opacity.
    line: u32,
    card: u32,
    /// Text on an accent-coloured label.
    ink: u32,
}

const STUDIO: Theme = Theme {
    bg_top: [18.0, 19.0, 27.0],
    bg_bottom: [7.0, 7.0, 10.0],
    text: 0xf1f2f6,
    dim: 0x8d91a0,
    faint: 0x5f6372,
    well: 0x0e0f15,
    line: 0xffffff,
    card: 0x17181f,
    ink: 0x101014,
};

const SONAR: Theme = Theme {
    bg_top: [5.0, 16.0, 10.0],
    bg_bottom: [1.0, 5.0, 3.0],
    text: 0xb4f7c2,
    dim: 0x4fae6c,
    faint: 0x2e6b43,
    well: 0x010603,
    line: 0x7dff9e,
    card: 0x06140b,
    ink: 0x021006,
};

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

fn mel_inv(m: f64) -> f64 {
    700.0 * (math::exp(m / 2595.0 * core::f64::consts::LN_10) - 1.0)
}

/// The lowest frequency a log axis shows: 20 Hz, or the band's own bottom.
fn log_floor(lo: f64, hi: f64) -> f64 {
    lo.max(LOG_MIN_HZ.min(hi * 0.5))
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

    /// Frequency at height fraction `u` (0 at the bottom, 1 at the top) of
    /// a plot showing `lo..hi` hertz.
    pub fn freq(self, u: f64, lo: f64, hi: f64) -> f64 {
        match self {
            FreqScale::Linear => lo + u * (hi - lo),
            FreqScale::Mel => {
                let (a, b) = (mel(lo), mel(hi));
                mel_inv(a + u * (b - a))
            }
            FreqScale::Log => {
                let lo = log_floor(lo, hi);
                lo * math::exp(u * math::ln(hi / lo))
            }
        }
    }

    /// Height fraction of frequency `f`; the inverse of [`FreqScale::freq`].
    pub fn frac(self, f: f64, lo: f64, hi: f64) -> f64 {
        match self {
            FreqScale::Linear => (f - lo) / (hi - lo),
            FreqScale::Mel => (mel(f) - mel(lo)) / (mel(hi) - mel(lo)),
            FreqScale::Log => {
                let lo = log_floor(lo, hi);
                if f <= 0.0 { f64::NEG_INFINITY } else { math::ln(f / lo) / math::ln(hi / lo) }
            }
        }
    }
}

/// Everything the renderer needs to know about what to show.
#[derive(Clone, Copy)]
pub struct View<'a> {
    /// Analysed data, if any. Its column count need not match the plot width.
    pub spectrogram: Option<&'a Spectrogram>,
    /// The file path; shown as a large name and a dim folder.
    pub title: &'a str,
    /// Comma-separated details (codec, rate, channel, window), shown as pills.
    pub description: &'a str,
    /// Length of the whole file in seconds; 0 hides the time axis.
    pub duration: f64,
    /// The seconds shown across the plot (the whole file unless zoomed).
    pub time_range: (f64, f64),
    /// The hertz shown up the plot (0 to Nyquist unless zoomed).
    pub freq_range: (f64, f64),
    /// The seconds the spectrogram's columns cover. While a zoom is being
    /// re-analysed this still describes the old data, which is stretched
    /// to fit until the new columns arrive.
    pub data_range: (f64, f64),
    /// Loudness of the whole file, for the strip above the plot, which then
    /// marks the visible part.
    pub overview: Option<&'a [Envelope]>,
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
    pub panel: Option<Panel<'a>>,
    /// Version string for the about panel and the like.
    pub version: &'a str,
    /// Let loud components bloom.
    pub glow: bool,
    pub freq_scale: FreqScale,
    /// Pointer position in device pixels, for the crosshair readout.
    pub cursor: Option<(f32, f32)>,
    /// A selected time and frequency region, e.g. for looped playback.
    pub selection: Option<Selection>,
    /// Submarine mode: a sonar waterfall in green phosphor. Frequency runs
    /// across and time runs down, the newest at the top; while the analysis
    /// runs, the picture scrolls down as it arrives. The strip above shows
    /// the spectrum, as a sonar's line trace.
    pub sonar: bool,
}

impl View<'_> {
    /// The seconds shown in the plot. In submarine mode, while the
    /// analysis runs, the view rides on its front so the picture scrolls.
    pub fn shown_time(&self, filled: usize, columns: usize) -> (f64, f64) {
        scrolled(self.sonar, self.time_range, self.data_range, filled, columns)
    }
}

/// See [`View::shown_time`].
pub fn scrolled(sonar: bool, time: (f64, f64), data: (f64, f64), filled: usize, columns: usize) -> (f64, f64) {
    if !sonar || time != data || columns == 0 || filled >= columns {
        return time;
    }
    let lag = (time.1 - time.0) * (columns - filled) as f64 / columns as f64;
    (time.0 - lag, time.1 - lag)
}

/// A card over the picture: help, settings or about.
#[derive(Clone, Copy)]
pub struct Panel<'a> {
    pub title: &'a str,
    pub sections: &'a [Section],
    /// Small print at the bottom, e.g. how to close the card.
    pub footer: &'a str,
}

/// A titled group of `(keys, description)` rows. Keys separated by two
/// spaces are drawn as separate keycaps; an empty key leaves only text.
#[derive(Clone, Debug, Default)]
pub struct Section {
    pub heading: String,
    pub rows: Vec<(String, String)>,
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

/// The loudness strip above the plot, in device pixels.
pub fn strip_rect(width: usize, height: usize, scale: f32) -> Plot {
    layout(width, height, scale).envelope
}

/// Converts between device pixels and seconds/hertz on the plot.
#[derive(Clone, Copy, Debug)]
pub struct Axes {
    pub plot: Plot,
    time: (f64, f64),
    freq: (f64, f64),
    scale: FreqScale,
    /// Frequency across and time up, as in submarine mode.
    waterfall: bool,
}

impl Axes {
    pub fn new(
        width: usize,
        height: usize,
        ui_scale: f32,
        time: (f64, f64),
        freq: (f64, f64),
        scale: FreqScale,
        waterfall: bool,
    ) -> Self {
        Axes { plot: plot_rect(width, height, ui_scale), time, freq, scale, waterfall }
    }

    fn of(plot: Plot, v: &View) -> Self {
        Axes { plot, time: v.time_range, freq: v.freq_range, scale: v.freq_scale, waterfall: v.sonar }
    }

    /// Fractions of the way across (left to right) and up (bottom to top).
    fn fractions(&self, x: f32, y: f32) -> (f64, f64) {
        let p = self.plot;
        let a = ((x - p.x as f32) / p.w.max(1) as f32).clamp(0.0, 1.0) as f64;
        let b = (((p.y + p.h) as f32 - y) / p.h.max(1) as f32).clamp(0.0, 1.0) as f64;
        (a, b)
    }

    /// Seconds and hertz under `(x, y)`, clamped to the plot.
    pub fn to_data(&self, x: f32, y: f32) -> (f64, f64) {
        let (a, b) = self.fractions(x, y);
        let (u, v) = if self.waterfall { (b, a) } else { (a, b) };
        let (t0, t1) = self.time;
        (t0 + u * (t1 - t0), self.scale.freq(v, self.freq.0, self.freq.1).max(0.0))
    }

    /// Device pixel of seconds `t` and hertz `f`, which may lie outside the plot.
    pub fn to_px(&self, t: f64, f: f64) -> (f32, f32) {
        let p = self.plot;
        let (t0, t1) = self.time;
        let u = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
        let v = self.scale.frac(f.max(1e-9), self.freq.0, self.freq.1).clamp(-1.0, 2.0);
        let (a, b) = if self.waterfall { (v, u) } else { (u, v) };
        (p.x as f32 + (a * p.w as f64) as f32, (p.y + p.h) as f32 - (b * p.h as f64) as f32)
    }

    /// Plot pixels along the time axis.
    pub fn time_len(&self) -> i64 {
        if self.waterfall { self.plot.h } else { self.plot.w }
    }

    /// Plot pixels along the frequency axis.
    pub fn freq_len(&self) -> i64 {
        if self.waterfall { self.plot.w } else { self.plot.h }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.plot.contains(x, y)
    }
}

/// A number with as many decimals as it needs, up to three.
fn format_number(x: f64) -> String {
    let t = format!("{x:.3}");
    let t = t.trim_end_matches('0').trim_end_matches('.');
    String::from(if t.is_empty() || t == "-" { "0" } else { t })
}

fn format_freq(f: f64) -> String {
    if f < 1000.0 { format!("{f:.0} Hz") } else { format!("{:.2} kHz", f / 1000.0) }
}

fn background(theme: &Theme, y: usize, height: usize) -> [f32; 3] {
    let t = y as f32 / height.max(1) as f32;
    let t = t * t * (3.0 - 2.0 * t);
    [0, 1, 2].map(|i| theme.bg_top[i] + (theme.bg_bottom[i] - theme.bg_top[i]) * t)
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

/// For each of `n` pixels along the frequency axis, lowest first: the
/// fractional band at its centre and the whole bands it spans.
fn band_rows(n: usize, bands: usize, v: &View) -> Vec<(f32, usize, usize)> {
    let (scale, nyquist, lo, hi) = match v.sample_rate {
        0 => (FreqScale::Linear, 1.0, 0.0, 1.0),
        r => (v.freq_scale, r as f64 / 2.0, v.freq_range.0, v.freq_range.1),
    };
    let band_at = |u: f64| (scale.freq(u, lo, hi) / nyquist * (bands - 1) as f64).clamp(0.0, (bands - 1) as f64) as f32;
    (0..n)
        .map(|k| {
            let bottom = band_at(k as f64 / n as f64);
            let top = band_at((k + 1) as f64 / n as f64);
            (0.5 * (top + bottom), math::ceilf(bottom) as usize, (math::floorf(top) as usize).min(bands - 1))
        })
        .collect()
}

/// Level of one pixel's frequency span. A span over several bands takes the
/// loudest, so thin tones survive; within one band it interpolates smoothly.
fn band_db(col: &[f32], (mid, lo, hi): (f32, usize, usize)) -> f32 {
    if hi > lo {
        return col[lo..=hi].iter().copied().fold(f32::MIN, f32::max);
    }
    // Catmull-Rom, clamped to its neighbours to avoid ringing.
    let bands = col.len();
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
}

/// Draws `values` (0 to 1, one per pixel) as a line across the strip `e`,
/// optionally filled underneath.
fn trace_line(c: &mut Canvas, e: Plot, pad: f32, values: &[f32], color: u32, fill: f32, thick: i64) {
    let bottom = (e.y + e.h) as f32 - pad;
    let h = e.h as f32 - 2.0 * pad;
    let y_of = |v: f32| (bottom - v.clamp(0.0, 1.0) * h) as i64;
    let mut prev = None;
    for (x, &v) in values.iter().enumerate().take(e.w as usize) {
        let y = y_of(v);
        let x = e.x + x as i64;
        if fill > 0.0 {
            c.fill(x, y, 1, bottom as i64 - y, color, fill);
        }
        let (a, b) = match prev {
            Some(p) => (y.min(p), y.max(p)),
            None => (y, y),
        };
        c.fill(x, a, 1, b - a + thick, color, 0.95);
        prev = Some(y);
    }
}

pub struct Renderer {
    text: Text,
    lut: Lut,
    theme: Theme,
    levels: Vec<f32>,
    glow: Vec<[f32; 3]>,
    scratch: Vec<[f32; 3]>,
    /// Mean level per frequency pixel of the last picture, for the trace.
    trace: Vec<f32>,
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
            theme: STUDIO,
            levels: Vec::new(),
            glow: Vec::new(),
            scratch: Vec::new(),
            trace: Vec::new(),
        }
    }

    /// Picks the colours for `v` and returns the view as shown: in
    /// submarine mode, scrolled to the analysis front.
    fn prepare<'a>(&mut self, v: &View<'a>) -> View<'a> {
        let palette = if v.sonar { Palette::Sonar } else { v.palette };
        if self.lut.palette() != palette {
            self.lut = Lut::new(palette);
        }
        self.theme = if v.sonar { SONAR } else { STUDIO };
        let (filled, columns) = v.spectrogram.map_or((0, 0), |s| (s.filled(), s.columns()));
        View { time_range: v.shown_time(filled, columns), ..*v }
    }

    /// Draws the whole picture, including the crosshair.
    pub fn render(&mut self, c: &mut Canvas, v: &View) {
        self.render_base(c, v);
        self.overlay(c, v);
    }

    /// Draws the pointer crosshair and readout over a finished picture, so
    /// a front end can cache the picture and redraw only this on movement.
    pub fn overlay(&mut self, c: &mut Canvas, v: &View) {
        let v = &self.prepare(v);
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
        let v = &self.prepare(v);
        let th = self.theme;
        let l = layout(c.width(), c.height(), v.scale);
        let (cw, ch) = (c.width(), c.height());
        for y in 0..ch {
            let bg = background(&th, y, ch);
            for (x, p) in c.row_mut(y).iter_mut().enumerate() {
                *p = quantize(bg, x, y);
            }
        }
        let bg_at = move |x: usize, y: usize| quantize(background(&th, y, ch), x, y);

        self.header(c, &l, v, cw as f32);
        let p = l.plot;
        let spec = v.spectrogram.filter(|s| s.columns() > 0 && s.bands() > 1);
        if p.w > 0 && p.h > 0 {
            self.trace.clear();
            match spec {
                Some(s) if s.filled() > 0 => self.spectrogram(c, &l, s, v),
                _ => c.fill(p.x, p.y, p.w, p.h, th.well, 1.0),
            }
            if v.sonar {
                self.trace_strip(c, &l);
            } else {
                self.envelope(c, &l, spec, v);
            }
            c.clip_round(l.envelope.round(l.radius * 0.7), bg_at);
            self.grid_and_axes(c, &l, v);
            c.clip_round(p.round(l.radius), bg_at);
            c.stroke_round(p.round(l.radius), l.hair, th.line, 0.07);
            self.legend(c, &l, v);
        }
        if let Some(msg) = v.message {
            self.message(c, &l, msg);
        }
        if let Some(panel) = v.panel {
            self.panel(c, &l, panel);
        }
    }

    fn header(&mut self, c: &mut Canvas, l: &Layout, v: &View, w: f32) {
        let s = l.s;
        let mark = if v.sonar { "spectro sonar" } else { "spectro" };
        let mw = self.text.width(Weight::Semibold, 13.0 * s, mark);
        let mark_x = w - l.margin - mw;
        let title_base = l.title_y + self.text.ascent(Weight::Semibold, l.title_px);
        self.text.draw(
            c,
            Weight::Semibold,
            13.0 * s,
            (mark_x, title_base - self.text.ascent(Weight::Semibold, 13.0 * s)),
            self.theme.faint,
            mark,
        );

        let x0 = l.plot.x as f32;
        let room = mark_x - x0 - 24.0 * s;
        if !v.title.is_empty() && room > 0.0 {
            let (folder, name) = split_path(v.title);
            let name = self.text.fit(Weight::Semibold, l.title_px, name, room, true);
            let nw = self.text.draw(c, Weight::Semibold, l.title_px, (x0, l.title_y), self.theme.text, &name);
            let fx = x0 + nw + 10.0 * s;
            if !folder.is_empty() && room - nw - 10.0 * s > 40.0 * s {
                let folder = self.text.fit(Weight::Regular, l.folder_px, folder, room - nw - 10.0 * s, false);
                let fy = title_base - self.text.ascent(Weight::Regular, l.folder_px);
                self.text.draw(c, Weight::Regular, l.folder_px, (fx, fy), self.theme.faint, &folder);
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
            c.fill_round(rr, self.theme.line, if i == 0 { 0.1 } else { 0.055 });
            let color = if i == 0 { self.theme.text } else { self.theme.dim };
            self.text.draw(c, weight, l.chip_px, (x + pad, l.chips_y + (l.chip_h - lh) / 2.0), color, chip);
            x += tw + 2.0 * pad + 6.0 * s;
        }
    }

    /// For each plot column `x`, the spectrogram column under it, if analysed.
    fn column_map(width: usize, s: &Spectrogram, v: &View) -> Vec<Option<usize>> {
        let (t0, t1) = v.time_range;
        let (d0, d1) = v.data_range;
        let cols = s.columns();
        (0..width)
            .map(|x| {
                let t = t0 + (x as f64 + 0.5) / width as f64 * (t1 - t0);
                if d1 <= d0 || t < d0 || t >= d1 {
                    return None;
                }
                let ci = (((t - d0) / (d1 - d0)) * cols as f64) as usize;
                Some(ci.min(cols - 1)).filter(|&ci| ci < s.filled())
            })
            .collect()
    }

    /// Draws one column of the loudness strip.
    fn envelope_column(&self, c: &mut Canvas, l: &Layout, x: usize, env: Envelope) {
        let e = l.envelope;
        let mid = e.y as f32 + e.h as f32 / 2.0;
        let half = e.h as f32 / 2.0 - 4.0 * l.s;
        let (hp, hr) = (env.peak.clamp(0.0, 1.0) * half, env.rms.clamp(0.0, 1.0) * half);
        let (peak_color, rms_color) = (self.lut.sample(0.55), self.lut.sample(0.85));
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

    /// The loudness strip: the whole file when an overview is available,
    /// with the visible part marked; otherwise just the visible part.
    fn envelope(&mut self, c: &mut Canvas, l: &Layout, spec: Option<&Spectrogram>, v: &View) {
        let e = l.envelope;
        let w = e.w as usize;
        c.fill(e.x, e.y, e.w, e.h, self.theme.well, 1.0);
        let mid = e.y as f32 + e.h as f32 / 2.0;
        c.fill(e.x, mid as i64, e.w, l.hair as i64, self.theme.line, 0.05);
        match (v.overview.filter(|o| !o.is_empty()), spec) {
            (Some(all), _) if v.duration > 0.0 => {
                for x in 0..w {
                    self.envelope_column(c, l, x, all[(x * all.len() / w.max(1)).min(all.len() - 1)]);
                }
                let (t0, t1) = v.time_range;
                if t0 > 1e-9 || t1 < v.duration - 1e-9 {
                    let to_x = |t: f64| e.x + (t / v.duration * e.w as f64) as i64;
                    let (x0, x1) = (to_x(t0), to_x(t1).max(to_x(t0) + 2));
                    c.fill(e.x, e.y, x0 - e.x, e.h, 0x000000, 0.6);
                    c.fill(x1, e.y, e.x + e.w - x1, e.h, 0x000000, 0.6);
                    let accent = quantize(self.lut.sample(0.93), 0, 0);
                    let t = (1.5 * l.s).max(1.0) as i64;
                    c.fill(x0, e.y, t, e.h, accent, 0.9);
                    c.fill(x1 - t, e.y, t, e.h, accent, 0.9);
                }
            }
            (_, Some(s)) => {
                for (x, ci) in Self::column_map(w, s, v).into_iter().enumerate() {
                    if let Some(ci) = ci {
                        self.envelope_column(c, l, x, s.envelope(ci));
                    }
                }
            }
            _ => {}
        }
    }

    /// The sonar's line trace: the mean spectrum of what is shown, aligned
    /// with the frequency axis below.
    fn trace_strip(&mut self, c: &mut Canvas, l: &Layout) {
        let e = l.envelope;
        let th = self.theme;
        c.fill(e.x, e.y, e.w, e.h, th.well, 1.0);
        for k in 1..4 {
            c.fill(e.x, e.y + e.h * k / 4, e.w, l.hair as i64, th.line, 0.05);
        }
        if self.trace.is_empty() {
            return;
        }
        let color = quantize(self.lut.sample(0.7), 0, 0);
        let thick = (1.2 * l.s).max(1.0) as i64;
        trace_line(c, e, 3.0 * l.s, &self.trace, color, 0.18, thick);
    }

    fn spectrogram(&mut self, c: &mut Canvas, l: &Layout, s: &Spectrogram, v: &View) {
        let p = l.plot;
        let (pw, ph) = (p.w as usize, p.h as usize);
        let wf = v.sonar;
        // Time runs across (or, in a waterfall, up); frequency the other way.
        let (nt, nf) = if wf { (ph, pw) } else { (pw, ph) };
        let map = Self::column_map(nt, s, v);
        let rows = band_rows(nf, s.bands(), v);
        let at = |i: usize, k: usize| if wf { (k, ph - 1 - i) } else { (i, ph - 1 - k) };
        let has = |x: usize, y: usize| if wf { map[ph - 1 - y].is_some() } else { map[x].is_some() };
        let range = (v.upper_db - v.lower_db) as f32;
        let lower = v.lower_db as f32;
        self.levels.clear();
        self.levels.resize(pw * ph, 0.0);
        self.trace.resize(nf, 0.0);
        let mut count = 0;
        for (i, ci) in map.iter().enumerate() {
            let Some(ci) = *ci else { continue };
            let col = s.column(ci);
            count += 1;
            for (k, &r) in rows.iter().enumerate() {
                let level = (band_db(col, r) - lower) / range;
                let (x, y) = at(i, k);
                self.levels[y * pw + x] = level;
                self.trace[k] += level.clamp(0.0, 1.0);
            }
        }
        for t in &mut self.trace {
            *t /= count.max(1) as f32;
        }

        // Bloom: bright parts, blurred at half resolution, screened back on.
        let (gw, gh) = (pw.div_ceil(2), ph.div_ceil(2));
        let glow = v.glow && count > 0;
        if glow {
            self.glow.clear();
            self.glow.resize(gw * gh, [0.0; 3]);
            for y in 0..ph {
                for x in (0..pw).filter(|&x| has(x, y)) {
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
        let well = self.theme.well;
        for y in 0..ph {
            let row = c.row_mut(p.y as usize + y);
            for x in 0..pw {
                let out = &mut row[p.x as usize + x];
                if !has(x, y) {
                    *out = well;
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

        // Phosphor screens show their scanlines.
        if wf {
            let period = ((1.5 * l.s) as usize).max(1);
            for y in (0..ph).filter(|y| (y / period) % 2 == 1) {
                c.fill(p.x, p.y + y as i64, p.w, 1, 0x000000, 0.2);
            }
        }

        // Progress scanline at the analysis front, fading over the older side.
        if s.filled() < s.columns() {
            let (d0, d1) = v.data_range;
            let front = d0 + (d1 - d0) * s.filled() as f64 / s.columns() as f64;
            let (t0, t1) = v.time_range;
            let u = (front - t0) / (t1 - t0);
            let accent = self.lut.sample(0.92);
            let fade = 28.0 * l.s;
            let t = ((1.5 * l.s) as i64).max(1);
            let front_px = if wf { ((1.0 - u) * ph as f64) as i64 } else { (u * pw as f64) as i64 };
            let len = if wf { ph } else { pw } as i64;
            if (0..=len).contains(&front_px) {
                for d in 0..fade as usize {
                    let k = 1.0 - d as f32 / fade;
                    let alpha = 0.4 * k * k;
                    if wf {
                        let y = front_px + d as i64;
                        if y >= len {
                            break;
                        }
                        for x in 0..pw {
                            c.blend(p.x + x as i64, p.y + y, quantize(accent, x, y as usize), alpha);
                        }
                    } else {
                        let x = front_px - 1 - d as i64;
                        if x < 0 {
                            break;
                        }
                        for y in 0..ph {
                            c.blend(p.x + x, p.y + y as i64, quantize(accent, x as usize, y), alpha);
                        }
                    }
                }
                let color = quantize(accent, 0, 0);
                if wf {
                    c.fill(p.x, p.y + front_px, p.w, t, color, 0.9);
                } else {
                    c.fill(p.x + front_px - t, p.y, t, p.h, color, 0.9);
                }
            }
        }
    }

    fn grid_and_axes(&mut self, c: &mut Canvas, l: &Layout, v: &View) {
        let p = l.plot;
        let s = l.s;
        let th = self.theme;
        let lh = self.text.line_height(Weight::Regular, l.label_px);
        let hair = l.hair as i64;
        let wf = v.sonar;
        let below = (p.y + p.h) as f32 + 9.0 * s;
        if v.sample_rate > 0 {
            let (lo, hi) = v.freq_range;
            let scale = v.freq_scale;
            let len = if wf { p.w } else { p.h } as f64;
            let gap = if wf {
                (self.text.width(Weight::Regular, l.label_px, "00.00 kHz") * 1.3) as f64
            } else {
                (lh * 2.4) as f64
            };
            // Distance from the low end of the axis, in pixels.
            let pos = |f: f64| scale.frac(f, lo, hi) * len;
            let bottom = scale.freq(0.0, lo, hi);
            let candidates: Vec<f64> = match scale {
                FreqScale::Linear => {
                    let steps = [
                        1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0,
                        50000.0,
                    ];
                    ticks(lo, hi, len / (hi - lo), gap, &steps).1
                }
                _ => [1.0, 10.0, 100.0, 1000.0, 10000.0, 100000.0]
                    .iter()
                    .flat_map(|e| [1.0, 2.0, 5.0].map(|m| m * e))
                    .collect(),
            };
            // Both edges are labelled with their true values, so the range
            // shown is always clear; round values fill in between.
            let mut values = Vec::from([bottom]);
            for f in candidates {
                let d = pos(f);
                let clear_top = len - d >= gap;
                let clear_last = values.last().is_none_or(|&last| d - pos(last) >= gap);
                if f > bottom && f < hi && clear_top && clear_last {
                    values.push(f);
                }
            }
            values.push(hi);
            let khz = hi >= 2000.0;
            for &f in &values {
                let d = pos(f);
                let inner = f > bottom && f < hi;
                let mut label = if khz { format_number(f / 1000.0) } else { format_number(f) };
                if f == hi {
                    label.push_str(if khz { " kHz" } else { " Hz" });
                }
                let tw = self.text.width(Weight::Regular, l.label_px, &label);
                if wf {
                    let x = p.x as f64 + d;
                    if inner {
                        c.fill(math::round(x) as i64, p.y, hair, p.h, th.line, 0.07);
                    }
                    let lx = (x as f32 - tw / 2.0).clamp(p.x as f32, ((p.x + p.w) as f32 - tw).max(p.x as f32));
                    self.text.draw(c, Weight::Regular, l.label_px, (lx, below), th.dim, &label);
                } else {
                    let y = (p.y + p.h) as f64 - d;
                    if inner {
                        c.fill(p.x, math::round(y) as i64, p.w, hair, th.line, 0.07);
                    }
                    let ly = (y as f32 - lh / 2.0).clamp(p.y as f32 - lh * 0.3, (p.y + p.h) as f32 - lh * 0.7);
                    self.text.draw(c, Weight::Regular, l.label_px, (p.x as f32 - 10.0 * s - tw, ly), th.dim, &label);
                }
            }
        }
        let (t0, t1) = v.time_range;
        if v.duration > 0.0 && t1 > t0 {
            let len = if wf { p.h } else { p.w } as f64;
            let px = len / (t1 - t0);
            let gap = if wf {
                (lh * 2.4) as f64
            } else {
                (self.text.width(Weight::Regular, l.label_px, "00:00.0") * 2.0) as f64
            };
            let steps = [
                0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0,
                1800.0, 3600.0, 7200.0,
            ];
            let (step, values) = ticks(t0, t1, px, gap, &steps);
            for &t in &values {
                // A scrolling waterfall can show time before the start.
                if t < -1e-9 || t > v.duration + 1e-9 {
                    continue;
                }
                let d = (t - t0) * px;
                let inner = t > t0 && t < t1;
                let mut label = format_time(t, step < 1.0);
                if (0.1..1.0).contains(&step) {
                    label.pop(); // tenths: one decimal is enough
                }
                let tw = self.text.width(Weight::Regular, l.label_px, &label);
                if wf {
                    let y = (p.y + p.h) as f64 - d;
                    if inner {
                        c.fill(p.x, math::round(y) as i64, p.w, hair, th.line, 0.07);
                    }
                    let ly = (y as f32 - lh / 2.0).clamp(p.y as f32 - lh * 0.3, (p.y + p.h) as f32 - lh * 0.7);
                    self.text.draw(c, Weight::Regular, l.label_px, (p.x as f32 - 10.0 * s - tw, ly), th.dim, &label);
                } else {
                    let x = p.x as f64 + d;
                    if inner {
                        c.fill(math::round(x) as i64, p.y, hair, p.h, th.line, 0.07);
                    }
                    let lx = (x as f32 - tw / 2.0).max(p.x as f32);
                    if lx + tw > (p.x + p.w) as f32 + 2.0 * s {
                        continue;
                    }
                    self.text.draw(c, Weight::Regular, l.label_px, (lx, below), th.dim, &label);
                }
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
        let th = self.theme;
        c.clip_round(rr, move |x, y| quantize(background(&th, y, ch), x, y));
        c.stroke_round(rr, l.hair, self.theme.line, 0.08);

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
            self.text.draw(c, Weight::Regular, l.label_px, (lx + l.legend_w + 8.0 * l.s, ly), self.theme.dim, &label);
        }
    }

    fn selection(&mut self, c: &mut Canvas, l: &Layout, v: &View, sel: Selection) {
        let p = l.plot;
        let axes = Axes::of(p, v);
        let (xa, ya) = axes.to_px(sel.t0, sel.f0);
        let (xb, yb) = axes.to_px(sel.t1, sel.f1);
        let cx = |x: f32| (x as i64).clamp(p.x, p.x + p.w);
        let cy = |y: f32| (y as i64).clamp(p.y, p.y + p.h);
        let (x0, y0) = (cx(xa.min(xb)), cy(ya.min(yb)));
        let (x1, y1) = (cx(xa.max(xb)).max(x0 + 1), cy(ya.max(yb)).max(y0 + 1));
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
            let t = sel.t0.min(sel.t1) + pos;
            let (px, py) = axes.to_px(t, sel.f0);
            let w = (1.5 * l.s).max(1.0) as i64;
            if v.sonar {
                if (p.y..p.y + p.h).contains(&(py as i64)) {
                    c.fill(x0, py as i64, x1 - x0, w, accent, 0.9);
                }
            } else {
                if (p.x..p.x + p.w).contains(&(px as i64)) {
                    c.fill(px as i64, y0, w, y1 - y0, accent, 0.9);
                }
                // The strip shows the whole file when it has an overview.
                let e = l.envelope;
                let sx = if v.overview.is_some() { e.x as f32 + (t / v.duration * e.w as f64) as f32 } else { px };
                c.fill(sx as i64, e.y, w, e.h, accent, 0.8);
            }
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
        self.text.draw(c, Weight::Semibold, px, (bx + pad, by + (h - lh) / 2.0), self.theme.ink, &text);
    }

    fn crosshair(&mut self, c: &mut Canvas, l: &Layout, s: &Spectrogram, v: &View, (cx, cy): (f32, f32)) {
        let p = l.plot;
        if !p.contains(cx, cy) {
            return;
        }
        let th = self.theme;
        let x = cx as i64;
        let y = cy as i64;
        let hair = l.hair as i64;
        c.fill(x, p.y, hair, p.h, th.line, 0.3);
        c.fill(p.x, y, p.w, hair, th.line, 0.3);

        let (t, f) = Axes::of(p, v).to_data(cx, cy);
        let (d0, d1) = v.data_range;
        let ci = if d1 > d0 && t >= d0 && t < d1 {
            ((((t - d0) / (d1 - d0)) * s.columns() as f64) as usize).min(s.columns() - 1)
        } else {
            usize::MAX
        };
        let e = l.envelope;
        if v.sonar {
            // The strip traces the spectrum at the pointer's time.
            c.fill(x, e.y, hair, e.h, th.line, 0.35);
            if ci < s.filled() {
                let col = s.column(ci);
                let range = (v.upper_db - v.lower_db) as f32;
                let values: Vec<f32> = band_rows(p.w as usize, s.bands(), v)
                    .into_iter()
                    .map(|r| (band_db(col, r) - v.lower_db as f32) / range)
                    .collect();
                let color = quantize(self.lut.sample(1.0), 0, 0);
                let thick = (1.5 * l.s).max(1.0) as i64;
                trace_line(c, e, 3.0 * l.s, &values, color, 0.0, thick);
            }
        } else {
            // The strip shows the whole file when it has an overview.
            let sx =
                if v.overview.is_some() && v.duration > 0.0 { e.x + (t / v.duration * e.w as f64) as i64 } else { x };
            c.fill(sx, e.y, hair, e.h, th.line, 0.35);
        }
        let mut parts = Vec::new();
        if v.duration > 0.0 {
            parts.push(format_time(t, true));
        }
        let nyquist = v.sample_rate as f64 / 2.0;
        if v.sample_rate > 0 {
            parts.push(format_freq(f));
        }
        if ci < s.filled() && v.sample_rate > 0 {
            let band = math::round(f / nyquist * (s.bands() - 1) as f64) as usize;
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
        c.fill_round(rr, th.card, 0.92);
        c.stroke_round(rr, l.hair, th.line, 0.12);
        self.text.draw(c, Weight::Semibold, px, (bx + pad, by + (h - lh) / 2.0), th.text, &text);
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
        self.text.draw(c, Weight::Semibold, big, (cx - fw / 2.0, y), self.theme.text, &first);
        y += bh + 4.0 * l.s;
        for line in rest {
            let line = self.text.fit(Weight::Regular, small, line, room, true);
            let w = self.text.width(Weight::Regular, small, &line);
            self.text.draw(c, Weight::Regular, small, (cx - w / 2.0, y), self.theme.dim, &line);
            y += sh;
        }
    }

    /// Width of a row's keycaps.
    fn keys_width(&self, l: &Layout, keys: &str) -> f32 {
        let (px, pad, gap) = (11.5 * l.s, 7.0 * l.s, 5.0 * l.s);
        keys.split("  ")
            .filter(|k| !k.is_empty())
            .map(|k| self.text.width(Weight::Semibold, px, k) + 2.0 * pad + gap)
            .sum::<f32>()
    }

    fn keycaps(&mut self, c: &mut Canvas, l: &Layout, keys: &str, (x, y): (f32, f32), row_h: f32) {
        let (px, pad, gap) = (11.5 * l.s, 7.0 * l.s, 5.0 * l.s);
        let h = 20.0 * l.s;
        let lh = self.text.line_height(Weight::Semibold, px);
        let top = y + (row_h - h) / 2.0;
        let mut x = x;
        for k in keys.split("  ").filter(|k| !k.is_empty()) {
            let w = self.text.width(Weight::Semibold, px, k) + 2.0 * pad;
            let rr = RoundRect { x, y: top, w, h, r: 5.0 * l.s };
            c.fill_round(rr, self.theme.line, 0.07);
            c.stroke_round(rr, l.hair, self.theme.line, 0.16);
            self.text.draw(c, Weight::Semibold, px, (x + pad, top + (h - lh) / 2.0), self.theme.text, k);
            x += w + gap;
        }
    }

    fn panel(&mut self, c: &mut Canvas, l: &Layout, panel: Panel) {
        let s = l.s;
        let size = 13.0 * s;
        let row_h = 27.0 * s;
        let head_px = 11.0 * s;
        let head_h = 30.0 * s;
        let title_px = 18.0 * s;
        let title_h = self.text.line_height(Weight::Semibold, title_px) + 14.0 * s;
        let footer_px = 11.5 * s;
        let footer_h = if panel.footer.is_empty() { 0.0 } else { 34.0 * s };
        let pad = 28.0 * s;
        let col_gap = 36.0 * s;
        let (w, h) = (c.width() as f32, c.height() as f32);

        let section_h =
            |sec: &Section| (if sec.heading.is_empty() { 0.0 } else { head_h }) + row_h * sec.rows.len() as f32;
        let total: f32 = panel.sections.iter().map(section_h).sum();
        let room = (h - 2.0 * pad - title_h - footer_h - 48.0 * s).max(row_h);
        // Split the sections into as few columns as fit, balancing heights.
        let mut columns: Vec<Vec<&Section>> = Vec::new();
        let ncols = (1..=3).find(|&n| total / n as f32 <= room).unwrap_or(3).min(panel.sections.len().max(1));
        let target = total / ncols as f32;
        let mut cur: Vec<&Section> = Vec::new();
        let mut acc = 0.0;
        for sec in panel.sections {
            let sh = section_h(sec);
            if !cur.is_empty() && columns.len() + 1 < ncols && acc + sh / 2.0 > target {
                columns.push(core::mem::take(&mut cur));
                acc = 0.0;
            }
            acc += sh;
            cur.push(sec);
        }
        columns.push(cur);

        // Per column: the key width, and the full width. Sections without
        // any keys start their text at the left edge.
        let key_gap = 14.0 * s;
        let has_keys = |sec: &Section| sec.rows.iter().any(|(k, _)| !k.is_empty());
        let widths: Vec<(f32, f32)> = columns
            .iter()
            .map(|col| {
                let kw = col.iter().flat_map(|sec| &sec.rows).map(|(k, _)| self.keys_width(l, k)).fold(0.0, f32::max);
                let mut full: f32 = 0.0;
                for sec in col {
                    full = full.max(self.text.width(Weight::Semibold, head_px, &sec.heading.to_uppercase()));
                    let indent = if has_keys(sec) { kw + key_gap } else { 0.0 };
                    for (_, d) in &sec.rows {
                        full = full.max(indent + self.text.width(Weight::Regular, size, d));
                    }
                }
                (kw, full)
            })
            .collect();
        let col_h = columns.iter().map(|col| col.iter().map(|sec| section_h(sec)).sum::<f32>()).fold(0.0, f32::max);
        let content_w: f32 = widths.iter().map(|(_, full)| full).sum::<f32>() + col_gap * (columns.len() - 1) as f32;
        let pw = (content_w + 2.0 * pad).max(self.text.width(Weight::Semibold, title_px, panel.title) + 2.0 * pad);
        let ph = pad + title_h + col_h + footer_h + pad - 8.0 * s;
        let px = ((w - pw) / 2.0).max(0.0);
        let py = ((h - ph) / 2.0).max(0.0);

        c.fill(0, 0, c.width() as i64, c.height() as i64, 0x000000, 0.55);
        for k in 1..=6 {
            let e = k as f32 * 3.0 * s;
            let rr = RoundRect { x: px - e, y: py - e + 6.0 * s, w: pw + 2.0 * e, h: ph + 2.0 * e, r: 16.0 * s + e };
            c.fill_round(rr, 0x000000, 0.07);
        }
        let card = RoundRect { x: px, y: py, w: pw, h: ph, r: 16.0 * s };
        c.fill_round(card, self.theme.card, 1.0);
        c.stroke_round(card, l.hair, self.theme.line, 0.09);
        self.text.draw(c, Weight::Semibold, title_px, (px + pad, py + pad - 4.0 * s), self.theme.text, panel.title);

        let lh = self.text.line_height(Weight::Regular, size);
        let mut x = px + pad;
        for (col, (kw, full)) in columns.iter().zip(&widths) {
            let mut y = py + pad + title_h;
            for sec in col {
                if !sec.heading.is_empty() {
                    let hy = y + head_h - self.text.line_height(Weight::Semibold, head_px) - 8.0 * s;
                    self.text.draw(
                        c,
                        Weight::Semibold,
                        head_px,
                        (x, hy),
                        self.theme.faint,
                        &sec.heading.to_uppercase(),
                    );
                    y += head_h;
                }
                let indent = if has_keys(sec) { kw + key_gap } else { 0.0 };
                for (k, d) in &sec.rows {
                    self.keycaps(c, l, k, (x, y), row_h);
                    let dx = x + indent;
                    self.text.draw(c, Weight::Regular, size, (dx, y + (row_h - lh) / 2.0), self.theme.dim, d);
                    y += row_h;
                }
            }
            x += full + col_gap;
        }
        if footer_h > 0.0 {
            let fw = self.text.width(Weight::Regular, footer_px, panel.footer);
            let fy = py + ph - pad + 8.0 * s - self.text.line_height(Weight::Regular, footer_px);
            self.text.draw(c, Weight::Regular, footer_px, (px + (pw - fw) / 2.0, fy), self.theme.faint, panel.footer);
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
            for (lo, hi) in [(0.0, 22050.0), (300.0, 5000.0)] {
                for f in [440.0, 1000.0, 4000.0] {
                    let u = scale.frac(f, lo, hi);
                    assert!((0.0..=1.0 + 1e-9).contains(&u), "{scale:?} {f}");
                    assert!((scale.freq(u, lo, hi) - f).abs() < 1e-6 * f, "{scale:?} {f}");
                }
                assert!((scale.freq(1.0, lo, hi) - hi).abs() < 1e-6 * hi);
            }
        }
        // Mel gives music room: 0-4 kHz takes over half the height.
        assert!(FreqScale::Mel.frac(4000.0, 0.0, 22050.0) > 0.5);
    }

    #[test]
    fn axes_round_trip() {
        for waterfall in [false, true] {
            for scale in FreqScale::ALL {
                for (time, freq) in [((0.0, 30.0), (0.0, 22050.0)), ((10.0, 14.0), (1000.0, 5000.0))] {
                    let a = Axes::new(1200, 700, 1.0, time, freq, scale, waterfall);
                    let (x, y) = a.to_px(12.5, 3000.0);
                    assert!(a.contains(x, y));
                    let (t, f) = a.to_data(x, y);
                    assert!((t - 12.5).abs() < 0.05, "{scale:?} {t}");
                    assert!((f - 3000.0).abs() < 60.0, "{scale:?} {f}");
                }
            }
        }
    }

    #[test]
    fn waterfall_puts_time_up_and_frequency_across() {
        let a = Axes::new(1200, 700, 1.0, (0.0, 10.0), (0.0, 20000.0), FreqScale::Linear, true);
        let (x_low, y_old) = a.to_px(1.0, 1000.0);
        let (x_high, y_new) = a.to_px(9.0, 19000.0);
        assert!(x_high > x_low && y_new < y_old);
        assert_eq!((a.time_len(), a.freq_len()), (a.plot.h, a.plot.w));
    }

    #[test]
    fn waterfall_scrolls_with_the_front() {
        let (time, data) = ((10.0, 20.0), (10.0, 20.0));
        // Half done: the newest data sits at the top of the view.
        assert_eq!(scrolled(true, time, data, 50, 100), (5.0, 15.0));
        assert_eq!(scrolled(true, time, data, 100, 100), time);
        assert_eq!(scrolled(false, time, data, 50, 100), time);
        // Old data stretched over a new view does not scroll.
        assert_eq!(scrolled(true, time, (0.0, 30.0), 50, 100), time);
    }

    #[test]
    fn formats() {
        assert_eq!(format_time(83.456, true), "1:23.46");
        assert_eq!(format_number(2.5), "2.5");
        assert_eq!(format_number(20.0), "20");
        assert_eq!(format_number(0.125), "0.125");
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
