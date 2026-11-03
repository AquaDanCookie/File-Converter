//! Terminal interface for File Converter.
//!
//! Examples:
//!     fileconverter clip.mp4 --to webm
//!     fileconverter photo.png --to jpg --scale 80 --quality 85
//!     fileconverter song.wav --to mp3
//!     fileconverter *                     # opens the GUI with the files

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use clap::Parser;

use crate::engine::{self, Job, ProgressFn};
use crate::{matrix, presets};

#[derive(Parser, Debug)]
#[command(
    name = "fileconverter",
    version,
    about = "Convert/compress audio, video, image and document files.",
    disable_help_subcommand = true
)]
struct Cli {
    /// Input files (plain paths or file:// URLs)
    #[arg(value_name = "FILE")]
    files: Vec<String>,

    /// Target format; without it the GUI opens
    #[arg(long, value_name = "FORMAT")]
    to: Option<String>,

    /// Force the GUI instead of headless conversion
    #[arg(long)]
    gui: bool,

    /// Video CRF 0-51 (lower = better, default: auto)
    #[arg(long, value_name = "N")]
    crf: Option<u32>,

    /// Video encoder preset (ultrafast..veryslow)
    #[arg(long, value_name = "NAME")]
    preset: Option<String>,

    /// Video constant bitrate in kbps (overrides CRF)
    #[arg(long, value_name = "K")]
    vbitrate: Option<u32>,

    /// Audio bitrate in kbps (128/192/256/320; default: auto)
    #[arg(long, value_name = "K")]
    bitrate: Option<u32>,

    /// Resample audio to this rate, e.g. 48000
    #[arg(long = "sample-rate", value_name = "HZ")]
    sample_rate: Option<u32>,

    /// Fit the output under this many MB (decimal; needs a duration for
    /// audio/video; overrides --bitrate/--vbitrate)
    #[arg(long = "max-size", value_name = "MB")]
    max_size: Option<f64>,

    /// Image scale percent 25-100
    #[arg(long, value_name = "PCT")]
    scale: Option<u32>,

    /// Image quality 1-100 (default: auto)
    #[arg(long, value_name = "Q")]
    quality: Option<u32>,

    /// Where to place converted files (default: next to input)
    #[arg(long, value_name = "DIR")]
    outdir: Option<PathBuf>,

    /// Parallel conversions
    #[arg(long, value_name = "N", default_value_t = 2)]
    jobs: usize,

    /// Print the supported format matrix and exit
    #[arg(long = "list-formats")]
    list_formats: bool,

    #[arg(long = "write-default-presets", hide = true)]
    write_default_presets: bool,
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &text[index + 1..index + 3];
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                output.push(value);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

/// Accept plain paths as well as the file:// URLs passed by KDE/Dolphin.
fn resolve(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("file://") {
        PathBuf::from(percent_decode(rest))
    } else {
        PathBuf::from(path)
    }
}

fn print_matrix() {
    println!("Supported conversions (category -> target formats):");
    for (category, outputs) in matrix::CATEGORY_OUTPUTS {
        let formats = outputs
            .iter()
            .map(|format| format.to_ascii_uppercase())
            .collect::<Vec<_>>()
            .join(", ");
        println!("  {category:<9}{formats}");
    }
}

fn apply_cli_options(options: &mut presets::Options, cli: &Cli) {
    if let Some(crf) = cli.crf {
        options.crf = Some(crf);
    }
    if let Some(preset) = &cli.preset {
        options.preset = Some(preset.clone());
    }
    if let Some(vbitrate) = cli.vbitrate {
        options.vbitrate = Some(vbitrate);
    }
    if let Some(bitrate) = cli.bitrate {
        options.bitrate = Some(bitrate);
    }
    if let Some(sample_rate) = cli.sample_rate {
        options.sample_rate = Some(sample_rate);
    }
    if let Some(max_size) = cli.max_size {
        options.max_size = Some(max_size);
    }
    if let Some(scale) = cli.scale {
        options.scale = Some(scale);
    }
    if let Some(quality) = cli.quality {
        options.quality = Some(quality);
    }
}

fn build_jobs(files: &[PathBuf], target: &str, cli: &Cli) -> (Vec<Job>, Vec<(PathBuf, String)>) {
    let store = presets::PresetStore::load();
    let mut jobs = Vec::new();
    let mut skipped = Vec::new();

    for path in files {
        let Some(category) = matrix::detect_category(path) else {
            skipped.push((path.clone(), "unsupported input format".into()));
            continue;
        };
        if !matrix::allows_target(category, target) {
            skipped.push((path.clone(), format!("not convertible to {target}")));
            continue;
        }
        let mut options =
            engine::quality_options(path, category, &store.defaults_for(category), Some(target));
        apply_cli_options(&mut options, cli);
        jobs.push(Job {
            source: path.clone(),
            target: target.to_string(),
            category: category.to_string(),
            options,
            outdir: cli.outdir.clone(),
        });
    }
    (jobs, skipped)
}

fn quality_note(job: &Job) -> String {
    engine::quality_note(job)
}

fn run_headless(cli: &Cli) -> i32 {
    let target = cli.to.as_deref().unwrap_or("").to_ascii_lowercase();
    let known = matrix::CATEGORY_OUTPUTS
        .iter()
        .flat_map(|(_, outputs)| outputs.iter())
        .any(|format| *format == target);
    if !known {
        eprintln!(
            "error: unknown target format '{}'",
            cli.to.as_deref().unwrap_or("")
        );
        print_matrix();
        return 2;
    }

    let files: Vec<PathBuf> = cli.files.iter().map(|path| resolve(path)).collect();
    let missing: Vec<&PathBuf> = files.iter().filter(|path| !path.exists()).collect();
    if !missing.is_empty() {
        for path in missing {
            eprintln!("error: input not found: {}", path.display());
        }
        return 2;
    }

    let (jobs, skipped) = build_jobs(&files, &target, cli);
    println!(
        "Converting {} file(s) to {} …",
        files.len(),
        target.to_ascii_uppercase()
    );
    for (path, reason) in &skipped {
        println!(
            "  SKIP  {}: {reason}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
    }

    let missing_packages = engine::dependency_report(&jobs);
    if !missing_packages.is_empty() {
        eprintln!("\nMissing dependencies: {}", missing_packages.join(", "));
        eprintln!("Install them with:\n  {}", engine::PACMAN_HINT);
        return 1;
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let progress: Arc<ProgressFn> = Arc::new(|pct, message| {
        println!("  [{pct:5.1}%] {message}");
    });
    let results = engine::run_batch(jobs.clone(), cancel, progress, cli.jobs);

    println!("\nSummary:");
    let mut ok_count = 0;
    for result in &results {
        let name = result
            .job
            .source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        match result.status {
            engine::Status::Ok => {
                ok_count += 1;
                let output = result
                    .output
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .unwrap_or_default()
                    .to_string_lossy();
                let size = result
                    .output
                    .as_ref()
                    .and_then(|path| path.metadata().ok())
                    .map(|metadata| metadata.len() as f64)
                    .unwrap_or(0.0);
                println!(
                    "  OK    {name} -> {output}{} ({})",
                    quality_note(&result.job),
                    engine::human_size(size)
                );
            }
            engine::Status::Skipped => println!("  SKIP  {name}: {}", result.message),
            engine::Status::Cancelled => println!("  ----  {name}: {}", result.message),
            engine::Status::Error => println!("  FAIL  {name}: {}", result.message),
        }
    }

    let expected = files.len();
    if ok_count == expected {
        0
    } else if ok_count == 0 {
        1
    } else {
        // Everything not converted was cleanly skipped/unsupported.
        0
    }
}

pub fn main() -> i32 {
    let cli = Cli::parse();

    if cli.list_formats {
        print_matrix();
        return 0;
    }

    if cli.write_default_presets {
        let path = presets::presets_file();
        if let Some(error) = presets::write_defaults(&path) {
            eprintln!("error: {error}");
            return 1;
        }
        println!("Presets written to {}", path.display());
        return 0;
    }

    let files: Vec<PathBuf> = cli.files.iter().map(|path| resolve(path)).collect();

    if cli.gui || cli.to.is_none() {
        return match crate::gui::run_gui(files) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("could not start the GUI: {error}");
                1
            }
        };
    }

    run_headless(&cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_percent_encoded_file_urls() {
        assert_eq!(
            resolve("file:///tmp/My%20File.wav"),
            PathBuf::from("/tmp/My File.wav")
        );
        assert_eq!(resolve("/tmp/plain.wav"), PathBuf::from("/tmp/plain.wav"));
    }

    #[test]
    fn detects_unknown_target() {
        let cli = Cli::parse_from(["fileconverter", "song.wav", "--to", "nope"]);
        assert_eq!(run_headless(&cli), 2);
    }
}
