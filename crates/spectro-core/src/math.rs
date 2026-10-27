//! Elementary functions for `no_std`.
//!
//! `core` has no transcendental functions, so these are small, accurate
//! (close to 1 ulp in f64) implementations of what the engine needs.

pub const PI: f64 = core::f64::consts::PI;
const LN2: f64 = core::f64::consts::LN_2;
const LN10: f64 = core::f64::consts::LN_10;

/// Largest integer value not greater than `x`.
pub fn floor(x: f64) -> f64 {
    if x.is_nan() || x.abs() >= 4.5e15 {
        return x; // already integral, infinite or NaN
    }
    let t = x as i64 as f64;
    if t > x { t - 1.0 } else { t }
}

pub fn ceil(x: f64) -> f64 {
    -floor(-x)
}

pub fn round(x: f64) -> f64 {
    floor(x + 0.5)
}

pub fn floorf(x: f32) -> f32 {
    floor(x as f64) as f32
}

pub fn ceilf(x: f32) -> f32 {
    ceil(x as f64) as f32
}

/// Scales `x` by `2^k` for `k` in the normal exponent range.
fn ldexp(x: f64, k: i64) -> f64 {
    let k = k.clamp(-1022, 1023);
    x * f64::from_bits(((k + 1023) as u64) << 52)
}

pub fn sqrt(x: f64) -> f64 {
    if x <= 0.0 || x.is_nan() || x.is_infinite() {
        return if x == 0.0 || x == f64::INFINITY { x } else { f64::NAN };
    }
    if x < f64::MIN_POSITIVE {
        return sqrt(x * ldexp(1.0, 108)) * ldexp(1.0, -54);
    }
    // The halved exponent gives a guess within a factor of 2; Newton doubles
    // the number of correct bits per step.
    let mut y = f64::from_bits((x.to_bits() >> 1) + (1023u64 << 51));
    for _ in 0..6 {
        y = 0.5 * (y + x / y);
    }
    y
}

pub fn exp(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x > 709.0 {
        return f64::INFINITY;
    }
    if x < -745.0 {
        return 0.0;
    }
    // exp(x) = 2^k * exp(r), |r| <= ln2/2.
    let k = round(x / LN2);
    let r = x - k * LN2;
    let mut term = 1.0;
    let mut sum = 1.0;
    for i in 1..=14 {
        term *= r / i as f64;
        sum += term;
    }
    if k < -1000.0 { ldexp(ldexp(sum, k as i64 + 500), -500) } else { ldexp(sum, k as i64) }
}

/// Natural logarithm. Returns `-inf` for 0 and NaN for negative input.
pub fn ln(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if x.is_infinite() {
        return x;
    }
    let mut bits = x.to_bits();
    let mut e: i64 = 0;
    if bits >> 52 == 0 {
        // Subnormal: normalise first.
        bits = (x * f64::from_bits(0x4350_0000_0000_0000)).to_bits(); // 2^54
        e -= 54;
    }
    e += ((bits >> 52) & 0x7ff) as i64 - 1023;
    let mut m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
    if m > core::f64::consts::SQRT_2 {
        m *= 0.5;
        e += 1;
    }
    // ln(m) = 2 atanh(s), s = (m - 1) / (m + 1), |s| < 0.172.
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    let mut term = s;
    let mut sum = 0.0;
    let mut i = 1.0;
    while i < 40.0 {
        sum += term / i;
        term *= s2;
        i += 2.0;
    }
    2.0 * sum + e as f64 * LN2
}

pub fn log10(x: f64) -> f64 {
    ln(x) / LN10
}

/// `x^y` for `x >= 0`.
pub fn pow(x: f64, y: f64) -> f64 {
    if y == 0.0 {
        return 1.0;
    }
    if x == 0.0 {
        return if y > 0.0 { 0.0 } else { f64::INFINITY };
    }
    exp(y * ln(x))
}

/// Real cube root.
pub fn cbrt(x: f64) -> f64 {
    if x == 0.0 || x.is_nan() {
        return x;
    }
    let a = x.abs();
    let mut y = exp(ln(a) / 3.0);
    y -= (y * y * y - a) / (3.0 * y * y); // polish the last bits
    if x < 0.0 { -y } else { y }
}

/// Sine and cosine of `x` together.
pub fn sin_cos(x: f64) -> (f64, f64) {
    if !x.is_finite() {
        return (f64::NAN, f64::NAN);
    }
    // Reduce to r in [-pi/4, pi/4] and a quadrant. Two-part pi/2 keeps the
    // reduction exact enough for the arguments used here (|x| < 1e6).
    const PIO2_HI: f64 = 1.570_796_326_734_125_6;
    const PIO2_LO: f64 = 6.077_100_506_506_192e-11;
    let q = round(x / (PI / 2.0));
    let r = (x - q * PIO2_HI) - q * PIO2_LO;
    let r2 = r * r;
    // Taylor series to degree 19/20; the error is far below 1 ulp at pi/4.
    let mut s = 0.0;
    let mut c = 0.0;
    let mut ts = r;
    let mut tc = 1.0;
    for i in 0..10 {
        s += ts;
        c += tc;
        let n = (2 * i + 2) as f64;
        ts *= -r2 / (n * (n + 1.0));
        tc *= -r2 / ((n - 1.0) * n);
    }
    match (q as i64).rem_euclid(4) {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    }
}

pub fn sin(x: f64) -> f64 {
    sin_cos(x).0
}

pub fn cos(x: f64) -> f64 {
    sin_cos(x).1
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    fn close(a: f64, b: f64, rel: f64) -> bool {
        a == b || (a - b).abs() <= rel * b.abs().max(1e-300)
    }

    #[test]
    fn matches_std() {
        let mut x = -50.0;
        while x < 50.0 {
            assert!(close(sin(x), x.sin(), 1e-12) || (sin(x) - x.sin()).abs() < 1e-15, "sin {x}");
            assert!(close(cos(x), x.cos(), 1e-12) || (cos(x) - x.cos()).abs() < 1e-15, "cos {x}");
            assert!(close(exp(x), x.exp(), 1e-14), "exp {x}");
            assert!(close(cbrt(x), x.cbrt(), 1e-14), "cbrt {x}");
            assert_eq!(floor(x), x.floor());
            assert_eq!(ceil(x), x.ceil());
            x += 0.0137;
        }
        for &v in &[1e-310, 1e-30, 0.001, 0.5, 0.7, 1.0, 1.5, 2.0, 10.0, 12345.678, 1e300] {
            assert!(close(ln(v), v.ln(), 1e-14) || (ln(v) - v.ln()).abs() < 1e-15, "ln {v}");
            assert!(close(log10(v), v.log10(), 1e-14) || (log10(v) - v.log10()).abs() < 1e-15);
            assert!(close(sqrt(v), v.sqrt(), 1e-15), "sqrt {v}");
            assert!(close(pow(v, 2.4), v.powf(2.4), 1e-13), "pow {v}");
        }
        assert_eq!(ln(0.0), f64::NEG_INFINITY);
        assert!(ln(-1.0).is_nan());
    }
}
