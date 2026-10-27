//! Real-input FFT returning the power spectrum.
//!
//! A length-`n` real transform is computed as a length-`n/2` complex
//! radix-2 transform of the even/odd samples packed into one complex
//! sequence, followed by the usual split step.

use alloc::vec;
use alloc::vec::Vec;

use crate::math;

#[derive(Clone, Copy, Default)]
struct C {
    re: f32,
    im: f32,
}

impl C {
    fn mul(self, o: C) -> C {
        C { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re }
    }
}

pub struct RealFft {
    n: usize,
    bitrev: Vec<u32>,
    twiddle: Vec<C>,
    split: Vec<C>,
    buf: Vec<C>,
}

impl RealFft {
    /// Creates a plan for `n = 2^bits` real samples, `2 <= bits <= 24`.
    pub fn new(bits: u32) -> Self {
        assert!((2..=24).contains(&bits), "unsupported FFT size");
        let n = 1usize << bits;
        let m = n / 2;
        let mbits = bits - 1;
        let bitrev = (0..m as u32).map(|i| if mbits == 0 { 0 } else { i.reverse_bits() >> (32 - mbits) }).collect();
        let tw = |k: usize, len: usize| {
            let (s, c) = math::sin_cos(-2.0 * math::PI * k as f64 / len as f64);
            C { re: c as f32, im: s as f32 }
        };
        let twiddle = (0..m / 2).map(|k| tw(k, m)).collect();
        let split = (0..m).map(|k| tw(k, n)).collect();
        RealFft { n, bitrev, twiddle, split, buf: vec![C::default(); m] }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Writes `|X[k]|^2` for `k = 0..=n/2` into `power` (length `n/2 + 1`).
    pub fn power(&mut self, input: &[f32], power: &mut [f32]) {
        let m = self.n / 2;
        assert_eq!(input.len(), self.n);
        assert_eq!(power.len(), m + 1);
        for (i, &r) in self.bitrev.iter().enumerate() {
            let j = r as usize;
            self.buf[i] = C { re: input[2 * j], im: input[2 * j + 1] };
        }
        let mut len = 2;
        while len <= m {
            let half = len / 2;
            let stride = m / len;
            for start in (0..m).step_by(len) {
                for k in 0..half {
                    let w = self.twiddle[k * stride];
                    let a = self.buf[start + k];
                    let b = self.buf[start + k + half].mul(w);
                    self.buf[start + k] = C { re: a.re + b.re, im: a.im + b.im };
                    self.buf[start + k + half] = C { re: a.re - b.re, im: a.im - b.im };
                }
            }
            len *= 2;
        }
        let z0 = self.buf[0];
        power[0] = (z0.re + z0.im) * (z0.re + z0.im);
        power[m] = (z0.re - z0.im) * (z0.re - z0.im);
        for (k, out) in power.iter_mut().enumerate().take(m).skip(1) {
            let a = self.buf[k];
            let b = self.buf[m - k];
            // Even part E = (a + conj b) / 2, odd part O = (a - conj b) / 2i.
            let e = C { re: 0.5 * (a.re + b.re), im: 0.5 * (a.im - b.im) };
            let o = C { re: 0.5 * (a.im + b.im), im: -0.5 * (a.re - b.re) };
            let t = o.mul(self.split[k]);
            let (re, im) = (e.re + t.re, e.im + t.im);
            *out = re * re + im * im;
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    #[allow(clippy::needless_range_loop)]
    fn matches_naive_dft() {
        for bits in [2u32, 3, 6, 9] {
            let n = 1 << bits;
            let x: Vec<f32> = (0..n).map(|i| ((i * 7919) % 13) as f32 / 6.0 - 1.0 + (i as f32 * 0.3).sin()).collect();
            let mut fft = RealFft::new(bits);
            let mut p = vec![0.0; n / 2 + 1];
            fft.power(&x, &mut p);
            for k in 0..=n / 2 {
                let (mut re, mut im) = (0.0f64, 0.0f64);
                for (i, &v) in x.iter().enumerate() {
                    let a = -2.0 * core::f64::consts::PI * (k * i) as f64 / n as f64;
                    re += v as f64 * a.cos();
                    im += v as f64 * a.sin();
                }
                let want = re * re + im * im;
                assert!((p[k] as f64 - want).abs() <= 1e-3 * want.max(1.0), "n={n} k={k}");
            }
        }
    }
}
