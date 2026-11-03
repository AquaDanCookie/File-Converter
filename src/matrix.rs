//! Input format detection and the conversion matrix.

use std::collections::HashSet;
use std::path::Path;

use crate::probe;

pub fn category_outputs(category: &str) -> &'static [&'static str] {
    match category {
        "Audio" => &["flac", "aac", "ogg", "mp3", "wav", "m4a", "opus"],
        "Video" => &["webm", "mkv", "mp4", "ogv", "avi", "gif"],
        "Image" => &["png", "jpg", "ico", "webp", "avif"],
        "Document" => &["pdf"],
        _ => &[],
    }
}

pub const CATEGORY_OUTPUTS: &[(&str, &[&str])] = &[
    (
        "Audio",
        &["flac", "aac", "ogg", "mp3", "wav", "m4a", "opus"],
    ),
    ("Video", &["webm", "mkv", "mp4", "ogv", "avi", "gif"]),
    ("Image", &["png", "jpg", "ico", "webp", "avif"]),
    ("Document", &["pdf"]),
];

const AUDIO_INPUT_LIST: &[&str] = &[
    "3gp", "3gpp", "aac", "ac3", "aif", "aifc", "aiff", "amr", "ape", "au", "avi", "bik", "caf",
    "cda", "dff", "dsf", "dts", "flac", "flv", "gif", "m4a", "m4b", "m4v", "mka", "mkv", "mov",
    "mp2", "mp3", "mp4", "mpa", "mpeg", "mpg", "oga", "ogg", "ogv", "opus", "ra", "rm", "snd",
    "ts", "tta", "vob", "voc", "wav", "webm", "wma", "wmv", "wv",
];

const VIDEO_INPUT_LIST: &[&str] = &[
    "3gp", "3gpp", "avi", "bik", "flv", "gif", "m4v", "mkv", "mov", "mp4", "mpeg", "mpg", "ogv",
    "rm", "ts", "vob", "webm", "wmv",
];

const IMAGE_INPUT_LIST: &[&str] = &[
    "arw", "avif", "bmp", "cr2", "dds", "dns", "dng", "exr", "heic", "ico", "jfif", "jpeg", "jpg",
    "nef", "png", "psd", "raf", "svg", "tif", "tiff", "tga", "webp",
];

pub const OFFICE_EXTS: &[&str] = &[
    "doc", "docm", "docx", "dot", "dotm", "dotx", "odp", "ods", "odt", "otp", "ots", "ott", "pot",
    "potm", "potx", "pps", "ppsx", "ppt", "pptm", "pptx", "xls", "xlsb", "xlsm", "xlsx", "xlt",
    "xltm", "xltx",
];

fn set(list: &[&'static str]) -> HashSet<&'static str> {
    list.iter().copied().collect()
}

fn audio_inputs() -> HashSet<&'static str> {
    set(AUDIO_INPUT_LIST)
}

fn video_inputs() -> HashSet<&'static str> {
    set(VIDEO_INPUT_LIST)
}

fn image_inputs() -> HashSet<&'static str> {
    set(IMAGE_INPUT_LIST)
}

pub fn extension_of(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| {
            name.rsplit_once('.')
                .map(|(_, ext)| ext.to_ascii_lowercase())
        })
        .filter(|ext| !ext.is_empty())
}

pub fn targets_for(category: &str) -> Vec<String> {
    let mut targets: Vec<String> = category_outputs(category)
        .iter()
        .map(|s| s.to_string())
        .collect();
    match category {
        "Video" => targets.extend(category_outputs("Audio").iter().map(|s| s.to_string())),
        "Image" => targets.push("pdf".to_string()),
        "Document" => {
            targets.push("pdf".to_string());
            targets.extend(category_outputs("Image").iter().map(|s| s.to_string()));
        }
        _ => {}
    }
    targets
}

pub fn output_category(fmt: &str) -> Option<&'static str> {
    for (category, outputs) in CATEGORY_OUTPUTS {
        if outputs.contains(&fmt) {
            return Some(category);
        }
    }
    if fmt == "pdf" {
        return Some("Document");
    }
    None
}

pub fn allows_target(category: &str, target: &str) -> bool {
    targets_for(category).iter().any(|fmt| fmt == target)
}

/// Detect the category for a file, or `None` when it is unsupported.
///
/// Extensions that could be either audio or video are disambiguated with
/// ffprobe. If probing fails, shared containers are treated as video, matching
/// the original application's behavior.
pub fn detect_category(path: &Path) -> Option<&'static str> {
    let ext = extension_of(path)?;
    let audio = audio_inputs();
    let video = video_inputs();

    if audio.contains(ext.as_str()) && !video.contains(ext.as_str()) {
        return Some("Audio");
    }
    if audio.contains(ext.as_str()) && video.contains(ext.as_str()) {
        let (has_video, has_audio) = probe::probe_streams(path);
        if has_video == Some(true) {
            return Some("Video");
        }
        if has_audio == Some(true) {
            return Some("Audio");
        }
        return Some("Video");
    }
    if OFFICE_EXTS.contains(&ext.as_str()) {
        return Some("Document");
    }
    if image_inputs().contains(ext.as_str()) || ext == "pdf" {
        return Some("Image");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_audio_extensions_without_probing() {
        assert_eq!(detect_category(Path::new("tone.wav")), Some("Audio"));
        assert_eq!(detect_category(Path::new("tone.FLAC")), Some("Audio"));
        assert_eq!(detect_category(Path::new("voice.m4a")), Some("Audio"));
    }

    #[test]
    fn detects_documents_and_images() {
        assert_eq!(detect_category(Path::new("notes.odt")), Some("Document"));
        assert_eq!(detect_category(Path::new("photo.png")), Some("Image"));
        assert_eq!(detect_category(Path::new("paper.pdf")), Some("Image"));
    }

    #[test]
    fn video_accepts_audio_targets() {
        assert!(allows_target("Video", "mp3"));
        assert!(allows_target("Video", "webm"));
        assert!(!allows_target("Audio", "webm"));
        assert!(allows_target("Image", "pdf"));
    }

    #[test]
    fn output_category_is_stable() {
        assert_eq!(output_category("mp3"), Some("Audio"));
        assert_eq!(output_category("webm"), Some("Video"));
        assert_eq!(output_category("pdf"), Some("Document"));
        assert_eq!(output_category("png"), Some("Image"));
    }
}
