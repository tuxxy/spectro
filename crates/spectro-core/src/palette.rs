//! Colour palettes.
//!
//! Spek's three palettes are reproduced exactly. The others are designed in
//! OKLab: colour stops are blended in a perceptual space and the curve is
//! then re-parametrised so equal steps in level are equal steps in perceived
//! colour difference. Lightness rises monotonically, so loudness reads the
//! same in colour and in greyscale, without bands or kinks.

use alloc::vec::Vec;

use crate::math;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Palette {
    /// Black, violet, crimson, amber, cream.
    #[default]
    Ember,
    /// Spek's default: Dan Bruton's visible spectrum.
    Spectrum,
    /// The SoX palette by Rob Sykes.
    Sox,
    /// Black to white.
    Mono,
    /// Black, deep blue, teal, ice.
    Glacier,
    /// Black, indigo, sea green, lime, cream.
    Aurora,
}

impl Palette {
    pub const ALL: [Palette; 6] =
        [Palette::Ember, Palette::Spectrum, Palette::Sox, Palette::Mono, Palette::Glacier, Palette::Aurora];

    pub fn name(self) -> &'static str {
        match self {
            Palette::Ember => "Ember",
            Palette::Spectrum => "Spectrum",
            Palette::Sox => "SoX",
            Palette::Mono => "Mono",
            Palette::Glacier => "Glacier",
            Palette::Aurora => "Aurora",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Palette::Ember => "ember",
            Palette::Spectrum => "spectrum",
            Palette::Sox => "sox",
            Palette::Mono => "mono",
            Palette::Glacier => "glacier",
            Palette::Aurora => "aurora",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == s)
    }

    pub fn next(self) -> Self {
        crate::cycle(&Self::ALL, self, 1)
    }

    pub fn prev(self) -> Self {
        crate::cycle(&Self::ALL, self, -1)
    }

    fn stops(self) -> Option<&'static [u32]> {
        Some(match self {
            Palette::Ember => {
                &[0x000000, 0x140b34, 0x3b0f6f, 0x6d1a7e, 0xa12a6f, 0xd2454f, 0xf0752c, 0xfaae2e, 0xfbe08a, 0xfffcf0]
            }
            Palette::Glacier => {
                &[0x000000, 0x07122e, 0x0e2c63, 0x1a5390, 0x2783ad, 0x3db5bf, 0x86dccd, 0xd4f4ea, 0xffffff]
            }
            Palette::Aurora => {
                &[0x000000, 0x0f0c2c, 0x2a1b63, 0x2d4590, 0x20799a, 0x1fa58b, 0x62c660, 0xc6e05a, 0xfff5c8]
            }
            _ => return None,
        })
    }
}

/// A sampled palette with 8-bit sRGB output and ordered dithering.
pub struct Lut {
    palette: Palette,
    rgb: Vec<[f32; 3]>,
}

const LUT_SIZE: usize = 2048;

/// 8x8 Bayer matrix, used to dither the final quantisation to 8 bits so
/// smooth gradients do not show bands.
const BAYER: [u8; 64] = [
    0, 32, 8, 40, 2, 34, 10, 42, 48, 16, 56, 24, 50, 18, 58, 26, 12, 44, 4, 36, 14, 46, 6, 38, 60, 28, 52, 20, 62, 30,
    54, 22, 3, 35, 11, 43, 1, 33, 9, 41, 51, 19, 59, 27, 49, 17, 57, 25, 15, 47, 7, 39, 13, 45, 5, 37, 63, 31, 55, 23,
    61, 29, 53, 21,
];

impl Lut {
    pub fn new(palette: Palette) -> Self {
        let rgb = match palette.stops() {
            Some(stops) => oklab_ramp(stops, LUT_SIZE),
            None => (0..LUT_SIZE)
                .map(|i| {
                    let level = i as f64 / (LUT_SIZE - 1) as f64;
                    let [r, g, b] = match palette {
                        Palette::Spectrum => spek_spectrum(level),
                        Palette::Sox => spek_sox(level),
                        _ => [level; 3],
                    };
                    [(r * 255.0) as f32, (g * 255.0) as f32, (b * 255.0) as f32]
                })
                .collect(),
        };
        Lut { palette, rgb }
    }

    pub fn palette(&self) -> Palette {
        self.palette
    }

    /// Colour for `level` in `0..=1` as unquantised sRGB in `0..=255`.
    pub fn sample(&self, level: f32) -> [f32; 3] {
        let pos = level.clamp(0.0, 1.0) * (LUT_SIZE - 1) as f32;
        let i = (pos as usize).min(LUT_SIZE - 2);
        let t = pos - i as f32;
        let (a, b) = (self.rgb[i], self.rgb[i + 1]);
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
    }

    /// Dithered `0x00RRGGBB` colour for `level` at pixel `(x, y)`.
    pub fn color(&self, level: f32, x: usize, y: usize) -> u32 {
        quantize(self.sample(level), x, y)
    }
}

/// Rounds an sRGB colour in `0..=255` to 8 bits with ordered dithering, so
/// smooth gradients keep no visible steps.
pub fn quantize([r, g, b]: [f32; 3], x: usize, y: usize) -> u32 {
    let d = (BAYER[(y & 7) * 8 + (x & 7)] as f32 + 0.5) / 64.0;
    let q = |v: f32| (v + d).clamp(0.0, 255.0) as u32;
    (q(r) << 16) | (q(g) << 8) | q(b)
}

// Modified version of Dan Bruton's algorithm, as in Spek.
fn spek_spectrum(level: f64) -> [f64; 3] {
    let level = level * 0.6625;
    let (r, g, b) = if level < 0.15 {
        ((0.15 - level) / (0.15 + 0.075), 0.0, 1.0)
    } else if level < 0.275 {
        (0.0, (level - 0.15) / (0.275 - 0.15), 1.0)
    } else if level < 0.325 {
        (0.0, 1.0, (0.325 - level) / (0.325 - 0.275))
    } else if level < 0.5 {
        ((level - 0.325) / (0.5 - 0.325), 1.0, 0.0)
    } else if level < 0.6625 {
        (1.0, (0.6625 - level) / (0.6625 - 0.5), 0.0)
    } else {
        (0.0, 0.0, 0.0)
    };
    let cf = if level < 0.1 { level / 0.1 } else { 1.0 };
    [r * cf, g * cf, b * cf]
}

// The default SoX palette, as in Spek.
fn spek_sox(level: f64) -> [f64; 3] {
    let h = math::PI / 2.0;
    let r = if level >= 0.73 {
        1.0
    } else if level >= 0.13 {
        math::sin((level - 0.13) / 0.60 * h)
    } else {
        0.0
    };
    let g = if level >= 0.91 {
        1.0
    } else if level >= 0.6 {
        math::sin((level - 0.6) / 0.31 * h)
    } else {
        0.0
    };
    let b = if level < 0.6 {
        0.5 * math::sin(level / 0.6 * math::PI)
    } else if level >= 0.78 {
        (level - 0.78) / 0.22
    } else {
        0.0
    };
    [r, g, b]
}

fn to_linear(c: f64) -> f64 {
    if c <= 0.04045 { c / 12.92 } else { math::pow((c + 0.055) / 1.055, 2.4) }
}

fn to_srgb(c: f64) -> f64 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * math::pow(c, 1.0 / 2.4) - 0.055 }
}

fn hex_to_oklab(hex: u32) -> [f64; 3] {
    let ch = |s: u32| to_linear(((hex >> s) & 0xff) as f64 / 255.0);
    let (r, g, b) = (ch(16), ch(8), ch(0));
    let l = math::cbrt(0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b);
    let m = math::cbrt(0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b);
    let s = math::cbrt(0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b);
    [
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    ]
}

fn oklab_to_srgb255([ll, a, b]: [f64; 3]) -> [f32; 3] {
    let l = ll + 0.396_337_777_4 * a + 0.215_803_757_3 * b;
    let m = ll - 0.105_561_345_8 * a - 0.063_854_172_8 * b;
    let s = ll - 0.089_484_177_5 * a - 1.291_485_548_0 * b;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    let r = 4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s;
    let g = -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s;
    let bb = -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s;
    [(to_srgb(r) * 255.0) as f32, (to_srgb(g) * 255.0) as f32, (to_srgb(bb) * 255.0) as f32]
}

/// Samples a perceptually uniform ramp through `stops`.
fn oklab_ramp(stops: &[u32], size: usize) -> Vec<[f32; 3]> {
    let lab: Vec<[f64; 3]> = stops.iter().map(|&h| hex_to_oklab(h)).collect();
    // Dense polyline through the stops (Catmull-Rom keeps the joins smooth).
    const DENSE: usize = 8192;
    let segs = lab.len() - 1;
    let at = |t: f64| -> [f64; 3] {
        let x = t * segs as f64;
        let i = (x as usize).min(segs - 1);
        let u = x - i as f64;
        let p = |k: isize| lab[(i as isize + k).clamp(0, segs as isize) as usize];
        let (p0, p1, p2, p3) = (p(-1), p(0), p(1), p(2));
        let mut out = [0.0; 3];
        for c in 0..3 {
            let (a, b, cc, d) = (p0[c], p1[c], p2[c], p3[c]);
            out[c] = 0.5
                * (2.0 * b
                    + (cc - a) * u
                    + (2.0 * a - 5.0 * b + 4.0 * cc - d) * u * u
                    + (3.0 * b - a - 3.0 * cc + d) * u * u * u);
        }
        out
    };
    let pts: Vec<[f64; 3]> = (0..DENSE).map(|i| at(i as f64 / (DENSE - 1) as f64)).collect();
    let mut dist = Vec::with_capacity(DENSE);
    let mut total = 0.0;
    dist.push(0.0);
    for w in pts.windows(2) {
        let d = (0..3).map(|c| (w[1][c] - w[0][c]) * (w[1][c] - w[0][c])).sum::<f64>();
        total += math::sqrt(d);
        dist.push(total);
    }
    let mut j = 0;
    (0..size)
        .map(|i| {
            let target = total * i as f64 / (size - 1) as f64;
            while j + 2 < DENSE && dist[j + 1] < target {
                j += 1;
            }
            let span = dist[j + 1] - dist[j];
            let u = if span > 0.0 { ((target - dist[j]) / span).clamp(0.0, 1.0) } else { 0.0 };
            let (a, b) = (pts[j], pts[j + 1]);
            oklab_to_srgb255([a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u, a[2] + (b[2] - a[2]) * u])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luma(c: [f32; 3]) -> f32 {
        0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
    }

    #[test]
    fn designed_palettes_are_monotonic_in_lightness() {
        for p in [Palette::Ember, Palette::Glacier, Palette::Aurora, Palette::Mono] {
            let lut = Lut::new(p);
            let mut prev = -1.0;
            for i in 0..=200 {
                let y = luma(lut.sample(i as f32 / 200.0));
                assert!(y >= prev - 0.5, "{p:?} at {i}: {y} < {prev}");
                prev = y;
            }
            assert!(luma(lut.sample(0.0)) < 1.0);
            assert!(luma(lut.sample(1.0)) > 235.0);
        }
    }

    #[test]
    fn spek_palettes_match_reference_points() {
        let s = Lut::new(Palette::Spectrum);
        assert_eq!(s.sample(0.0), [0.0, 0.0, 0.0]);
        let sox = Lut::new(Palette::Sox);
        let top = sox.sample(1.0);
        assert!(top.iter().all(|&v| v > 254.0));
    }

    #[test]
    fn oklab_round_trip() {
        for hex in [0x000000u32, 0xffffff, 0x3b0f6f, 0xfaae2e, 0x1a5390] {
            let c = oklab_to_srgb255(hex_to_oklab(hex));
            let want = [(hex >> 16) & 255, (hex >> 8) & 255, hex & 255];
            for k in 0..3 {
                assert!((c[k] - want[k] as f32).abs() < 0.01, "{hex:06x}");
            }
        }
    }
}
