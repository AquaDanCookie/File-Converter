//! Conversion engine.
//!
//! Audio/video -> ffmpeg, images -> ImageMagick, office documents ->
//! LibreOffice, image -> PDF -> img2pdf with an ImageMagick fallback.

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::presets::Options;
use crate::{matrix, probe};

#[derive(Debug)]
pub enum ConvError {
    Cancelled,
    Message(String),
}

impl std::fmt::Display for ConvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => write!(f, "cancelled"),
            Self::Message(message) => write!(f, "{message}"),
        }
    }
}

impl From<String> for ConvError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for ConvError {
    fn from(message: &str) -> Self {
        Self::Message(message.to_string())
    }
}

impl From<std::io::Error> for ConvError {
    fn from(error: std::io::Error) -> Self {
        Self::Message(error.to_string())
    }
}

pub type ConvResult<T> = Result<T, ConvError>;
pub type ProgressFn = dyn Fn(f64, String) + Send + Sync;

pub const AUDIO_BITRATE_OPTIONS: &[u32] = &[128, 192, 256, 320];
pub const AUDIO_SAMPLE_RATES: &[u32] =
    &[8000, 11025, 16000, 22050, 32000, 44100, 48000, 88200, 96000];
pub const VIDEO_PRESETS: &[&str] = &[
    "ultrafast",
    "superfast",
    "veryfast",
    "faster",
    "fast",
    "medium",
    "slow",
    "slower",
    "veryslow",
];

/// Extra room reserved for container headers, tags, and encoder rounding.
const SIZE_HEADROOM: f64 = 0.92;

const DEPENDENCIES: &[(&str, &[&str])] = &[
    ("ffmpeg", &["ffmpeg", "ffprobe"]),
    ("imagemagick", &["magick"]),
    ("libreoffice", &["soffice", "libreoffice"]),
    ("img2pdf", &["img2pdf"]),
];

pub const PACMAN_HINT: &str = "sudo pacman -S --needed ffmpeg imagemagick libreoffice img2pdf";

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn binary(name: &str) -> Option<PathBuf> {
    if name == "soffice" {
        which("soffice").or_else(|| which("libreoffice"))
    } else {
        which(name)
    }
}

pub fn required_packages(category: &str, target: &str, source: &Path) -> Vec<String> {
    let ext = matrix::extension_of(source).unwrap_or_default();
    match category {
        "Audio" | "Video" => vec!["ffmpeg".into()],
        "Document" => {
            if target == "pdf" {
                vec!["libreoffice".into()]
            } else {
                vec!["libreoffice".into(), "imagemagick".into()]
            }
        }
        "Image" => {
            if matrix::OFFICE_EXTS.contains(&ext.as_str()) {
                vec!["libreoffice".into(), "imagemagick".into()]
            } else if target == "pdf" {
                if binary("img2pdf").is_some() || binary("magick").is_some() {
                    vec![]
                } else {
                    vec!["img2pdf".into(), "imagemagick".into()]
                }
            } else {
                vec!["imagemagick".into()]
            }
        }
        _ => vec![],
    }
}

pub fn missing_packages(packages: &[String]) -> Vec<String> {
    let mut unique: Vec<&String> = packages.iter().collect();
    unique.sort();
    unique.dedup();
    unique
        .into_iter()
        .filter(|pkg| {
            let binaries = DEPENDENCIES
                .iter()
                .find(|(name, _)| *name == pkg.as_str())
                .map(|(_, binaries)| *binaries)
                .unwrap_or(&[]);
            if binaries.is_empty() {
                binary(pkg.as_str()).is_none()
            } else {
                !binaries.iter().any(|name| binary(name).is_some())
            }
        })
        .cloned()
        .collect()
}

pub fn human_size(mut num: f64) -> String {
    for unit in ["B", "KiB", "MiB", "GiB"] {
        if num.abs() < 1024.0 || unit == "GiB" {
            if unit == "B" {
                return format!("{num:.0} {unit}");
            }
            return format!("{num:.1} {unit}");
        }
        num /= 1024.0;
    }
    format!("{num:.1} GiB")
}

fn generic_info(path: &Path) -> String {
    let mut lines = Vec::new();
    if let Ok(metadata) = path.metadata() {
        lines.push(format!(
            "{}  ({})",
            path.file_name().unwrap_or_default().to_string_lossy(),
            human_size(metadata.len() as f64)
        ));
    } else {
        lines.push(
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
        );
    }
    if let Some(file) = which("file") {
        if let Ok(output) = Command::new(file).arg("-b").arg(path).output() {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !text.is_empty() {
                lines.push(text);
            }
        }
    }
    if lines.len() == 1 {
        lines.push("No detailed media info available (install 'mediainfo').".into());
    }
    lines.join("\n")
}

/// Human-readable media information for the GUI.
pub fn media_info_text(path: &Path) -> String {
    if !path.exists() {
        return format!(
            "{}: file not found",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
    }

    if let Some(mediainfo) = which("mediainfo") {
        if let Ok(output) = Command::new(mediainfo)
            .args(["--Output=Text"])
            .arg(path)
            .output()
        {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !text.is_empty() {
                    return text;
                }
            }
        }
    }

    let category = matrix::detect_category(path).unwrap_or("");
    if matches!(category, "Audio" | "Video") {
        if let Some(ffprobe) = which("ffprobe") {
            if let Ok(output) = Command::new(ffprobe)
                .args(["-hide_banner"])
                .arg(path)
                .output()
            {
                if output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    let body = if stderr.is_empty() { stdout } else { stderr };
                    if !body.is_empty() {
                        return body;
                    }
                }
            }
        }
    } else if category == "Image" {
        if let Some(magick) = which("magick") {
            if let Ok(output) = Command::new(magick)
                .args(["identify", "-verbose"])
                .arg(path)
                .output()
            {
                if output.status.success() {
                    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !text.is_empty() {
                        return text;
                    }
                }
            }
        }
    }

    generic_info(path)
}

#[derive(Clone, Debug)]
pub struct Job {
    pub source: PathBuf,
    pub target: String,
    pub category: String,
    pub options: Options,
    pub outdir: Option<PathBuf>,
}

impl Job {
    pub fn output_path(&self) -> PathBuf {
        let directory = self
            .outdir
            .clone()
            .unwrap_or_else(|| self.source.parent().unwrap_or(Path::new(".")).to_path_buf());
        let stem = self
            .source
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        unique_path(&directory.join(format!("{stem}.{}", self.target)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Skipped,
    Error,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct ConversionResult {
    pub job: Job,
    pub status: Status,
    pub output: Option<PathBuf>,
    pub message: String,
}

fn unique_path(base: &Path) -> PathBuf {
    if !base.exists() {
        return base.to_path_buf();
    }
    let stem = base.file_stem().unwrap_or_default().to_string_lossy();
    let suffix = base
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    for index in 1..10_000 {
        let candidate = base.with_file_name(format!("{stem}.{index}{suffix}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    base.to_path_buf()
}

fn run_process(
    args: &[String],
    progress: Option<&(dyn Fn(String) + Send + Sync)>,
    cancel: &Arc<AtomicBool>,
    timeout: Option<Duration>,
) -> ConvResult<String> {
    let mut command = Command::new(&args[0]);
    command
        .args(&args[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| ConvError::Message(format!("missing tool: {} ({error})", args[0])))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let err_lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let started = Instant::now();

    let mut cancelled = false;
    let mut timed_out = false;
    let mut exit_code: Option<i32> = None;

    thread::scope(|scope| {
        if let Some(stdout) = stdout {
            scope.spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Some(feed) = progress {
                        feed(line);
                    }
                }
            });
        }
        if let Some(stderr) = stderr {
            let lines = Arc::clone(&err_lines);
            scope.spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    lines.lock().unwrap().push(line);
                }
            });
        }

        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exit_code = status.code();
                    break;
                }
                Ok(None) => {}
                Err(_) => break,
            }
            if cancel.load(Ordering::SeqCst) {
                cancelled = true;
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            if let Some(limit) = timeout {
                if started.elapsed() > limit {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
    });

    if cancelled {
        return Err(ConvError::Cancelled);
    }
    if timed_out {
        return Err(ConvError::Message(format!(
            "{} timed out",
            Path::new(&args[0])
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        )));
    }
    let code = exit_code.unwrap_or(-1);
    if code != 0 {
        let lines = err_lines.lock().unwrap();
        let detail = lines
            .iter()
            .rev()
            .take(12)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(ConvError::Message(if detail.trim().is_empty() {
            format!(
                "{} exited with code {code}",
                Path::new(&args[0])
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            )
        } else {
            detail
        }));
    }
    let captured = err_lines.lock().unwrap().join("\n");
    Ok(captured)
}

// -------------------------------------------------------------------------
// Auto quality
// -------------------------------------------------------------------------

/// Choose the offered bitrate closest to the source's own bitrate.
///
/// Returns `None` when the source bitrate cannot be determined. PCM/WAV
/// sources are handled by `probe` deriving the rate from the stream fields.
pub fn auto_bitrate_kbps(source: &Path) -> Option<u32> {
    let bps = probe::audio_bitrate_bps(source)?;
    let kbps = bps / 1000.0;
    nearest_option(kbps, AUDIO_BITRATE_OPTIONS)
}

fn nearest_option(value: f64, options: &[u32]) -> Option<u32> {
    options.iter().copied().min_by(|a, b| {
        let da = (*a as f64 - value).abs();
        let db = (*b as f64 - value).abs();
        da.partial_cmp(&db)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.cmp(a))
    })
}

pub fn effective_bitrate(source: &Path, options: &Options) -> u32 {
    options
        .bitrate
        .or_else(|| auto_bitrate_kbps(source))
        .unwrap_or(192)
}

/// Map the source's video bitrate onto a CRF (lower = better quality).
pub fn auto_crf(source: &Path) -> u32 {
    let Some(bps) = probe::video_bitrate_bps(source) else {
        return 23;
    };
    let mbps = bps / 1_000_000.0;
    if mbps >= 8.0 {
        18
    } else if mbps >= 5.0 {
        19
    } else if mbps >= 3.0 {
        20
    } else if mbps >= 2.0 {
        21
    } else if mbps >= 1.0 {
        22
    } else if mbps >= 0.5 {
        23
    } else if mbps >= 0.3 {
        25
    } else {
        28
    }
}

pub fn effective_crf(source: &Path, options: &Options) -> u32 {
    options.crf.unwrap_or_else(|| auto_crf(source))
}

/// Use the source's own quality; lossless/unknown sources want max quality.
pub fn auto_quality(source: &Path) -> u32 {
    match probe::image_quality(source) {
        Some(value) => value.clamp(60, 95),
        None => 95,
    }
}

pub fn effective_quality(source: &Path, options: &Options) -> u32 {
    options.quality.unwrap_or_else(|| auto_quality(source))
}

/// Format a number without trailing zeros, e.g. 20 -> "20", 0.5 -> "0.5".
pub fn format_number(value: f64) -> String {
    if value.fract().abs() < 1e-9 {
        format!("{value:.0}")
    } else {
        let mut text = format!("{value:.6}");
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
        text
    }
}

/// Short human-readable quality annotation for a finished/scheduled job.
pub fn quality_note(job: &Job) -> String {
    let options = &job.options;
    if let Some(max_size) = options.max_size {
        return format!(" [≤ {} MB]", format_number(max_size));
    }
    match options_category(&job.category, Some(&job.target)).as_str() {
        "audio" => {
            let mut bitrate = effective_bitrate(&job.source, options);
            if job.target == "opus" {
                bitrate = bitrate.min(256);
            }
            format!(" [{bitrate} kbps]")
        }
        "video" => {
            if let Some(vbitrate) = options.vbitrate {
                format!(" [constant {vbitrate} kbps]")
            } else {
                format!(
                    " [crf {}, {}]",
                    effective_crf(&job.source, options),
                    options.preset.as_deref().unwrap_or("medium")
                )
            }
        }
        "image" => format!(" [{} quality]", effective_quality(&job.source, options)),
        _ => String::new(),
    }
}

fn options_category(category: &str, target: Option<&str>) -> String {
    let Some(target) = target else {
        return category.to_ascii_lowercase();
    };
    match matrix::output_category(target) {
        Some("Document") | None => category.to_ascii_lowercase(),
        Some(other) => other.to_ascii_lowercase(),
    }
}

/// Resolve preset defaults into option values. `None` fields stay auto and
/// are resolved against the individual source when the job runs.
pub fn quality_options(
    _source: &Path,
    category: &str,
    defaults: &Options,
    target: Option<&str>,
) -> Options {
    match options_category(category, target).as_str() {
        "audio" => Options {
            bitrate: defaults.bitrate,
            sample_rate: defaults.sample_rate,
            max_size: defaults.max_size,
            ..Options::default()
        },
        "video" => Options {
            vbitrate: defaults.vbitrate,
            crf: defaults.crf,
            preset: Some(defaults.preset.clone().unwrap_or_else(|| "medium".into())),
            max_size: defaults.max_size,
            ..Options::default()
        },
        "image" => Options {
            scale: Some(defaults.scale.unwrap_or(100)),
            quality: defaults.quality,
            max_size: defaults.max_size,
            ..Options::default()
        },
        _ => Options {
            max_size: defaults.max_size,
            ..Options::default()
        },
    }
}

// -------------------------------------------------------------------------
// ffmpeg helpers
// -------------------------------------------------------------------------

fn audio_codec(target: &str) -> Option<(&'static str, bool)> {
    match target {
        "mp3" => Some(("libmp3lame", true)),
        "aac" => Some(("aac", true)),
        "ogg" => Some(("libvorbis", true)),
        "m4a" => Some(("aac", true)),
        "opus" => Some(("libopus", true)),
        "flac" => Some(("flac", false)),
        "wav" => Some(("pcm_s16le", false)),
        _ => None,
    }
}

fn cover_capable(target: &str) -> bool {
    matches!(target, "mp3" | "m4a" | "flac")
}

fn quality_from_crf(crf: u32) -> u32 {
    ((crf as f64 / 51.0 * 8.0).round() as u32 + 2).clamp(2, 10)
}

fn parse_timestamp_ms(raw: &str) -> Option<f64> {
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() == 3 {
        let hours: f64 = parts[0].parse().ok()?;
        let minutes: f64 = parts[1].parse().ok()?;
        let seconds: f64 = parts[2].parse().ok()?;
        return Some((hours * 3600.0 + minutes * 60.0 + seconds) * 1000.0);
    }
    None
}

fn ffmpeg_progress_feed<'a>(
    progress: &'a ProgressFn,
    name: &str,
    duration: Option<f64>,
) -> impl Fn(String) + Send + Sync + 'a {
    let name = name.to_string();
    let last = Arc::new(Mutex::new(0.0f64));
    move |line| {
        let value = if let Some(rest) = line.strip_prefix("out_time_us=") {
            rest.trim().parse::<f64>().ok().map(|us| us / 1000.0)
        } else if let Some(rest) = line.strip_prefix("out_time_ms=") {
            // Despite the name, ffmpeg reports microseconds here.
            rest.trim().parse::<f64>().ok().map(|us| us / 1000.0)
        } else if let Some(rest) = line.strip_prefix("out_time=") {
            parse_timestamp_ms(rest.trim())
        } else {
            None
        };
        let Some(current) = value else {
            return;
        };
        if let Some(duration) = duration.filter(|duration| *duration > 0.0) {
            let pct = (current / duration * 100.0).clamp(0.0, 100.0);
            let mut last = last.lock().unwrap();
            if pct - *last >= 1.0 || pct >= 100.0 {
                *last = pct;
                progress(pct, format!("{name}: {pct:.0}%"));
            }
        }
    }
}

fn libopus_rate(rate: u32) -> u32 {
    let rates = [8000, 12000, 16000, 24000, 48000];
    rates
        .into_iter()
        .min_by_key(|candidate| ((*candidate as i64 - rate as i64).abs(), *candidate))
        .unwrap_or(48000)
}

/// Total bitrate that fits `max_size_mb` for the source duration, in kbps.
fn size_budget_kbps(source: &Path, max_size_mb: f64) -> Option<f64> {
    if max_size_mb <= 0.0 {
        return None;
    }
    let duration = probe::duration_ms(source)?;
    if duration <= 0.0 {
        return None;
    }
    Some(max_size_mb * 1_000_000.0 * 8.0 / (duration / 1000.0) / 1000.0)
}

fn pick_bitrate(budget_kbps: f64, rates: &[u32]) -> Option<u32> {
    rates
        .iter()
        .copied()
        .filter(|rate| (*rate as f64) <= budget_kbps)
        .max()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AudioSizePlan {
    bitrate_kbps: Option<u32>,
    sample_rate: Option<u32>,
}

fn low_sample_rate(options: &Options) -> u32 {
    options
        .sample_rate
        .filter(|rate| *rate <= 24_000)
        .unwrap_or(22_050)
}

/// Pick a lossy audio bitrate (and a lower sample rate when MP3 needs one)
/// that leaves room for the container. Lossless formats are left at normal
/// quality and checked after encoding.
fn audio_size_plan(
    source: &Path,
    target: &str,
    options: &Options,
) -> ConvResult<Option<AudioSizePlan>> {
    let Some(max_size) = options.max_size.filter(|size| *size > 0.0) else {
        return Ok(None);
    };
    let (_, lossy) = audio_codec(target)
        .ok_or_else(|| ConvError::Message(format!("unsupported audio target: {target}")))?;
    if !lossy {
        // Lossless output is encoded normally and checked after the fact. A
        // small FLAC/WAV may already be below the limit; a large one cannot be
        // shrunk by lowering a bitrate, so it is rejected later.
        return Ok(None);
    }
    let Some(raw_budget) = size_budget_kbps(source, max_size) else {
        return Err(ConvError::Message(
            "cannot enforce Max File Size: source duration is unknown".into(),
        ));
    };
    let budget = raw_budget * SIZE_HEADROOM;

    let (bitrate, sample_rate) = match target {
        "mp3" => {
            if budget < 32.0 {
                // MPEG-1 Layer III does not allow bitrates below 32 kbps at
                // 44.1/48 kHz. MPEG-2 at 22.05 kHz does, and is still plenty
                // for long, low-bitrate files.
                (
                    pick_bitrate(
                        budget,
                        &[8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160],
                    ),
                    Some(low_sample_rate(options)),
                )
            } else {
                (
                    pick_bitrate(
                        budget,
                        &[
                            32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
                        ],
                    ),
                    options.sample_rate,
                )
            }
        }
        "opus" => (
            Some(budget.floor().clamp(6.0, 256.0) as u32),
            options.sample_rate,
        ),
        "aac" | "m4a" => {
            if budget < 32.0 {
                // Native AAC also refuses to go much below ~20 kbps at 44.1
                // kHz. Drop to 22.05 kHz so 16 kbps actually sticks.
                (
                    pick_bitrate(
                        budget,
                        &[
                            8, 16, 24, 32, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
                        ],
                    ),
                    Some(low_sample_rate(options)),
                )
            } else {
                (
                    pick_bitrate(
                        budget,
                        &[
                            8, 16, 24, 32, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
                        ],
                    ),
                    options.sample_rate,
                )
            }
        }
        "ogg" => {
            if budget < 48.0 {
                // libvorbis rejects very low bitrates at 44.1 kHz. Lower the
                // sample rate to what the requested size can afford.
                let (sample_rate, minimum) = if budget < 16.0 {
                    (8000, 16)
                } else if budget < 24.0 {
                    (11025, 16)
                } else if budget < 32.0 {
                    (16000, 24)
                } else {
                    (22050, 32)
                };
                let rate = pick_bitrate(
                    budget,
                    &[
                        minimum, 24, 32, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
                    ],
                );
                (
                    rate,
                    Some(
                        options
                            .sample_rate
                            .filter(|rate| *rate <= 22_050)
                            .unwrap_or(sample_rate),
                    ),
                )
            } else {
                (
                    pick_bitrate(budget, &[48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320]),
                    options.sample_rate,
                )
            }
        }
        _ => (None, options.sample_rate),
    };

    let Some(bitrate) = bitrate else {
        return Err(ConvError::Message(format!(
            "cannot fit under {} MB: the source is too long for the lowest {} bitrate",
            format_number(max_size),
            target.to_ascii_uppercase()
        )));
    };
    Ok(Some(AudioSizePlan {
        bitrate_kbps: Some(bitrate),
        sample_rate,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VideoRatePlan {
    video_kbps: Option<u32>,
    audio_kbps: u32,
}

/// Divide a video budget between video and audio. The share given to audio is
/// deliberately small; the video stream is what the viewer notices most.
fn video_rate_plan(source: &Path, target: &str, options: &Options) -> ConvResult<VideoRatePlan> {
    let Some(max_size) = options.max_size.filter(|size| *size > 0.0) else {
        let audio_kbps = match target {
            "webm" => 64,
            "ogv" => 128,
            _ => 192,
        };
        return Ok(VideoRatePlan {
            video_kbps: options.vbitrate,
            audio_kbps,
        });
    };
    let Some(raw_budget) = size_budget_kbps(source, max_size) else {
        return Err(ConvError::Message(
            "cannot enforce Max File Size: source duration is unknown".into(),
        ));
    };
    let budget = raw_budget * 0.90;
    let audio_min = match target {
        "webm" => 6.0,
        "ogv" => 48.0,
        "avi" => 32.0,
        "mp4" | "mkv" => 16.0,
        _ => 8.0,
    };
    let audio_kbps = (budget * 0.20).clamp(audio_min, 192.0).floor() as u32;
    let video_min = 8.0;
    if budget < audio_kbps as f64 + video_min {
        return Err(ConvError::Message(format!(
            "cannot fit under {} MB: {} is too long for the lowest video bitrate",
            format_number(max_size),
            target.to_ascii_uppercase()
        )));
    }
    let video_kbps = (budget - audio_kbps as f64).floor().max(video_min) as u32;
    Ok(VideoRatePlan {
        video_kbps: Some(video_kbps),
        audio_kbps,
    })
}

fn build_audio_cmd(
    source: &Path,
    output: &Path,
    target: &str,
    options: &Options,
) -> ConvResult<Vec<String>> {
    let (codec, lossy) = audio_codec(target)
        .ok_or_else(|| ConvError::Message(format!("unsupported audio target: {target}")))?;
    let plan = audio_size_plan(source, target, options)?;

    let mut cmd = vec![
        "ffmpeg".to_string(),
        "-hide_banner".into(),
        "-y".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        "-i".into(),
        source.to_string_lossy().into(),
        "-map_metadata".into(),
        "0".into(),
        "-map_chapters".into(),
        "0".into(),
        "-map".into(),
        "0:a".into(),
    ];

    if let Some(index) = probe::attached_picture(source) {
        if cover_capable(target) {
            cmd.extend([
                "-map".into(),
                format!("0:{index}"),
                "-c:v".into(),
                "copy".into(),
                "-disposition:v".into(),
                "attached_pic".into(),
            ]);
        }
    }
    if target == "mp3" {
        cmd.extend(["-id3v2_version".into(), "3".into()]);
    }
    let sample_rate = plan
        .as_ref()
        .and_then(|plan| plan.sample_rate)
        .or(options.sample_rate);
    if let Some(sample_rate) = sample_rate {
        let rate = if target == "opus" {
            libopus_rate(sample_rate)
        } else {
            sample_rate
        };
        cmd.extend(["-ar".into(), rate.to_string()]);
    }
    if lossy {
        let mut bitrate = effective_bitrate(source, options);
        if let Some(plan) = &plan {
            if let Some(rate) = plan.bitrate_kbps {
                bitrate = rate;
            }
        }
        if target == "opus" {
            bitrate = bitrate.min(256);
        }
        cmd.extend(["-b:a".into(), format!("{bitrate}k")]);
    }
    cmd.extend(["-c:a".into(), codec.into(), output.to_string_lossy().into()]);
    Ok(cmd)
}

fn build_video_cmd(
    source: &Path,
    output: &Path,
    target: &str,
    options: &Options,
) -> ConvResult<Vec<String>> {
    let preset = options
        .preset
        .clone()
        .filter(|preset| VIDEO_PRESETS.contains(&preset.as_str()))
        .unwrap_or_else(|| "medium".into());
    let crf = effective_crf(source, options);
    let rates = video_rate_plan(source, target, options)?;
    let vbitrate = rates.video_kbps;
    let audio_kbps = rates.audio_kbps;

    let rate_arg = if let Some(bitrate) = vbitrate {
        vec!["-b:v".to_string(), format!("{}k", bitrate.max(8))]
    } else {
        vec!["-crf".to_string(), crf.to_string()]
    };

    let mut cmd = vec![
        "ffmpeg".to_string(),
        "-hide_banner".into(),
        "-y".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        "-i".into(),
        source.to_string_lossy().into(),
        "-map_metadata".into(),
        "0".into(),
        "-map_chapters".into(),
        "0".into(),
    ];

    if target == "gif" {
        cmd.extend([
            "-filter_complex".into(),
            "[0:v]fps=15,split[a][b];[a]palettegen=max_colors=256[p];\
             [b][p]paletteuse=dither=bayer:bayer_scale=5"
                .into(),
            "-loop".into(),
            "0".into(),
            output.to_string_lossy().into(),
        ]);
        return Ok(cmd);
    }

    let sample_rate = if audio_kbps < 32 && matches!(target, "mp4" | "mkv") {
        Some(
            options
                .sample_rate
                .filter(|rate| *rate <= 24_000)
                .unwrap_or(22_050),
        )
    } else {
        options.sample_rate
    };
    if let Some(sample_rate) = sample_rate {
        let rate = if target == "webm" {
            libopus_rate(sample_rate)
        } else {
            sample_rate
        };
        cmd.extend(["-ar".into(), rate.to_string()]);
    }

    let vp9 = [
        "-deadline",
        if preset == "fast" || preset == "faster" || preset == "veryfast" {
            "realtime"
        } else if preset == "slower" || preset == "veryslow" {
            "best"
        } else {
            "good"
        },
        "-cpu-used",
        match preset.as_str() {
            "ultrafast" => "8",
            "superfast" => "6",
            "veryfast" => "5",
            "faster" => "4",
            "fast" => "2",
            "medium" => "1",
            "slow" => "0",
            "slower" => "1",
            _ => "0",
        },
    ];

    match target {
        "mp4" => {
            cmd.extend(["-c:v".into(), "libx264".into()]);
            cmd.extend(rate_arg);
            cmd.extend([
                "-preset".into(),
                preset.clone(),
                "-pix_fmt".into(),
                "yuv420p".into(),
                "-c:a".into(),
                "aac".into(),
                "-b:a".into(),
                format!("{audio_kbps}k"),
                "-movflags".into(),
                "+faststart".into(),
            ]);
        }
        "mkv" => {
            cmd.extend(["-c:v".into(), "libx264".into()]);
            cmd.extend(rate_arg);
            cmd.extend([
                "-preset".into(),
                preset.clone(),
                "-c:a".into(),
                "aac".into(),
                "-b:a".into(),
                format!("{audio_kbps}k"),
            ]);
        }
        "webm" => {
            cmd.extend(["-c:v".into(), "libvpx-vp9".into()]);
            if vbitrate.is_some() {
                cmd.extend(rate_arg);
            } else {
                cmd.extend(["-crf".into(), crf.to_string(), "-b:v".into(), "0".into()]);
            }
            cmd.extend(vp9.iter().map(|value| value.to_string()));
            cmd.extend([
                "-c:a".into(),
                "libopus".into(),
                "-b:a".into(),
                format!("{audio_kbps}k"),
            ]);
        }
        "ogv" => {
            cmd.extend(["-c:v".into(), "libtheora".into()]);
            if vbitrate.is_some() {
                cmd.extend(rate_arg);
            } else {
                cmd.extend(["-q:v".into(), quality_from_crf(crf).to_string()]);
            }
            cmd.extend([
                "-c:a".into(),
                "libvorbis".into(),
                "-b:a".into(),
                format!("{audio_kbps}k"),
            ]);
        }
        "avi" => {
            cmd.extend(["-c:v".into(), "mpeg4".into()]);
            if vbitrate.is_some() {
                cmd.extend(rate_arg);
            } else {
                cmd.extend(["-q:v".into(), quality_from_crf(crf).to_string()]);
            }
            cmd.extend([
                "-c:a".into(),
                "libmp3lame".into(),
                "-b:a".into(),
                format!("{audio_kbps}k"),
            ]);
        }
        _ => {
            return Err(ConvError::Message(format!(
                "unsupported video target: {target}"
            )))
        }
    }

    cmd.push(output.to_string_lossy().into());
    Ok(cmd)
}

fn convert_audio_to(
    job: &Job,
    output: &Path,
    options: &Options,
    progress: &ProgressFn,
    cancel: &Arc<AtomicBool>,
) -> ConvResult<()> {
    let duration = probe::duration_ms(&job.source);
    let feed = ffmpeg_progress_feed(
        progress,
        job.source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
        duration,
    );
    let cmd = build_audio_cmd(&job.source, output, &job.target, options)?;
    if let Err(error) = run_process(&cmd, Some(&feed), cancel, None) {
        let _ = fs::remove_file(output);
        return Err(error);
    }
    Ok(())
}

fn convert_audio(
    job: &Job,
    progress: &ProgressFn,
    cancel: &Arc<AtomicBool>,
) -> ConvResult<PathBuf> {
    let output = job.output_path();
    convert_audio_to(job, &output, &job.options, progress, cancel)?;
    Ok(output)
}

fn convert_video_to(
    job: &Job,
    output: &Path,
    options: &Options,
    progress: &ProgressFn,
    cancel: &Arc<AtomicBool>,
) -> ConvResult<()> {
    let (has_video, _) = probe::probe_streams(&job.source);
    if has_video == Some(false) {
        return Err(ConvError::Message("source has no video stream".into()));
    }
    let duration = probe::duration_ms(&job.source);
    let feed = ffmpeg_progress_feed(
        progress,
        job.source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
        duration,
    );
    let cmd = build_video_cmd(&job.source, output, &job.target, options)?;
    if let Err(error) = run_process(&cmd, Some(&feed), cancel, None) {
        let _ = fs::remove_file(output);
        return Err(error);
    }
    Ok(())
}

fn convert_video(
    job: &Job,
    progress: &ProgressFn,
    cancel: &Arc<AtomicBool>,
) -> ConvResult<PathBuf> {
    let output = job.output_path();
    convert_video_to(job, &output, &job.options, progress, cancel)?;
    Ok(output)
}

// -------------------------------------------------------------------------
// ImageMagick helpers
// -------------------------------------------------------------------------

fn size_limit_bytes(options: &Options) -> Option<f64> {
    let value = options.max_size?;
    (value > 0.0).then_some(value * 1_000_000.0)
}

fn image_settings(options: &Options) -> Vec<String> {
    let scale = options.scale.unwrap_or(100).clamp(1, 400);
    let mut args = vec!["-resize".into(), format!("{scale}%")];
    if let Some(quality) = options.quality {
        args.extend(["-quality".into(), quality.clamp(1, 100).to_string()]);
    }
    args
}

fn build_magick_cmd(
    source: &Path,
    output: &Path,
    options: &Options,
    first_frame: bool,
) -> Vec<String> {
    let source_arg = if first_frame {
        format!("{}[0]", source.to_string_lossy())
    } else {
        source.to_string_lossy().into()
    };
    let mut cmd = vec!["magick".to_string(), source_arg];
    cmd.extend(image_settings(options));
    cmd.push(output.to_string_lossy().into());
    cmd
}

fn move_file(from: &Path, to: &Path) -> ConvResult<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to)?;
    let _ = fs::remove_file(from);
    Ok(())
}

fn magick_fit(
    source: &Path,
    output: &Path,
    options: &Options,
    cancel: &Arc<AtomicBool>,
    first_frame: bool,
) -> ConvResult<()> {
    let Some(limit) = size_limit_bytes(options) else {
        run_process(
            &build_magick_cmd(source, output, options, first_frame),
            None,
            cancel,
            None,
        )?;
        return Ok(());
    };

    let target = output
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let quality_capable = matches!(target.as_str(), "jpg" | "jpeg" | "webp" | "avif");
    let base_scale = options.scale.unwrap_or(100).clamp(1, 400);
    let base_quality = options.quality.unwrap_or(95).clamp(1, 100);
    let suffix = output
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();

    let temp = tempfile::Builder::new()
        .prefix("fileconverter-fit-")
        .tempdir()
        .map_err(|error| ConvError::Message(error.to_string()))?;
    let candidate = temp.path().join(format!("cand{suffix}"));

    let attempt = |scale: u32, quality: Option<u32>| -> ConvResult<u64> {
        let mut trial = options.clone();
        trial.scale = Some(scale.clamp(1, 400));
        if let Some(quality) = quality {
            trial.quality = Some(quality.clamp(1, 100));
        }
        run_process(
            &build_magick_cmd(source, &candidate, &trial, first_frame),
            None,
            cancel,
            None,
        )?;
        Ok(fs::metadata(&candidate)
            .map_err(|error| ConvError::Message(error.to_string()))?
            .len())
    };

    let size = attempt(base_scale, Some(base_quality))?;
    if size as f64 <= limit {
        move_file(&candidate, output)?;
        return Ok(());
    }

    if quality_capable {
        let mut low = 1i64;
        let mut high = base_quality as i64;
        let mut best_quality = None;
        while low <= high {
            let middle = (low + high) / 2;
            if (attempt(base_scale, Some(middle as u32))? as f64) <= limit {
                best_quality = Some(middle as u32);
                low = middle + 1;
            } else {
                high = middle - 1;
            }
        }
        if let Some(quality) = best_quality {
            let size = attempt(base_scale, Some(quality))?;
            if size as f64 <= limit {
                move_file(&candidate, output)?;
                return Ok(());
            }
        }
    }

    let final_quality = if quality_capable { 1 } else { base_quality };
    let mut low = 1i64;
    let mut high = base_scale as i64;
    let mut best_scale = None;
    while low <= high {
        let middle = (low + high) / 2;
        if (attempt(middle as u32, Some(final_quality))? as f64) <= limit {
            best_scale = Some(middle as u32);
            low = middle + 1;
        } else {
            high = middle - 1;
        }
    }
    let final_scale = best_scale.unwrap_or(1);
    let size = attempt(final_scale, Some(final_quality))?;
    if size as f64 <= limit {
        move_file(&candidate, output)?;
        Ok(())
    } else {
        Err(ConvError::Message(format!(
            "could not fit output under {} MB (smallest attempt: {} MB)",
            format_number(limit / 1_000_000.0),
            format_number(size as f64 / 1_000_000.0)
        )))
    }
}

fn convert_raster_image(
    job: &Job,
    _progress: &ProgressFn,
    cancel: &Arc<AtomicBool>,
) -> ConvResult<PathBuf> {
    let output = job.output_path();
    magick_fit(&job.source, &output, &job.options, cancel, false)?;
    Ok(output)
}

fn convert_pdf_to_image(
    job: &Job,
    _progress: &ProgressFn,
    cancel: &Arc<AtomicBool>,
) -> ConvResult<PathBuf> {
    let output = job.output_path();
    magick_fit(&job.source, &output, &job.options, cancel, true)?;
    Ok(output)
}

// -------------------------------------------------------------------------
// Document helpers (LibreOffice)
// -------------------------------------------------------------------------

fn path_to_file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for ch in path.to_string_lossy().chars() {
        if ch.is_ascii_alphanumeric() || "/-._~".contains(ch) {
            uri.push(ch);
        } else {
            let mut buffer = [0u8; 4];
            for byte in ch.encode_utf8(&mut buffer).as_bytes() {
                uri.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    uri
}

fn office_to_pdf(source: &Path, dest_dir: &Path, cancel: &Arc<AtomicBool>) -> ConvResult<PathBuf> {
    let profile = tempfile::Builder::new()
        .prefix("fileconverter-lo-")
        .tempdir()
        .map_err(|error| ConvError::Message(error.to_string()))?;
    let soffice = binary("soffice").unwrap_or_else(|| PathBuf::from("soffice"));
    let args = vec![
        soffice.to_string_lossy().into(),
        format!("-env:UserInstallation={}", path_to_file_uri(profile.path())),
        "--headless".into(),
        "--nologo".into(),
        "--nofirststartwizard".into(),
        "--convert-to".into(),
        "pdf".into(),
        "--outdir".into(),
        dest_dir.to_string_lossy().into(),
        source.to_string_lossy().into(),
    ];
    run_process(&args, None, cancel, Some(Duration::from_secs(1800)))?;

    let stem = source.file_stem().unwrap_or_default().to_string_lossy();
    let produced = dest_dir.join(format!("{stem}.pdf"));
    if produced.exists() {
        return Ok(produced);
    }
    if let Ok(entries) = fs::read_dir(dest_dir) {
        let mut candidates: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().map(|ext| ext == "pdf").unwrap_or(false))
            .collect();
        candidates.sort();
        if let Some(candidate) = candidates.into_iter().next() {
            return Ok(candidate);
        }
    }
    Err(ConvError::Message(format!(
        "LibreOffice produced no PDF for {}",
        source.file_name().unwrap_or_default().to_string_lossy()
    )))
}

fn shrink_pdf(path: &Path, limit_bytes: f64, cancel: &Arc<AtomicBool>) -> ConvResult<()> {
    let Some(gs) = binary("gs") else {
        return Err(ConvError::Message(
            "Ghostscript (gs) is required to enforce Max File Size for PDF output".into(),
        ));
    };
    let original_size = fs::metadata(path)?.len();
    if original_size as f64 <= limit_bytes {
        return Ok(());
    }

    let temp = tempfile::Builder::new()
        .prefix("fileconverter-gs-")
        .tempdir()
        .map_err(|error| ConvError::Message(error.to_string()))?;
    let passes: Vec<Vec<&str>> = vec![
        vec!["-dPDFSETTINGS=/ebook"],
        vec!["-dPDFSETTINGS=/screen"],
        vec![
            "-dPDFSETTINGS=/screen",
            "-dColorImageResolution=36",
            "-dGrayImageResolution=36",
            "-dMonoImageResolution=150",
            "-dDownsampleColorImages=true",
            "-dDownsampleGrayImages=true",
            "-dDownsampleMonoImages=true",
            "-dColorImageDownsampleType=/Bicubic",
            "-dGrayImageDownsampleType=/Bicubic",
        ],
    ];

    let mut best_size = original_size;
    for (index, pass) in passes.iter().enumerate() {
        let out = temp.path().join(format!("shrink{index}.pdf"));
        let mut args = vec![
            gs.to_string_lossy().into(),
            "-sDEVICE=pdfwrite".into(),
            "-dNOPAUSE".into(),
            "-dBATCH".into(),
            "-dQUIET".into(),
        ];
        for option in pass {
            args.push((*option).into());
        }
        args.push(format!("-sOutputFile={}", out.to_string_lossy()));
        args.push(path.to_string_lossy().into());

        run_process(&args, None, cancel, Some(Duration::from_secs(600)))?;
        let size = fs::metadata(&out)?.len();
        if size as f64 <= limit_bytes {
            move_file(&out, path)?;
            return Ok(());
        }
        best_size = best_size.min(size);
    }

    Err(ConvError::Message(format!(
        "could not fit PDF under {} MB (smallest attempt: {} MB)",
        format_number(limit_bytes / 1_000_000.0),
        format_number(best_size as f64 / 1_000_000.0)
    )))
}

fn convert_office_to_pdf(
    job: &Job,
    _progress: &ProgressFn,
    cancel: &Arc<AtomicBool>,
) -> ConvResult<PathBuf> {
    let output = job.output_path();
    let limit = size_limit_bytes(&job.options);
    let temp = tempfile::Builder::new()
        .prefix("fileconverter-out-")
        .tempdir()
        .map_err(|error| ConvError::Message(error.to_string()))?;
    let pdf = office_to_pdf(&job.source, temp.path(), cancel)?;
    if let Some(limit) = limit {
        shrink_pdf(&pdf, limit, cancel)?;
    }
    move_file(&pdf, &output)?;
    Ok(output)
}

fn convert_office_to_image(
    job: &Job,
    _progress: &ProgressFn,
    cancel: &Arc<AtomicBool>,
) -> ConvResult<PathBuf> {
    let output = job.output_path();
    let temp = tempfile::Builder::new()
        .prefix("fileconverter-out-")
        .tempdir()
        .map_err(|error| ConvError::Message(error.to_string()))?;
    let pdf = office_to_pdf(&job.source, temp.path(), cancel)?;
    magick_fit(&pdf, &output, &job.options, cancel, true)?;
    Ok(output)
}

// -------------------------------------------------------------------------
// Image -> PDF
// -------------------------------------------------------------------------

fn img2pdf(source: &Path, output: &Path, cancel: &Arc<AtomicBool>) -> ConvResult<()> {
    let Some(binary) = binary("img2pdf") else {
        return Err(ConvError::Message("img2pdf is not installed".into()));
    };
    run_process(
        &[
            binary.to_string_lossy().into(),
            "-o".into(),
            output.to_string_lossy().into(),
            source.to_string_lossy().into(),
        ],
        None,
        cancel,
        None,
    )?;
    Ok(())
}

fn convert_to_pdf(
    job: &Job,
    _progress: &ProgressFn,
    cancel: &Arc<AtomicBool>,
) -> ConvResult<PathBuf> {
    let output = job.output_path();
    let limit = size_limit_bytes(&job.options);
    match img2pdf(&job.source, &output, cancel) {
        Ok(()) => {}
        Err(ConvError::Cancelled) => return Err(ConvError::Cancelled),
        Err(ConvError::Message(_)) => {
            // Let Ghostscript do the size fitting for PDFs; ImageMagick only
            // assembles the initial PDF.
            let mut magick_options = job.options.clone();
            magick_options.max_size = None;
            magick_fit(&job.source, &output, &magick_options, cancel, false)?;
        }
    }
    if let Some(limit) = limit {
        if let Err(error) = shrink_pdf(&output, limit, cancel) {
            let _ = fs::remove_file(&output);
            return Err(error);
        }
    }
    Ok(output)
}

// -------------------------------------------------------------------------
// Dispatch
// -------------------------------------------------------------------------

pub fn dependency_report(jobs: &[Job]) -> Vec<String> {
    let mut packages = Vec::new();
    for job in jobs {
        packages.extend(required_packages(&job.category, &job.target, &job.source));
    }
    missing_packages(&packages)
}

fn output_size(path: &Path) -> ConvResult<u64> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|error| ConvError::Message(error.to_string()))
}

/// Last line of defence: never report success for an output that is over the
/// requested limit. If the normal encoder missed the target, retry with a
/// smaller budget; if even the smallest attempt is still too large, fail and
/// remove the oversized file.
fn enforce_size_limit(
    job: &Job,
    output: &Path,
    limit: f64,
    progress: &ProgressFn,
    cancel: &Arc<AtomicBool>,
) -> ConvResult<()> {
    let mut size = output_size(output)?;
    if size as f64 <= limit {
        return Ok(());
    }

    let category = options_category(&job.category, Some(&job.target));
    let original_max = job.options.max_size.unwrap_or(limit / 1_000_000.0);

    match category.as_str() {
        "audio" => {
            if audio_codec(&job.target)
                .map(|(_, lossy)| !lossy)
                .unwrap_or(false)
            {
                return Err(ConvError::Message(format!(
                    "could not fit {} under {} MB: lossless output cannot be shrunk; \
                     choose a lossy format or raise the limit",
                    job.target.to_ascii_uppercase(),
                    format_number(limit / 1_000_000.0)
                )));
            }
            for factor in [0.85_f64, 0.65, 0.45] {
                let mut retry = job.options.clone();
                retry.max_size = Some((original_max * factor).max(0.000_001));
                retry.bitrate = None;
                convert_audio_to(job, output, &retry, progress, cancel)?;
                size = output_size(output)?;
                if size as f64 <= limit {
                    return Ok(());
                }
            }
        }
        "video" => {
            for factor in [0.85_f64, 0.65, 0.45] {
                let mut retry = job.options.clone();
                retry.max_size = Some((original_max * factor).max(0.000_001));
                retry.vbitrate = None;
                convert_video_to(job, output, &retry, progress, cancel)?;
                size = output_size(output)?;
                if size as f64 <= limit {
                    return Ok(());
                }
            }
        }
        "image" => {
            if job.target == "pdf" {
                shrink_pdf(output, limit * 0.98, cancel)?;
            } else {
                let mut retry = job.options.clone();
                retry.max_size = Some(((limit / 1_000_000.0) * 0.85).max(0.000_001));
                let ext = matrix::extension_of(&job.source).unwrap_or_default();
                let first_frame = ext == "pdf" || matrix::OFFICE_EXTS.contains(&ext.as_str());
                magick_fit(&job.source, output, &retry, cancel, first_frame)?;
            }
            size = output_size(output)?;
        }
        "document" => {
            shrink_pdf(output, limit * 0.98, cancel)?;
            size = output_size(output)?;
        }
        _ => {}
    }

    if size as f64 <= limit {
        Ok(())
    } else {
        Err(ConvError::Message(format!(
            "could not fit output under {} MB (smallest attempt: {} MB)",
            format_number(limit / 1_000_000.0),
            format_number(size as f64 / 1_000_000.0)
        )))
    }
}

pub fn convert_job(job: &Job, progress: &ProgressFn, cancel: &Arc<AtomicBool>) -> ConversionResult {
    let cancelled = || ConversionResult {
        job: job.clone(),
        status: Status::Cancelled,
        output: None,
        message: "cancelled".into(),
    };

    if cancel.load(Ordering::SeqCst) {
        return ConversionResult {
            job: job.clone(),
            status: Status::Cancelled,
            output: None,
            message: "cancelled before start".into(),
        };
    }
    if !job.source.exists() {
        return ConversionResult {
            job: job.clone(),
            status: Status::Error,
            output: None,
            message: "file not found".into(),
        };
    }

    let result = (|| -> ConvResult<PathBuf> {
        progress(
            0.0,
            format!(
                "{}: starting...",
                job.source.file_name().unwrap_or_default().to_string_lossy()
            ),
        );
        let ext = matrix::extension_of(&job.source).unwrap_or_default();
        let output = match job.category.as_str() {
            "Audio" => convert_audio(job, progress, cancel)?,
            "Video" => {
                if audio_codec(&job.target).is_some() {
                    convert_audio(job, progress, cancel)?
                } else {
                    convert_video(job, progress, cancel)?
                }
            }
            "Image" => {
                if matrix::OFFICE_EXTS.contains(&ext.as_str()) {
                    convert_office_to_image(job, progress, cancel)?
                } else if ext == "pdf" {
                    convert_pdf_to_image(job, progress, cancel)?
                } else if job.target == "pdf" {
                    convert_to_pdf(job, progress, cancel)?
                } else {
                    convert_raster_image(job, progress, cancel)?
                }
            }
            "Document" => {
                if job.target == "pdf" {
                    convert_office_to_pdf(job, progress, cancel)?
                } else {
                    convert_office_to_image(job, progress, cancel)?
                }
            }
            _ => {
                return Ok(PathBuf::new());
            }
        };
        Ok(output)
    })();

    let result = result.and_then(|output| {
        if output.as_os_str().is_empty() {
            return Ok(output);
        }
        if let Some(limit) = size_limit_bytes(&job.options) {
            if let Err(error) = enforce_size_limit(job, &output, limit, progress, cancel) {
                let _ = fs::remove_file(&output);
                return Err(error);
            }
        }
        progress(
            100.0,
            format!(
                "{}: done",
                job.source.file_name().unwrap_or_default().to_string_lossy()
            ),
        );
        Ok(output)
    });

    match result {
        Ok(output) if output.as_os_str().is_empty() => ConversionResult {
            job: job.clone(),
            status: Status::Skipped,
            output: None,
            message: "unsupported input format".into(),
        },
        Ok(output) => ConversionResult {
            job: job.clone(),
            status: Status::Ok,
            output: Some(output),
            message: String::new(),
        },
        Err(ConvError::Cancelled) => cancelled(),
        Err(ConvError::Message(message)) => ConversionResult {
            job: job.clone(),
            status: Status::Error,
            output: None,
            message: if message.trim().is_empty() {
                "conversion failed".into()
            } else {
                message
            },
        },
    }
}

pub fn run_batch(
    jobs: Vec<Job>,
    cancel: Arc<AtomicBool>,
    progress: Arc<ProgressFn>,
    max_workers: usize,
) -> Vec<ConversionResult> {
    if jobs.is_empty() {
        return Vec::new();
    }
    let total = jobs.len();
    let queue = Arc::new(Mutex::new(
        jobs.into_iter().enumerate().collect::<VecDeque<_>>(),
    ));
    let results: Arc<Mutex<Vec<Option<ConversionResult>>>> =
        Arc::new(Mutex::new((0..total).map(|_| None).collect()));
    let completed = Arc::new(AtomicUsize::new(0));

    thread::scope(|scope| {
        for _ in 0..max_workers.max(1) {
            let queue = Arc::clone(&queue);
            let results = Arc::clone(&results);
            let completed = Arc::clone(&completed);
            let cancel = Arc::clone(&cancel);
            let progress = Arc::clone(&progress);
            scope.spawn(move || loop {
                let next = queue.lock().unwrap().pop_front();
                let Some((index, job)) = next else {
                    break;
                };
                let callback = {
                    let progress = Arc::clone(&progress);
                    move |pct: f64, message: String| {
                        let overall =
                            (index as f64 + pct.clamp(0.0, 100.0) / 100.0) / total as f64 * 100.0;
                        progress(overall, format!("[{}/{}] {message}", index + 1, total));
                    }
                };
                let result = convert_job(&job, &callback, &cancel);
                results.lock().unwrap()[index] = Some(result);
                let done = completed.fetch_add(1, Ordering::SeqCst) + 1;
                progress(100.0, format!("finished {done}/{total}"));
            });
        }
    });

    let results = Arc::try_unwrap(results)
        .map(|mutex| mutex.into_inner().unwrap())
        .unwrap_or_default();
    results.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_bitrate_handles_cd_wav() {
        assert_eq!(nearest_option(1411.2, AUDIO_BITRATE_OPTIONS), Some(320));
        assert_eq!(nearest_option(192.0, AUDIO_BITRATE_OPTIONS), Some(192));
        assert_eq!(nearest_option(112.0, AUDIO_BITRATE_OPTIONS), Some(128));
        assert_eq!(nearest_option(255.0, AUDIO_BITRATE_OPTIONS), Some(256));
    }

    #[test]
    fn size_math_is_decimal_megabytes() {
        let options = Options {
            max_size: Some(1.0),
            ..Options::default()
        };
        assert_eq!(size_limit_bytes(&options), Some(1_000_000.0));
        assert_eq!(pick_bitrate(20.4, &[8, 16, 24, 32]), Some(16));
        assert_eq!(pick_bitrate(7.9, &[8, 16, 24, 32]), None);
    }

    #[test]
    fn lossless_audio_is_checked_after_encoding() {
        let source = Path::new("/tmp/nonexistent-audio.wav");
        let options = Options {
            max_size: Some(1.0),
            ..Options::default()
        };
        assert_eq!(audio_size_plan(source, "flac", &options).unwrap(), None);
        assert_eq!(audio_size_plan(source, "wav", &options).unwrap(), None);
        assert!(audio_size_plan(source, "mp3", &options).is_err());
    }
}
