//! Thin wrappers around ffprobe/ImageMagick used to inspect sources.
//!
//! ffprobe omits `bit_rate` for some streams (notably a number of PCM/WAV
//! variants). For audio streams we therefore derive the bitrate from
//! sample-rate × channels × bit-depth when the field is unavailable.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn which(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

pub fn ffprobe_binary() -> Option<PathBuf> {
    which("ffprobe")
}

fn probe_json(path: &Path) -> Option<Value> {
    let ffprobe = ffprobe_binary()?;
    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

fn streams(value: &Value) -> &[Value] {
    value
        .get("streams")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn positive(value: Option<&Value>) -> Option<f64> {
    number(value).filter(|value| *value > 0.0)
}

fn codec_bits(codec: &str) -> Option<f64> {
    let bits = match codec {
        "pcm_u8" | "pcm_s8" | "pcm_s8_planar" => 8.0,
        "pcm_s16le" | "pcm_s16be" | "pcm_s16le_planar" => 16.0,
        "pcm_s24le" | "pcm_s24be" | "pcm_s24daud" | "pcm_s24le_planar" => 24.0,
        "pcm_s32le" | "pcm_s32be" | "pcm_s32le_planar" => 32.0,
        "pcm_f32le" | "pcm_f32be" => 32.0,
        "pcm_f64le" | "pcm_f64be" => 64.0,
        _ => return None,
    };
    Some(bits)
}

fn sample_format_bits(sample_fmt: &str) -> Option<f64> {
    if sample_fmt.starts_with("u8") || sample_fmt.starts_with("s8") {
        Some(8.0)
    } else if sample_fmt.starts_with("s16") {
        Some(16.0)
    } else if sample_fmt.starts_with("s32") || sample_fmt.starts_with("flt") {
        Some(32.0)
    } else if sample_fmt.starts_with("dbl") {
        Some(64.0)
    } else {
        None
    }
}

/// Bitrate of one stream, falling back to sample rate × channels × depth.
fn stream_bit_rate(stream: &Value) -> Option<f64> {
    if let Some(rate) = positive(stream.get("bit_rate")) {
        return Some(rate);
    }
    let sample_rate = positive(stream.get("sample_rate"))?;
    let channels = positive(stream.get("channels"))?;
    let bits = positive(stream.get("bits_per_sample"))
        .or_else(|| positive(stream.get("bits_per_raw_sample")))
        .or_else(|| {
            stream
                .get("codec_name")
                .and_then(Value::as_str)
                .and_then(codec_bits)
        })
        .or_else(|| {
            stream
                .get("sample_fmt")
                .and_then(Value::as_str)
                .and_then(sample_format_bits)
        })?;
    let rate = sample_rate * channels * bits;
    (rate > 0.0).then_some(rate)
}

fn container_bit_rate(value: &Value) -> Option<f64> {
    positive(
        value
            .get("format")
            .and_then(|format| format.get("bit_rate")),
    )
}

pub fn probe_streams(path: &Path) -> (Option<bool>, Option<bool>) {
    let Some(value) = probe_json(path) else {
        return (None, None);
    };
    let has_video = streams(&value)
        .iter()
        .any(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"));
    let has_audio = streams(&value)
        .iter()
        .any(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("audio"));
    (Some(has_video), Some(has_audio))
}

/// Container duration in milliseconds, when ffprobe can tell.
pub fn duration_ms(path: &Path) -> Option<f64> {
    let value = probe_json(path)?;
    number(
        value
            .get("format")
            .and_then(|format| format.get("duration")),
    )
    .map(|seconds| seconds * 1000.0)
    .or_else(|| {
        streams(&value)
            .iter()
            .find_map(|stream| number(stream.get("duration")))
            .map(|seconds| seconds * 1000.0)
    })
}

/// Audio-stream bitrate in bits/second (container as fallback), or `None`.
pub fn audio_bitrate_bps(path: &Path) -> Option<f64> {
    let value = probe_json(path)?;
    let has_video = streams(&value)
        .iter()
        .any(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"));

    for stream in streams(&value) {
        if stream.get("codec_type").and_then(Value::as_str) != Some("audio") {
            continue;
        }
        if let Some(rate) = stream_bit_rate(stream) {
            return Some(rate);
        }
    }

    // A container bitrate for a video file includes the video stream, so it is
    // not a meaningful audio bitrate. For audio-only files it is a good
    // fallback when the stream itself did not expose one.
    if has_video {
        None
    } else {
        container_bit_rate(&value)
    }
}

/// Video-stream bitrate in bits/second (container as fallback), or `None`.
pub fn video_bitrate_bps(path: &Path) -> Option<f64> {
    let value = probe_json(path)?;
    for stream in streams(&value) {
        if stream.get("codec_type").and_then(Value::as_str) != Some("video") {
            continue;
        }
        let attached = stream
            .get("disposition")
            .and_then(|disposition| disposition.get("attached_pic"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if attached == 0 {
            if let Some(rate) = stream_bit_rate(stream) {
                return Some(rate);
            }
        }
    }
    container_bit_rate(&value)
}

/// Index of the album-cover (attached picture) video stream, or `None`.
pub fn attached_picture(path: &Path) -> Option<u32> {
    let value = probe_json(path)?;
    for stream in streams(&value) {
        if stream.get("codec_type").and_then(Value::as_str) != Some("video") {
            continue;
        }
        let attached = stream
            .get("disposition")
            .and_then(|disposition| disposition.get("attached_pic"))
            .map(|flag| match flag {
                Value::Bool(flag) => *flag,
                Value::Number(number) => number.as_i64().unwrap_or(0) != 0,
                _ => false,
            })
            .unwrap_or(false);
        if attached {
            return stream
                .get("index")
                .and_then(Value::as_u64)
                .map(|index| index as u32);
        }
    }
    None
}

/// Encoder quality (1-100) reported by ImageMagick for lossy sources.
pub fn image_quality(path: &Path) -> Option<u32> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "jpg" | "jpeg" | "jfif" | "webp" | "avif") {
        return None;
    }
    let magick = which("magick")?;
    let output = Command::new(magick)
        .args(["identify", "-format", "%Q"])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let digits: String = String::from_utf8_lossy(&output.stdout)
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse::<u32>().ok().map(|value| value.clamp(1, 100))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn derives_pcm_wav_bitrate_when_ffprobe_omits_it() {
        // This is what some WAV/PCM streams look like: no bit_rate, but all
        // of the fields needed to calculate it are present.
        let stream = json!({
            "codec_type": "audio",
            "codec_name": "pcm_s16le",
            "sample_rate": "44100",
            "channels": 2,
            "bits_per_sample": 16
        });
        assert_eq!(stream_bit_rate(&stream), Some(1_411_200.0));
    }

    #[test]
    fn derives_bitrate_from_codec_name() {
        let stream = json!({
            "codec_type": "audio",
            "codec_name": "pcm_s24le",
            "sample_rate": "48000",
            "channels": 2
        });
        assert_eq!(stream_bit_rate(&stream), Some(2_304_000.0));
    }

    #[test]
    fn prefers_ffprobe_bitrate() {
        let stream = json!({
            "codec_type": "audio",
            "bit_rate": "192000",
            "sample_rate": "44100",
            "channels": 2,
            "bits_per_sample": 16
        });
        assert_eq!(stream_bit_rate(&stream), Some(192_000.0));
    }
}
