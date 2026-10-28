//! Looping playback of a time and frequency region through `ffplay`.
//!
//! Like decoding, playback runs FFmpeg as a child process: `ffplay` seeks
//! to the region, loops it forever, and an FFT brick-wall filter
//! (`afftfilt`) silences every frequency outside the band.

use std::path::Path;
use std::process::{Child, Command, Stdio};
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

pub struct Player {
    child: Child,
    started: Instant,
    length: f64,
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

impl Player {
    pub fn start(path: &Path, stream: usize, r: Region, nyquist: f64) -> Result<Self, String> {
        let length = (r.t1 - r.t0).max(0.01);
        let mut cmd = Command::new(tool("ffplay"));
        cmd.args(["-v", "error", "-nodisp", "-nostats", "-hide_banner", "-loop", "0"])
            .arg("-ss")
            .arg(format!("{:.3}", r.t0.max(0.0)))
            .arg("-t")
            .arg(format!("{length:.3}"))
            .arg("-ast")
            .arg(format!("a:{stream}"));
        if let Some(af) = band_filter(r.f0, r.f1, nyquist) {
            cmd.arg("-af").arg(af);
        }
        let child = cmd
            .arg(input_arg(path))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Cannot run ffplay ({e}); it comes with FFmpeg"))?;
        Ok(Player { child, started: Instant::now(), length })
    }

    /// Seconds into the region, wrapping at its end.
    pub fn position(&self) -> f64 {
        self.started.elapsed().as_secs_f64() % self.length
    }

    /// False once `ffplay` has exited (it only does so on an error).
    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
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
    fn filter_only_when_band_is_narrower() {
        assert_eq!(band_filter(0.0, 22050.0, 22050.0), None);
        let f = band_filter(4000.0, 6000.0, 22050.0).unwrap();
        assert!(f.starts_with("afftfilt=real='re*between(b*sr/(2*(nb-1)),4000.0,6000.0)'"), "{f}");
    }
}
