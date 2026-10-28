//! Runs decoding and analysis, either inline or on a background thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spectro_core::analysis::{Analyzer, Column, Spectrogram, WindowFunction, bands};

use crate::media::Decoder;

#[derive(Clone, Debug)]
pub struct Request {
    pub path: PathBuf,
    pub stream: usize,
    pub channel: usize,
    pub channels: usize,
    pub sample_rate: u32,
    /// Samples in the whole stream.
    pub samples: u64,
    /// The time range to analyse, in seconds.
    pub t0: f64,
    pub t1: f64,
    pub fft_bits: u32,
    pub window: WindowFunction,
    pub columns: usize,
}

/// Decodes and analyses `req`, calling `emit` for each finished column.
/// Stops early, without error, once `cancel` is set.
pub fn run(req: &Request, cancel: &AtomicBool, emit: &mut impl FnMut(&Column)) -> Result<(), String> {
    if req.columns == 0 {
        return Ok(());
    }
    let sr = req.sample_rate.max(1) as f64;
    let start = ((req.t0 * sr).round().max(0.0) as u64).min(req.samples);
    let end = ((req.t1 * sr).round() as u64).clamp(start, req.samples);
    let (mut decoder, mut analyzer) = if start == 0 && end == req.samples {
        let decoder = Decoder::open(&req.path, req.stream, req.channel, req.channels, 0.0, None)?;
        (decoder, Analyzer::new(req.fft_bits, req.window, req.samples, req.columns))
    } else {
        // Start a window early and stop a window late so the edge columns
        // see real audio rather than padding.
        let n = 1u64 << req.fft_bits;
        let origin = start.saturating_sub(n);
        let lead = start - origin;
        let limit = lead + (end - start) + n;
        let (from, length) = (origin as f64 / sr, Some(limit as f64 / sr));
        let decoder = Decoder::open(&req.path, req.stream, req.channel, req.channels, from, length)?;
        (decoder, Analyzer::with_range(req.fft_bits, req.window, lead, end - start, limit, req.columns))
    };
    feed(&mut decoder, &mut analyzer, req, cancel, emit)
}

fn feed(
    decoder: &mut Decoder,
    analyzer: &mut Analyzer,
    req: &Request,
    cancel: &AtomicBool,
    emit: &mut impl FnMut(&Column),
) -> Result<(), String> {
    let mut block = Vec::new();
    while decoder.read(&mut block).map_err(|e| format!("Cannot decode: {e}"))? {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        analyzer.push(&block, emit);
        if analyzer.done() == req.columns {
            break;
        }
    }
    analyzer.finish(emit);
    Ok(())
}

pub struct Shared {
    pub spectrogram: Spectrogram,
    pub error: Option<String>,
}

/// A background analysis. Dropping it cancels the work.
pub struct Job {
    cancel: Arc<AtomicBool>,
    pub shared: Arc<Mutex<Shared>>,
    pub request: Request,
}

impl Job {
    /// Starts `req` on a new thread; `notify` is called (at most ~30 times a
    /// second) when new columns are available, and once at the end.
    pub fn spawn(req: Request, notify: impl Fn() + Send + 'static) -> Job {
        let cancel = Arc::new(AtomicBool::new(false));
        let spectrogram = Spectrogram::new(req.columns, bands(req.fft_bits));
        let shared = Arc::new(Mutex::new(Shared { spectrogram, error: None }));
        let (c, s, r) = (cancel.clone(), shared.clone(), req.clone());
        std::thread::spawn(move || {
            let mut last = Instant::now();
            let result = run(&r, &c, &mut |col| {
                if let Ok(mut sh) = s.lock() {
                    sh.spectrogram.set(col);
                }
                if last.elapsed() > Duration::from_millis(33) {
                    last = Instant::now();
                    notify();
                }
            });
            if let (Err(e), Ok(mut sh)) = (result, s.lock()) {
                sh.error = Some(e);
            }
            if !c.load(Ordering::Relaxed) {
                notify();
            }
        });
        Job { cancel, shared, request: req }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
