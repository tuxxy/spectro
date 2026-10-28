//! spectro: an acoustic spectrum analyser.
//!
//! The engine lives in the `no_std` crate `spectro-core`; this binary adds
//! audio input through FFmpeg, a window, and a command line.

#![forbid(unsafe_code)]

mod dialogs;
mod gui;
mod media;
mod pipeline;
mod settings;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use spectro_core::analysis::{Spectrogram, bands};
use spectro_core::canvas::Canvas;
use spectro_core::render::{Renderer, View, plot_rect};

use media::Stream;
use settings::Settings;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
Usage: spectro [OPTIONS] [FILE]

Shows the spectrogram of an audio file. With --output, renders it to a PNG
image instead of opening a window.

Options:
  -o, --output FILE           write a PNG image and exit
      --width PIXELS          image width for --output (default 1280)
      --height PIXELS         image height for --output (default 720)
      --scale FACTOR          layout scale for --output, e.g. 2 for HiDPI (default 1)
  -s, --stream N              audio stream, starting at 1
  -c, --channel N             audio channel, starting at 1
  -p, --palette NAME          ember, spectrum, sox, mono, glacier or aurora
  -f, --window-function NAME  hann, hamming or blackman-harris
  -w, --window-size N         DFT size, a power of two from 256 to 16384
  -u, --upper DB              upper limit of the dynamic range in dBFS
  -l, --lower DB              lower limit of the dynamic range in dBFS
      --glow on|off           let loud components bloom (default on)
      --freq-scale NAME       linear (default), mel or log
  -h, --help                  show this help and exit
  -V, --version               show the version and exit

Keys: Ctrl-O open, Ctrl-S save, Ctrl-E settings, F1 help (Cmd on macOS).
  c/C channel  s/S stream  p/P palette  f/F window function
  w/W window size  u/U upper limit  l/L lower limit
  y/Y frequency scale  g glow

Settings are kept in ~/.config/spectro/preferences. FFmpeg (ffmpeg and
ffprobe on the PATH, or SPECTRO_FFMPEG / SPECTRO_FFPROBE) decodes the audio.
";

struct Args {
    file: Option<PathBuf>,
    output: Option<PathBuf>,
    width: usize,
    height: usize,
    scale: f32,
    stream: usize,
    channel: usize,
    settings: Vec<(&'static str, String)>,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        file: None,
        output: None,
        width: 1280,
        height: 720,
        scale: 1.0,
        stream: 0,
        channel: 0,
        settings: Vec::new(),
    };
    let mut it = std::env::args_os().skip(1);
    let mut only_files = false;
    while let Some(raw) = it.next() {
        let text = raw.to_string_lossy().into_owned();
        if only_files || !text.starts_with('-') || text == "-" {
            if args.file.replace(PathBuf::from(raw)).is_some() {
                return Err("only one input file can be given".into());
            }
            continue;
        }
        if text == "--" {
            only_files = true;
            continue;
        }
        let (name, inline) = match text.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_string(), Some(v.to_string())),
            _ => (text.clone(), None),
        };
        let mut value = || -> Result<String, String> {
            inline
                .clone()
                .or_else(|| it.next().map(|v| v.to_string_lossy().into_owned()))
                .ok_or_else(|| format!("{name} needs a value"))
        };
        let number = |v: String| v.parse::<usize>().map_err(|_| format!("{name}: not a number: {v}"));
        match name.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("spectro {VERSION}");
                return Ok(None);
            }
            "-o" | "--output" => args.output = Some(PathBuf::from(value()?)),
            "--width" => args.width = number(value()?)?.clamp(200, 16384),
            "--height" => args.height = number(value()?)?.clamp(150, 16384),
            "--scale" => {
                let v = value()?;
                args.scale =
                    v.parse::<f32>().ok().filter(|s| (0.5..=8.0).contains(s)).ok_or(format!("bad scale: {v}"))?;
            }
            "-s" | "--stream" => args.stream = number(value()?)?.saturating_sub(1),
            "-c" | "--channel" => args.channel = number(value()?)?.saturating_sub(1),
            "-p" | "--palette" => args.settings.push(("palette", value()?)),
            "-f" | "--window-function" => args.settings.push(("window_function", value()?)),
            "-w" | "--window-size" => args.settings.push(("window_size", value()?)),
            "-u" | "--upper" => args.settings.push(("upper_db", value()?)),
            "-l" | "--lower" => args.settings.push(("lower_db", value()?)),
            "--glow" => args.settings.push(("glow", value()?)),
            "--freq-scale" => args.settings.push(("freq_scale", value()?)),
            _ => return Err(format!("unknown option {name}")),
        }
    }
    Ok(Some(args))
}

/// Builds the view of a file; shared by the window and `--output`.
pub fn view<'a>(
    title: &'a str,
    description: &'a str,
    stream: Option<&Stream>,
    spectrogram: Option<&'a Spectrogram>,
    settings: &Settings,
    scale: f32,
) -> View<'a> {
    View {
        spectrogram,
        title,
        description,
        duration: stream.map_or(0.0, |s| s.duration),
        sample_rate: stream.map_or(0, |s| s.sample_rate),
        palette: settings.palette,
        upper_db: settings.upper_db,
        lower_db: settings.lower_db,
        scale,
        message: None,
        panel: None,
        version: VERSION,
        glow: settings.glow,
        freq_scale: settings.freq_scale,
        cursor: None,
    }
}

fn render_to_file(args: &Args, settings: &Settings, output: &Path) -> Result<(), String> {
    let file = args.file.as_deref().ok_or("--output needs an input file")?;
    let streams = media::probe(file)?;
    let s = streams.get(args.stream).ok_or(format!("there is no stream {}", args.stream + 1))?;
    if let Some(p) = s.problem() {
        return Err(p.into());
    }
    if args.channel >= s.channels as usize {
        return Err(format!("there is no channel {}", args.channel + 1));
    }
    let plot = plot_rect(args.width, args.height, args.scale);
    let req = pipeline::Request {
        path: file.to_path_buf(),
        stream: args.stream,
        channel: args.channel,
        channels: s.channels as usize,
        samples: s.samples(),
        fft_bits: settings.fft_bits,
        window: settings.window,
        columns: plot.w as usize,
    };
    let mut spec = Spectrogram::new(req.columns, bands(req.fft_bits));
    pipeline::run(&req, &AtomicBool::new(false), &mut |c| spec.set(c))?;
    let title = file.display().to_string();
    let desc = media::describe(&streams, args.stream, args.channel, 1 << settings.fft_bits, settings.window.name());
    let v = view(&title, &desc, Some(s), Some(&spec), settings, args.scale);
    let mut canvas = Canvas::new(args.width, args.height, 0);
    Renderer::new().render(&mut canvas, &v);
    let png = spectro_core::png::encode(canvas.width(), canvas.height(), canvas.pixels());
    std::fs::write(output, png).map_err(|e| format!("cannot write {}: {e}", output.display()))
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(a)) => a,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("spectro: {e}\nTry 'spectro --help' for more information.");
            return ExitCode::from(2);
        }
    };
    let mut settings = if args.output.is_some() { Settings::default() } else { Settings::load() };
    for (k, v) in &args.settings {
        if !settings.set(k, v) {
            eprintln!("spectro: invalid {k}: {v}");
            return ExitCode::from(2);
        }
    }
    let result = match &args.output {
        Some(out) => render_to_file(&args, &settings, out),
        None => gui::run(args.file, settings, args.stream, args.channel),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("spectro: {e}");
            ExitCode::FAILURE
        }
    }
}
