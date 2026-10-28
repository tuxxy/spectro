//! Audio input through the `ffprobe` and `ffmpeg` executables.
//!
//! Running FFmpeg as a child process keeps it out of our address space: no
//! FFI, no `unsafe`, and a malformed file can at worst crash the decoder.
//! Arguments are passed directly (never through a shell) and local paths get
//! the `file:` protocol prefix, so a file name can never be read as an
//! option or a network URL.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

#[derive(Clone, Debug, Default)]
pub struct Stream {
    pub codec: String,
    pub bit_rate: u64,
    pub sample_rate: u32,
    pub bits_per_sample: u32,
    pub channels: u32,
    pub duration: f64,
}

impl Stream {
    /// Expected number of samples per channel.
    pub fn samples(&self) -> u64 {
        (self.duration * self.sample_rate as f64).round().max(0.0) as u64
    }

    /// Why this stream cannot be analysed, in Spek's words.
    pub fn problem(&self) -> Option<&'static str> {
        if self.channels == 0 || self.sample_rate == 0 {
            Some("No audio channels")
        } else if self.samples() == 0 {
            Some("Unknown duration")
        } else {
            None
        }
    }
}

pub(crate) fn input_arg(path: &Path) -> OsString {
    let mut arg = OsString::from("file:");
    arg.push(path.as_os_str());
    arg
}

pub(crate) fn tool(name: &str) -> OsString {
    let var = format!("SPECTRO_{}", name.to_ascii_uppercase());
    std::env::var_os(var).unwrap_or_else(|| OsString::from(name))
}

/// Lists the audio streams in `path`.
pub fn probe(path: &Path) -> Result<Vec<Stream>, String> {
    if !path.is_file() {
        return Err("Cannot open input file".into());
    }
    let out = Command::new(tool("ffprobe"))
        .args(["-v", "error", "-select_streams", "a", "-show_entries"])
        .arg("stream=codec_name,codec_long_name,sample_rate,channels,bit_rate,bits_per_raw_sample,bits_per_sample,duration:format=duration")
        .args(["-of", "flat=s=_"])
        .arg(input_arg(path))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Cannot run ffprobe ({e}); is FFmpeg installed?"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().last().unwrap_or("").trim();
        return Err(if line.is_empty() { "Cannot open input file".into() } else { line.to_string() });
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let streams = parse_probe(&text);
    if streams.is_empty() {
        return Err("The file contains no audio streams".into());
    }
    Ok(streams)
}

fn parse_probe(text: &str) -> Vec<Stream> {
    let mut streams: Vec<Stream> = Vec::new();
    let mut format_duration = 0.0;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim().trim_matches('"');
        let num = || value.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0).unwrap_or(0.0);
        if key == "format_duration" {
            format_duration = num();
            continue;
        }
        let Some(rest) = key.strip_prefix("streams_stream_") else { continue };
        let Some((index, field)) = rest.split_once('_') else { continue };
        let Ok(index) = index.parse::<usize>() else { continue };
        if index > 4096 {
            continue;
        }
        if streams.len() <= index {
            streams.resize_with(index + 1, Stream::default);
        }
        let s = &mut streams[index];
        match field {
            "codec_long_name" => s.codec = value.to_string(),
            "codec_name" if s.codec.is_empty() => s.codec = value.to_string(),
            "sample_rate" => s.sample_rate = num().min(u32::MAX as f64) as u32,
            "channels" => s.channels = num().min(1024.0) as u32,
            "bit_rate" => s.bit_rate = num() as u64,
            "bits_per_raw_sample" if num() > 0.0 => s.bits_per_sample = num() as u32,
            "bits_per_sample" if s.bits_per_sample == 0 => s.bits_per_sample = num() as u32,
            "duration" => s.duration = num(),
            _ => {}
        }
    }
    for s in &mut streams {
        if s.duration <= 0.0 {
            s.duration = format_duration;
        }
    }
    streams
}

/// A running decoder producing one channel of 32-bit float samples.
pub struct Decoder {
    child: Child,
    stdout: ChildStdout,
    channels: usize,
    channel: usize,
    bytes: Vec<u8>,
    pending: usize,
}

impl Decoder {
    pub fn open(path: &Path, stream: usize, channel: usize, channels: usize) -> Result<Self, String> {
        let mut child = Command::new(tool("ffmpeg"))
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(input_arg(path))
            .arg("-map")
            .arg(format!("0:a:{stream}"))
            .args(["-vn", "-sn", "-dn", "-f", "f32le", "-acodec", "pcm_f32le"])
            .arg(OsStr::new("pipe:1"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Cannot run ffmpeg ({e}); is FFmpeg installed?"))?;
        let stdout = child.stdout.take().ok_or("Cannot read from ffmpeg")?;
        Ok(Decoder {
            child,
            stdout,
            channels: channels.max(1),
            channel: channel.min(channels.max(1) - 1),
            bytes: vec![0; 1 << 16],
            pending: 0,
        })
    }

    /// Reads the next block of samples into `out`. Returns `Ok(false)` at
    /// the end of the stream.
    pub fn read(&mut self, out: &mut Vec<f32>) -> io::Result<bool> {
        out.clear();
        let frame = 4 * self.channels;
        loop {
            let n = match self.stdout.read(&mut self.bytes[self.pending..]) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if n == 0 {
                return Ok(false);
            }
            let have = self.pending + n;
            let whole = have / frame * frame;
            for f in self.bytes[..whole].chunks_exact(frame) {
                let at = 4 * self.channel;
                let v = f32::from_le_bytes([f[at], f[at + 1], f[at + 2], f[at + 3]]);
                out.push(if v.is_finite() { v } else { 0.0 });
            }
            self.bytes.copy_within(whole..have, 0);
            self.pending = have - whole;
            if !out.is_empty() {
                return Ok(true);
            }
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spek-style one-line description of a stream.
pub fn describe(streams: &[Stream], stream: usize, channel: usize, fft: usize, window: &str) -> String {
    let Some(s) = streams.get(stream) else { return String::new() };
    let mut items = Vec::new();
    if !s.codec.is_empty() {
        // "FLAC (Free Lossless Audio Codec)" reads as "FLAC".
        let short = s.codec.split(" (").next().unwrap_or(&s.codec);
        items.push(short.to_string());
    }
    if s.bit_rate > 0 {
        items.push(format!("{} kbps", (s.bit_rate + 500) / 1000));
    }
    if s.sample_rate > 0 {
        let khz = format!("{:.1}", s.sample_rate as f64 / 1000.0);
        items.push(format!("{} kHz", khz.trim_end_matches(".0")));
    }
    if s.bits_per_sample > 0 && s.bit_rate == 0 {
        items.push(format!("{}-bit", s.bits_per_sample));
    }
    match s.channels {
        0 => {}
        1 => items.push("Mono".into()),
        n => items.push(format!("Channel {} of {n}", channel + 1)),
    }
    items.push(format!("{window} \u{b7} {fft}"));
    let desc = items.join(", ");
    if streams.len() > 1 { format!("Stream {} of {}: {desc}", stream + 1, streams.len()) } else { desc }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat_output() {
        let text = "streams_stream_0_codec_name=\"flac\"\n\
                    streams_stream_0_codec_long_name=\"FLAC (Free Lossless Audio Codec)\"\n\
                    streams_stream_0_sample_rate=\"44100\"\n\
                    streams_stream_0_channels=2\n\
                    streams_stream_0_bit_rate=\"N/A\"\n\
                    streams_stream_0_bits_per_raw_sample=\"16\"\n\
                    streams_stream_0_bits_per_sample=0\n\
                    streams_stream_0_duration=\"N/A\"\n\
                    streams_stream_1_codec_long_name=\"MP3 (MPEG audio layer 3)\"\n\
                    streams_stream_1_bit_rate=\"320000\"\n\
                    format_duration=\"12.500000\"\n";
        let s = parse_probe(text);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].codec, "FLAC (Free Lossless Audio Codec)");
        assert_eq!((s[0].sample_rate, s[0].channels, s[0].bits_per_sample), (44100, 2, 16));
        assert_eq!(s[0].duration, 12.5);
        assert_eq!(s[0].samples(), 551_250);
        assert_eq!(
            describe(&s, 0, 1, 2048, "Hann"),
            "Stream 1 of 2: FLAC, 44.1 kHz, 16-bit, Channel 2 of 2, Hann \u{b7} 2048"
        );
        assert!(describe(&s, 1, 0, 2048, "Hann").contains("320 kbps"));
    }
}
