//! Streaming spectrogram analysis.
//!
//! The audio is split into as many time intervals as the image has columns.
//! Each column averages the power of several overlapping, windowed FFTs
//! spread across its interval (Welch's method), which gives a much smoother
//! and less noisy picture than a single transform per column, then converts
//! to dBFS. A full-scale sine at a bin centre reads 0 dB for every window.

use alloc::vec;
use alloc::vec::Vec;

use crate::fft::RealFft;
use crate::math;

pub const MIN_FFT_BITS: u32 = 8;
pub const MAX_FFT_BITS: u32 = 14;
pub const DEFAULT_FFT_BITS: u32 = 11;

/// The quietest level stored, in dB. Digital silence maps here.
pub const FLOOR_DB: f32 = -200.0;

/// Upper bound on transforms averaged into one column.
const MAX_FFTS_PER_COLUMN: u64 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WindowFunction {
    #[default]
    Hann,
    Hamming,
    BlackmanHarris,
}

impl WindowFunction {
    pub const ALL: [WindowFunction; 3] =
        [WindowFunction::Hann, WindowFunction::Hamming, WindowFunction::BlackmanHarris];

    pub fn name(self) -> &'static str {
        match self {
            WindowFunction::Hann => "Hann",
            WindowFunction::Hamming => "Hamming",
            WindowFunction::BlackmanHarris => "Blackman\u{2013}Harris",
        }
    }

    /// Stable identifier for configuration files.
    pub fn id(self) -> &'static str {
        match self {
            WindowFunction::Hann => "hann",
            WindowFunction::Hamming => "hamming",
            WindowFunction::BlackmanHarris => "blackman-harris",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.id() == s)
    }

    pub fn next(self) -> Self {
        crate::cycle(&Self::ALL, self, 1)
    }

    pub fn prev(self) -> Self {
        crate::cycle(&Self::ALL, self, -1)
    }

    /// Symmetric window coefficients of length `n`.
    pub fn coefficients(self, n: usize) -> Vec<f32> {
        let d = (n.max(2) - 1) as f64;
        (0..n)
            .map(|i| {
                let c = |k: f64| math::cos(2.0 * math::PI * k * i as f64 / d);
                (match self {
                    WindowFunction::Hann => 0.5 - 0.5 * c(1.0),
                    WindowFunction::Hamming => 0.53836 - 0.46164 * c(1.0),
                    WindowFunction::BlackmanHarris => 0.35875 - 0.48829 * c(1.0) + 0.14128 * c(2.0) - 0.01168 * c(3.0),
                }) as f32
            })
            .collect()
    }
}

/// Number of frequency bands produced by a `2^bits` transform.
pub fn bands(bits: u32) -> usize {
    (1 << (bits - 1)) + 1
}

/// One finished column of the analysis.
pub struct Column<'a> {
    pub index: usize,
    /// Level per band in dBFS, low frequency first.
    pub db: &'a [f32],
    /// Loudness of the column's time slice.
    pub envelope: Envelope,
}

/// Peak and RMS amplitude (linear, full scale = 1) of a time slice.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Envelope {
    pub peak: f32,
    pub rms: f32,
}

/// A spectrogram in dBFS, stored column by column (low frequency first),
/// with the loudness envelope of each column.
#[derive(Clone)]
pub struct Spectrogram {
    columns: usize,
    bands: usize,
    filled: usize,
    data: Vec<f32>,
    envelope: Vec<Envelope>,
}

impl Spectrogram {
    pub fn new(columns: usize, bands: usize) -> Self {
        Spectrogram {
            columns,
            bands,
            filled: 0,
            data: vec![FLOOR_DB; columns * bands],
            envelope: vec![Envelope::default(); columns],
        }
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn bands(&self) -> usize {
        self.bands
    }

    /// Columns written so far (they arrive left to right).
    pub fn filled(&self) -> usize {
        self.filled
    }

    pub fn column(&self, c: usize) -> &[f32] {
        &self.data[c * self.bands..(c + 1) * self.bands]
    }

    pub fn envelope(&self, c: usize) -> Envelope {
        self.envelope[c]
    }

    /// Stores a column. Columns past the end or of the wrong height are ignored.
    pub fn set(&mut self, col: &Column) {
        let c = col.index;
        if c < self.columns && col.db.len() == self.bands {
            self.data[c * self.bands..(c + 1) * self.bands].copy_from_slice(col.db);
            self.envelope[c] = col.envelope;
            self.filled = self.filled.max(c + 1);
        }
    }
}

/// Turns a stream of mono samples into spectrogram columns.
pub struct Analyzer {
    fft: RealFft,
    window: Vec<f32>,
    norm: f32,
    total: u64,
    columns: usize,
    next: usize,
    /// Absolute index of `buf[0]` in the stream.
    buf_start: u64,
    buf: Vec<f32>,
    eof: bool,
    frame: Vec<f32>,
    power: Vec<f32>,
    acc: Vec<f32>,
    out: Vec<f32>,
}

impl Analyzer {
    /// `total` is the expected number of samples; `columns` the image width.
    pub fn new(bits: u32, window: WindowFunction, total: u64, columns: usize) -> Self {
        let bits = bits.clamp(MIN_FFT_BITS, MAX_FFT_BITS);
        let fft = RealFft::new(bits);
        let n = fft.len();
        let window = window.coefficients(n);
        let sum: f32 = window.iter().sum();
        let b = bands(bits);
        Analyzer {
            fft,
            norm: 4.0 / (sum * sum),
            window,
            total,
            columns,
            next: 0,
            buf_start: 0,
            buf: Vec::new(),
            eof: false,
            frame: vec![0.0; n],
            power: vec![0.0; b],
            acc: vec![0.0; b],
            out: vec![0.0; b],
        }
    }

    pub fn bands(&self) -> usize {
        self.power.len()
    }

    /// Columns emitted so far.
    pub fn done(&self) -> usize {
        self.next
    }

    fn interval(&self, c: usize) -> (u64, u64) {
        let at = |c: usize| (self.total as u128 * c as u128 / self.columns as u128) as u64;
        (at(c), at(c + 1))
    }

    /// Centres of the transforms that make up column `c`.
    fn centres(&self, c: usize) -> (u64, u64, u64) {
        let (start, end) = self.interval(c);
        let len = end - start;
        let half = (self.fft.len() / 2) as u64;
        let count = len.div_ceil(half).clamp(1, MAX_FFTS_PER_COLUMN);
        (start, len, count)
    }

    fn centre(start: u64, len: u64, count: u64, j: u64) -> u64 {
        start + ((2 * j + 1) as u128 * len as u128 / (2 * count) as u128) as u64
    }

    /// Feeds samples; `emit` is called for each finished column.
    pub fn push(&mut self, samples: &[f32], emit: &mut impl FnMut(&Column)) {
        let end = self.buf_start + self.buf.len() as u64;
        let room = self.total.saturating_sub(end) as usize;
        self.buf.extend_from_slice(&samples[..samples.len().min(room)]);
        self.drain(emit);
    }

    /// Signals the end of the stream and emits all remaining columns; any
    /// part of the expected duration that never arrived reads as silence.
    pub fn finish(&mut self, emit: &mut impl FnMut(&Column)) {
        self.eof = true;
        self.drain(emit);
    }

    fn drain(&mut self, emit: &mut impl FnMut(&Column)) {
        let n = self.fft.len() as u64;
        let have = self.buf_start + self.buf.len() as u64;
        while self.next < self.columns {
            let (start, len, count) = self.centres(self.next);
            let last = Self::centre(start, len, count, count - 1);
            let need = (last + n / 2).min(self.total);
            if !self.eof && have < need {
                break;
            }
            self.acc.fill(0.0);
            for j in 0..count {
                let centre = Self::centre(start, len, count, j);
                self.transform(centre);
                for (a, p) in self.acc.iter_mut().zip(&self.power) {
                    *a += *p;
                }
            }
            let scale = self.norm / count as f32;
            for (o, a) in self.out.iter_mut().zip(&self.acc) {
                let p = *a * scale;
                *o = if p > 1e-20 { (10.0 * math::log10(p as f64)) as f32 } else { FLOOR_DB };
            }
            let envelope = self.envelope(start, start + len);
            emit(&Column { index: self.next, db: &self.out, envelope });
            self.next += 1;
            // Keep only what the next column can still reach.
            if self.next < self.columns {
                let (s, l, k) = self.centres(self.next);
                let keep_from = Self::centre(s, l, k, 0).saturating_sub(n / 2).min(s);
                if keep_from > self.buf_start {
                    let drop = ((keep_from - self.buf_start) as usize).min(self.buf.len());
                    self.buf.drain(..drop);
                    self.buf_start += drop as u64;
                }
            }
        }
    }

    /// Peak and RMS of the samples in `[start, end)` that are buffered.
    fn envelope(&self, start: u64, end: u64) -> Envelope {
        let from = start.saturating_sub(self.buf_start) as usize;
        let to = (end.saturating_sub(self.buf_start) as usize).min(self.buf.len());
        let slice = &self.buf[from.min(to)..to];
        if slice.is_empty() {
            return Envelope::default();
        }
        let (mut peak, mut sum) = (0.0f32, 0.0f64);
        for &v in slice {
            peak = peak.max(v.abs());
            sum += (v * v) as f64;
        }
        Envelope { peak, rms: math::sqrt(sum / slice.len() as f64) as f32 }
    }

    fn transform(&mut self, centre: u64) {
        let n = self.fft.len();
        let first = centre as i64 - (n / 2) as i64;
        for (i, (f, w)) in self.frame.iter_mut().zip(&self.window).enumerate() {
            let at = first + i as i64;
            let v = if at < self.buf_start as i64 {
                0.0
            } else {
                *self.buf.get((at - self.buf_start as i64) as usize).unwrap_or(&0.0)
            };
            *f = v * w;
        }
        self.fft.power(&self.frame, &mut self.power);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    fn run(samples: &[f32], bits: u32, w: WindowFunction, cols: usize, chunk: usize) -> Spectrogram {
        let mut a = Analyzer::new(bits, w, samples.len() as u64, cols);
        let mut s = Spectrogram::new(cols, a.bands());
        for part in samples.chunks(chunk) {
            a.push(part, &mut |c| s.set(c));
        }
        a.finish(&mut |c| s.set(c));
        assert_eq!(s.filled(), cols);
        s
    }

    #[test]
    fn full_scale_sine_reads_zero_db() {
        let n = 1 << 11;
        let bin = 100;
        let x: std::vec::Vec<f32> =
            (0..48000).map(|i| (2.0 * core::f64::consts::PI * bin as f64 * i as f64 / n as f64).sin() as f32).collect();
        for w in WindowFunction::ALL {
            let s = run(&x, 11, w, 40, 1000);
            for c in 2..38 {
                let col = s.column(c);
                assert!((col[bin] - 0.0).abs() < 0.05, "{w:?} col {c}: {}", col[bin]);
                assert!(col[bin + 40] < -60.0);
                let e = s.envelope(c);
                assert!(
                    (e.peak - 1.0).abs() < 0.01 && (e.rms - core::f32::consts::FRAC_1_SQRT_2).abs() < 0.01,
                    "{e:?}"
                );
            }
        }
    }

    #[test]
    fn chunking_does_not_change_result() {
        let x: std::vec::Vec<f32> = (0..20000).map(|i| ((i * 31) % 97) as f32 / 97.0 - 0.5).collect();
        let a = run(&x, 9, WindowFunction::Hann, 33, 7);
        let b = run(&x, 9, WindowFunction::Hann, 33, 20000);
        for c in 0..33 {
            assert_eq!(a.column(c), b.column(c));
            assert_eq!(a.envelope(c), b.envelope(c));
        }
    }

    #[test]
    fn short_stream_and_wide_image() {
        // More columns than samples, and a stream that ends early.
        let x = [0.5f32; 10];
        let mut a = Analyzer::new(8, WindowFunction::Hann, 100, 300);
        let mut n = 0;
        a.push(&x, &mut |_| n += 1);
        a.finish(&mut |_| n += 1);
        assert_eq!(n, 300);
    }
}
