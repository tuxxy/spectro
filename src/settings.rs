//! View settings and their persistence in a small `key=value` file.

use std::path::PathBuf;

use spectro_core::analysis::{DEFAULT_FFT_BITS, MAX_FFT_BITS, MIN_FFT_BITS, WindowFunction};
use spectro_core::palette::Palette;
use spectro_core::render::{DEFAULT_LOWER_DB, DEFAULT_UPPER_DB, FreqScale, MAX_DB, MIN_DB};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub palette: Palette,
    pub window: WindowFunction,
    pub fft_bits: u32,
    pub upper_db: i32,
    pub lower_db: i32,
    pub glow: bool,
    pub freq_scale: FreqScale,
    /// Submarine mode: a green sonar waterfall.
    pub sonar: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            palette: Palette::default(),
            window: WindowFunction::default(),
            fft_bits: DEFAULT_FFT_BITS,
            upper_db: DEFAULT_UPPER_DB,
            lower_db: DEFAULT_LOWER_DB,
            glow: true,
            freq_scale: FreqScale::default(),
            sonar: false,
        }
    }
}

/// `$XDG_CONFIG_HOME/spectro`, `~/.config/spectro` or `%APPDATA%\spectro`.
pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("spectro"))
}

fn path() -> Option<PathBuf> {
    Some(config_dir()?.join("preferences"))
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Settings::default();
        if let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) {
            for line in text.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    s.set(k.trim(), v.trim());
                }
            }
        }
        s.clamp();
        s
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        let text = format!(
            "# spectro preferences\npalette={}\nwindow_function={}\nwindow_size={}\nupper_db={}\nlower_db={}\nglow={}\nfreq_scale={}\nsonar={}\n",
            self.palette.id(),
            self.window.id(),
            1u32 << self.fft_bits,
            self.upper_db,
            self.lower_db,
            on_off(self.glow),
            self.freq_scale.id(),
            on_off(self.sonar)
        );
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, text);
    }

    /// Applies one setting by name; returns false for unknown names or values.
    pub fn set(&mut self, key: &str, value: &str) -> bool {
        let ok = match key {
            "palette" => Palette::from_id(&value.to_ascii_lowercase()).map(|p| self.palette = p).is_some(),
            "window_function" => {
                WindowFunction::from_id(&value.to_ascii_lowercase()).map(|w| self.window = w).is_some()
            }
            "window_size" => match value.parse::<u32>() {
                Ok(n) if n.is_power_of_two() && (MIN_FFT_BITS..=MAX_FFT_BITS).contains(&n.trailing_zeros()) => {
                    self.fft_bits = n.trailing_zeros();
                    true
                }
                _ => false,
            },
            "upper_db" => value.parse().map(|v| self.upper_db = v).is_ok(),
            "lower_db" => value.parse().map(|v| self.lower_db = v).is_ok(),
            "freq_scale" => FreqScale::from_id(&value.to_ascii_lowercase()).map(|f| self.freq_scale = f).is_some(),
            "glow" => parse_on_off(value).map(|v| self.glow = v).is_some(),
            "sonar" => parse_on_off(value).map(|v| self.sonar = v).is_some(),
            _ => false,
        };
        self.clamp();
        ok
    }

    fn clamp(&mut self) {
        self.fft_bits = self.fft_bits.clamp(MIN_FFT_BITS, MAX_FFT_BITS);
        self.upper_db = self.upper_db.clamp(MIN_DB + 1, MAX_DB);
        self.lower_db = self.lower_db.clamp(MIN_DB, self.upper_db - 1);
    }

    /// Spek's spectrogram keys. Returns whether the key was one of them.
    pub fn key(&mut self, c: char) -> bool {
        match c {
            'f' => self.window = self.window.next(),
            'F' => self.window = self.window.prev(),
            'p' => self.palette = self.palette.next(),
            'P' => self.palette = self.palette.prev(),
            'l' => self.lower_db = (self.lower_db + 1).min(self.upper_db - 1),
            'L' => self.lower_db = (self.lower_db - 1).max(MIN_DB),
            'u' => self.upper_db = (self.upper_db + 1).min(MAX_DB),
            'U' => self.upper_db = (self.upper_db - 1).max(self.lower_db + 1),
            'w' => self.fft_bits = (self.fft_bits + 1).min(MAX_FFT_BITS),
            'W' => self.fft_bits = (self.fft_bits - 1).max(MIN_FFT_BITS),
            'g' | 'G' => self.glow = !self.glow,
            'm' | 'M' => self.sonar = !self.sonar,
            'y' => self.freq_scale = self.freq_scale.next(),
            'Y' => self.freq_scale = self.freq_scale.prev(),
            _ => return false,
        }
        true
    }
}

fn on_off(v: bool) -> &'static str {
    if v { "on" } else { "off" }
}

fn parse_on_off(v: &str) -> Option<bool> {
    match v {
        "on" | "true" | "1" => Some(true),
        "off" | "false" | "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_respect_limits() {
        let mut s = Settings::default();
        for _ in 0..200 {
            s.key('U');
        }
        assert_eq!(s.upper_db, s.lower_db + 1);
        for _ in 0..200 {
            s.key('L');
        }
        assert_eq!(s.lower_db, MIN_DB);
        for _ in 0..20 {
            s.key('w');
        }
        assert_eq!(s.fft_bits, MAX_FFT_BITS);
        assert!(s.set("window_size", "256") && s.fft_bits == 8);
        assert!(!s.set("window_size", "300"));
        assert!(s.set("palette", "SoX") && s.palette == Palette::Sox);
        assert!(s.set("sonar", "on") && s.sonar);
        assert!(s.key('m') && !s.sonar);
        assert!(!s.set("sonar", "maybe"));
    }
}
