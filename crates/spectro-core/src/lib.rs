//! The spectro engine: everything between decoded samples and finished
//! pixels, with no dependencies and no `std`.
//!
//! - [`analysis`]: streaming windowed FFTs into a dBFS [`analysis::Spectrogram`]
//! - [`palette`]: Spek's palettes and perceptually uniform OKLab ones
//! - [`render`]: the full picture with header, rulers and legend
//! - [`png`]: a small PNG encoder for saving it

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod analysis;
pub mod canvas;
pub mod fft;
pub mod font;
pub mod math;
pub mod palette;
pub mod png;
pub mod render;

/// Steps through `all` from `cur` by `step`, wrapping around.
pub(crate) fn cycle<T: Copy + PartialEq>(all: &[T], cur: T, step: isize) -> T {
    let i = all.iter().position(|&x| x == cur).unwrap_or(0) as isize;
    all[(i + step).rem_euclid(all.len() as isize) as usize]
}
