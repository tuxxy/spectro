# spectro

An acoustic spectrum analyser for audio files, written in Rust. It is a
clone of [Spek](https://github.com/alexkay/spek): open a file and see its
spectrogram, with the same layout, keys and options. The aim is a small,
safe program that draws the best-looking spectrograms around.

![spectro showing an MP3 with a 16 kHz encoder cutoff](docs/screenshot.png)

## Why

- **Small and safe.** The engine (`spectro-core`) is `#![no_std]`, has no
  dependencies and no `unsafe` code. It does the FFT, the analysis, the
  palettes, the text, the layout and the PNG encoding by itself, and builds
  for bare-metal targets. The program around it is also `unsafe`-free and
  adds only `winit` and `softbuffer` for the window.
- **FFmpeg in its own process.** Audio is decoded by the `ffmpeg` and
  `ffprobe` programs, not linked in. A broken or hostile file can at worst
  crash the decoder, never spectro. File names go to FFmpeg as plain
  arguments with the `file:` prefix, so they are never read as options or
  URLs.
- **Better pictures.**
  - Each column averages the power of up to 24 overlapping windowed FFTs
    across its time slice (Welch's method), so noise looks smooth and tones
    look sharp. Spek takes one FFT per window length and averages in dB.
  - Levels are true dBFS: a full-scale sine reads 0 dB for every window
    function.
  - New palettes (Ember, the default, plus Glacier and Aurora) are designed
    in OKLab and spaced so equal steps in level look like equal steps in
    colour. Lightness only ever rises, so they also read in greyscale.
    Spek's Spectrum, SoX and Mono palettes are kept exactly.
  - Colours are dithered when rounded to 8 bits, so gradients have no
    bands.
  - When many frequency bins share one pixel row, the loudest wins, so thin
    tones never vanish; when one bin spans many rows, values are
    interpolated.
  - Sharp, anti-aliased text and lines at any display scale, including
    Retina/HiDPI, using embedded Source Sans 3 outlines.

| Spectrum (Spek's default) | Glacier |
| --- | --- |
| ![Spectrum palette](docs/spectrum.png) | ![Glacier palette](docs/glacier.png) |

## Install

You need a Rust toolchain (1.85 or newer) and FFmpeg (`ffmpeg` and
`ffprobe` on your `PATH`).

```bash
cargo install --git https://github.com/tuxxy/spectro
```

On Linux, building needs the X11 and xkbcommon headers (for example
`libx11-dev libxkbcommon-dev pkg-config` on Debian/Ubuntu). The file dialogs
use `zenity` or `kdialog`; on macOS they use `osascript`, on Windows
PowerShell.

## Use

```bash
spectro song.flac
```

Or start `spectro` and drop a file on the window. To make an image without
a window:

```bash
spectro -o song.png --width 1600 --height 900 --scale 2 song.flac
```

`spectro --help` lists every option: stream, channel, palette, window
function, window size and dynamic range.

### Keys

On macOS use Cmd instead of Ctrl.

| Key | Action |
| --- | --- |
| `Ctrl-O` | Open a file |
| `Ctrl-S` | Save the picture as PNG |
| `Ctrl-E` | Show the settings |
| `F1` | Show the keys |
| `Shift-F1` | About |
| `c`, `C` | Change the audio channel |
| `s`, `S` | Change the audio stream |
| `p`, `P` | Change the palette |
| `f`, `F` | Change the DFT window function (Hann, Hamming, Blackman–Harris) |
| `w`, `W` | Change the DFT window size (256 to 16384) |
| `u`, `U` | Change the upper limit of the dynamic range in dBFS |
| `l`, `L` | Change the lower limit of the dynamic range in dBFS |
| `Esc` | Close a panel |

Palette, window function, window size and range are saved in
`~/.config/spectro/preferences` (or `$XDG_CONFIG_HOME`, or `%APPDATA%` on
Windows).

## Differences from Spek

- `F1` shows the keys in the window instead of opening the online manual.
- `Ctrl-E` shows the current settings. Spek's preferences dialog sets the
  language and update checks; spectro is English only and never uses the
  network.
- Extra: `--output` and the other command-line options, and the three new
  palettes.

## Layout

| Path | What |
| --- | --- |
| `crates/spectro-core` | The `no_std` engine: `math`, `fft`, `analysis`, `palette`, `font`, `canvas`, `render`, `png` |
| `src` | The program: FFmpeg input, background analysis, window, dialogs, settings, command line |
| `tools/fontgen.py` | Extracts glyph outlines from a TrueType font into `crates/spectro-core/assets` |

## Licence

GNU General Public License, version 3 or later; see [LICENSE](LICENSE).
Spek, whose design this follows, is under the same licence. The glyph
outlines are derived from Source Sans 3 under the SIL Open Font License 1.1;
see [CREDITS.md](CREDITS.md).
