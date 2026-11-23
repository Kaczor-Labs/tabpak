//! The song's audio: a recording fetched from YouTube by yt-dlp, Songsterr's own sound, or the player's file;
//! decoded here and written as Ogg Vorbis (the format every feedpak reader plays).

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::num::{NonZeroU8, NonZeroU32};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use crate::songsterr::http_get;

/// Decoded audio, one buffer per channel.
pub struct Pcm {
    pub rate: u32,
    pub channels: Vec<Vec<f32>>,
}

impl Pcm {
    pub fn seconds(&self) -> f64 {
        self.channels.first().map(|c| c.len() as f64 / self.rate as f64).unwrap_or(0.0)
    }
}

/// Folders a Mac app started from Finder doesn't have on its PATH.
const EXTRA_DIRS: [&str; 4] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];

fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".local/bin"));
    }
    dirs.extend(EXTRA_DIRS.iter().map(PathBuf::from));
    dirs
}

pub fn find_tool(name: &str) -> Option<PathBuf> {
    search_dirs().into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

fn tool(name: &str) -> Result<Command, String> {
    let path = find_tool(name).ok_or_else(|| format!("{name} isn't installed. In Terminal: brew install {name}"))?;
    let mut cmd = Command::new(path);
    if let Ok(joined) = std::env::join_paths(search_dirs()) {
        cmd.env("PATH", joined); // yt-dlp looks for its own helpers there
    }
    Ok(cmd)
}

fn last_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("it failed without a message");
    let line = line.trim().trim_start_matches("ERROR: ");
    line.split(". ").next().unwrap_or(line).to_string()
}

/// Download a YouTube video's audio into `dir`; `progress` gets 0 to 1.
pub fn download(video_id: &str, dir: &Path, progress: &mut dyn FnMut(f32)) -> Result<PathBuf, String> {
    let mut child = tool("yt-dlp")?
        .args(["--no-warnings", "--no-playlist", "-q", "--progress", "--newline"])
        .args(["-f", "bestaudio[ext=m4a]/bestaudio", "-o"])
        .arg(dir.join(format!("{video_id}.%(ext)s")))
        .arg(format!("https://www.youtube.com/watch?v={video_id}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("yt-dlp didn't start: {e}"))?;
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(percent) = line.split('%').next().and_then(|s| s.split_whitespace().last()).and_then(|s| s.parse::<f32>().ok()) {
                progress((percent / 100.0).clamp(0.0, 1.0));
            }
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(last_line(&out.stderr));
    }
    std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.file_stem().is_some_and(|s| s == video_id))
        .ok_or_else(|| "yt-dlp finished without a file".to_string())
}

/// The built-in decoders plus Opus, which Songsterr's own sound comes in.
fn codecs() -> &'static symphonia::core::codecs::registry::CodecRegistry {
    static CODECS: OnceLock<symphonia::core::codecs::registry::CodecRegistry> = OnceLock::new();
    CODECS.get_or_init(|| {
        let mut registry = symphonia::core::codecs::registry::CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut registry);
        registry.register_audio_decoder_at_tier::<symphonia_opus::OpusDecoder>(symphonia::core::common::Tier::Fallback);
        registry
    })
}

fn decode_symphonia(source: Box<dyn symphonia::core::io::MediaSource>, ext: &str) -> Result<Pcm, String> {
    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let mss = MediaSourceStream::new(source, Default::default());
    let mut hint = Hint::new();
    if !ext.is_empty() {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| e.to_string())?;
    let track = format.default_track(TrackType::Audio).ok_or("no audio track")?.clone();
    let Some(CodecParameters::Audio(params)) = &track.codec_params else {
        return Err("no audio codec parameters".into());
    };
    let mut decoder = codecs().make_audio_decoder(params, &AudioDecoderOptions::default()).map_err(|e| e.to_string())?;
    let (mut rate, mut count, mut lead_cut) = (0u32, 0usize, 0usize);
    let (mut samples, mut chunk): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) | Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(e) if samples.is_empty() => return Err(e.to_string()),
            Err(_) => break,
        };
        if packet.track_id != track.id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                rate = buf.spec().rate();
                count = buf.spec().channels().count();
                chunk.clear();
                buf.copy_to_vec_interleaved(&mut chunk);
                // the encoder's lead-in and padding are not part of the song
                let frames = chunk.len() / count.max(1);
                let lead = (packet.trim_start.get() as usize).min(frames);
                lead_cut += lead;
                let tail = (packet.trim_end.get() as usize).min(frames - lead);
                samples.extend_from_slice(&chunk[lead * count..(frames - tail) * count]);
            }
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue, // skip a damaged packet
            Err(e) => return Err(e.to_string()),
        }
    }
    if rate == 0 || count == 0 || samples.is_empty() {
        return Err("no audio decoded".into());
    }
    // Ogg names the lead-in on the track instead of on its first packets: 6.5 ms in Songsterr's Opus
    let lead = if lead_cut == 0 { track.delay.unwrap_or(0) as usize } else { 0 };
    let frames = (samples.len() / count).saturating_sub(lead);
    let mut channels = vec![Vec::with_capacity(frames); count.min(2)];
    for frame in samples.chunks_exact(count).skip(lead) {
        for (c, buf) in channels.iter_mut().enumerate() {
            buf.push(frame[c]);
        }
    }
    Ok(Pcm { rate, channels })
}

fn decode_file(path: &Path) -> Result<Pcm, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    decode_symphonia(Box::new(file), path.extension().and_then(|e| e.to_str()).unwrap_or(""))
}

/// Decode audio held in memory; `ext` is the file ending it would have ("opus").
pub fn decode_bytes(data: Vec<u8>, ext: &str) -> Result<Pcm, String> {
    decode_symphonia(Box::new(std::io::Cursor::new(data)), ext)
}

/// Several recordings of the same length and rate played at once: Songsterr's instrument stems back into a mix.
pub fn mix(parts: &[&Pcm]) -> Option<Pcm> {
    let first = parts.first()?;
    let frames = parts.iter().map(|p| p.channels[0].len()).max()?;
    let mut channels = vec![vec![0.0f32; frames]; 2];
    for part in parts.iter().filter(|p| p.rate == first.rate) {
        for (c, out) in channels.iter_mut().enumerate() {
            let source = &part.channels[c.min(part.channels.len() - 1)];
            for (o, x) in out.iter_mut().zip(source) {
                *o += x;
            }
        }
    }
    Some(Pcm { rate: first.rate, channels })
}

impl Pcm {
    /// The loudest sample, 0 to 1 and beyond when it would clip.
    pub fn peak(&self) -> f32 {
        self.channels.iter().flatten().fold(0.0f32, |m, x| m.max(x.abs()))
    }

    pub fn scale(&mut self, gain: f32) {
        self.channels.iter_mut().flatten().for_each(|x| *x *= gain);
    }
}

/// Decode an audio file. What the built-in decoders can't read (Opus, for one) goes through ffmpeg when it's
/// installed.
pub fn decode(path: &Path) -> Result<Pcm, String> {
    let first = match decode_file(path) {
        Ok(pcm) => return Ok(pcm),
        Err(e) => e,
    };
    let Ok(mut ffmpeg) = tool("ffmpeg") else {
        return Err(format!("couldn't read the audio ({first})"));
    };
    let wav = path.with_extension("decoded.wav");
    let out = ffmpeg
        .args(["-v", "error", "-y", "-i"])
        .arg(path)
        .args(["-vn", "-c:a", "pcm_s16le"])
        .arg(&wav)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("couldn't read the audio ({})", last_line(&out.stderr)));
    }
    let pcm = decode_file(&wav);
    let _ = std::fs::remove_file(&wav);
    pcm
}

/// Ogg Vorbis at the encoder's default quality (about 160 kbit/s for stereo).
pub fn encode_ogg(pcm: &Pcm) -> Result<Vec<u8>, String> {
    let rate = NonZeroU32::new(pcm.rate).ok_or("no sample rate")?;
    let count = NonZeroU8::new(pcm.channels.len() as u8).ok_or("no channels")?;
    let mut out = Vec::new();
    let mut encoder = vorbis_rs::VorbisEncoderBuilder::new(rate, count, &mut out)
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    let frames = pcm.channels[0].len();
    let block = 1 << 16;
    let mut at = 0;
    while at < frames {
        let end = (at + block).min(frames);
        let slices: Vec<&[f32]> = pcm.channels.iter().map(|c| &c[at..end]).collect();
        encoder.encode_audio_block(&slices).map_err(|e| e.to_string())?;
        at = end;
    }
    encoder.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// The video's thumbnail cut to a square JPEG; a song without a cover is fine.
pub fn cover(video_id: &str) -> Option<Vec<u8>> {
    let raw = http_get(&format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg")).ok()?;
    square_jpeg(&raw, 600)
}

pub fn square_jpeg(raw: &[u8], side: u32) -> Option<Vec<u8>> {
    let img = image::load_from_memory(raw).ok()?;
    let edge = img.width().min(img.height());
    let cut = img.crop_imm((img.width() - edge) / 2, (img.height() - edge) / 2, edge, edge);
    let sized = cut.resize_exact(side, side, image::imageops::FilterType::Lanczos3).to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88).encode_image(&sized).ok()?;
    Some(out)
}

/// Read a whole file that must fit in memory (a cover picture, a downloaded tab).
pub fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    File::open(path).and_then(|mut f| f.read_to_end(&mut data)).map_err(|e| e.to_string())?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_square_cover_from_a_wide_thumbnail() {
        let wide = image::RgbImage::from_pixel(48, 36, image::Rgb([200, 40, 40]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(wide).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let jpeg = square_jpeg(&png, 32).unwrap();
        let back = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((back.width(), back.height()), (32, 32));
    }
}
