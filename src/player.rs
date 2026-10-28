//! Playback through `ffplay`: a time and frequency region on loop, or the
//! file from some point on, live.
//!
//! Like decoding, playback runs FFmpeg as a child process: `ffplay` seeks
//! to the start, and an FFT brick-wall filter (`afftfilt`) silences every
//! frequency outside the band. Live playback reads `ffplay`'s status line,
//! which gives the time actually being heard, so a picture can follow it.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::media::{input_arg, tool};

/// Seconds and hertz.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub t0: f64,
    pub t1: f64,
    pub f0: f64,
    pub f1: f64,
}

/// The last time `ffplay` reported, and when it did.
type Clock = Arc<Mutex<Option<(f64, Instant)>>>;

pub struct Player {
    child: Child,
    started: Instant,
    length: f64,
    /// For live playback.
    clock: Option<Clock>,
}

/// The playing time at the start of an `ffplay` status line, e.g.
/// `"  10.02 M-A:  0.000 fd=   0 ..."`.
fn parse_status(line: &str) -> Option<f64> {
    // Some builds clear the terminal line first.
    let first = line.trim_start_matches("\u{1b}[2K").split_whitespace().next()?;
    first.parse::<f64>().ok().filter(|t| t.is_finite())
}

/// The `afftfilt` expression keeping bins in `[f0, f1]`, or `None` when the
/// band is the whole spectrum.
fn band_filter(f0: f64, f1: f64, nyquist: f64) -> Option<String> {
    if f0 <= 1.0 && f1 >= nyquist * 0.999 {
        return None;
    }
    let keep = format!("between(b*sr/(2*(nb-1)),{f0:.1},{f1:.1})");
    Some(format!("afftfilt=real='re*{keep}':imag='im*{keep}':win_size=4096"))
}

fn ffplay(stream: usize, from: f64, (f0, f1): (f64, f64), nyquist: f64) -> Command {
    let mut cmd = Command::new(tool("ffplay"));
    cmd.args(["-v", "error", "-nodisp", "-hide_banner"])
        .arg("-ss")
        .arg(format!("{:.3}", from.max(0.0)))
        .arg("-ast")
        .arg(format!("a:{stream}"));
    if let Some(af) = band_filter(f0, f1, nyquist) {
        cmd.arg("-af").arg(af);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null());
    cmd
}

fn spawn(cmd: &mut Command) -> Result<Child, String> {
    cmd.spawn().map_err(|e| format!("Cannot run ffplay ({e}); it comes with FFmpeg"))
}

impl Player {
    /// Loops the region `r` forever.
    pub fn start(path: &Path, stream: usize, r: Region, nyquist: f64) -> Result<Self, String> {
        let length = (r.t1 - r.t0).max(0.01);
        let mut cmd = ffplay(stream, r.t0, (r.f0, r.f1), nyquist);
        cmd.args(["-nostats", "-loop", "0", "-t", &format!("{length:.3}")]).arg(input_arg(path));
        let child = spawn(cmd.stderr(Stdio::null()))?;
        Ok(Player { child, started: Instant::now(), length, clock: None })
    }

    /// Plays from `from` seconds to the end, keeping only `band` hertz.
    pub fn live(path: &Path, stream: usize, from: f64, band: (f64, f64), nyquist: f64) -> Result<Self, String> {
        let mut cmd = ffplay(stream, from, band, nyquist);
        cmd.args(["-stats", "-autoexit"]).arg(input_arg(path));
        let mut child = spawn(cmd.stderr(Stdio::piped()))?;
        let clock: Clock = Arc::new(Mutex::new(None));
        if let Some(mut err) = child.stderr.take() {
            let clock = clock.clone();
            std::thread::spawn(move || {
                // Status lines end in a carriage return; read them as they come.
                let (mut buf, mut line) = ([0u8; 512], Vec::new());
                while let Ok(n) = err.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    for &b in &buf[..n] {
                        if b != b'\r' && b != b'\n' {
                            line.push(b);
                            continue;
                        }
                        if let Some(t) = parse_status(&String::from_utf8_lossy(&line))
                            && let Ok(mut c) = clock.lock()
                        {
                            *c = Some((t, Instant::now()));
                        }
                        line.clear();
                    }
                }
            });
        }
        Ok(Player { child, started: Instant::now(), length: f64::INFINITY, clock: Some(clock) })
    }

    /// Seconds into the region, wrapping at its end.
    pub fn position(&self) -> f64 {
        self.started.elapsed().as_secs_f64() % self.length
    }

    /// For live playback, the file time being heard, once `ffplay` has
    /// started; between its reports it is carried forward by the wall clock.
    pub fn clock(&self) -> Option<f64> {
        let (t, at) = (*self.clock.as_ref()?.lock().ok()?)?;
        Some(t + at.elapsed().as_secs_f64().min(0.5))
    }

    /// `None` while `ffplay` runs; once it has exited, whether it succeeded.
    /// A loop only ends on an error; live playback also at the end.
    pub fn finished(&mut self) -> Option<bool> {
        match self.child.try_wait() {
            Ok(None) => None,
            Ok(Some(status)) => Some(status.success()),
            Err(_) => Some(false),
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_status_clock() {
        assert_eq!(parse_status("  10.02 M-A:  0.000 fd=   0 aq=   56KB vq=    0KB sq=    0B "), Some(10.02));
        assert_eq!(parse_status("    nan M-A:    nan fd=   0"), None);
        assert_eq!(parse_status("\u{1b}[2K"), None);
        assert_eq!(parse_status("\u{1b}[2K   3.50 M-A:  0.000"), Some(3.5));
        assert_eq!(parse_status(""), None);
    }

    #[test]
    fn filter_only_when_band_is_narrower() {
        assert_eq!(band_filter(0.0, 22050.0, 22050.0), None);
        let f = band_filter(4000.0, 6000.0, 22050.0).unwrap();
        assert!(f.starts_with("afftfilt=real='re*between(b*sr/(2*(nb-1)),4000.0,6000.0)'"), "{f}");
    }
}
