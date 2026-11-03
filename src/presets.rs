//! Preset management.
//!
//! Presets live in `~/.config/file-converter/presets.json` and are re-read
//! when the GUI/CLI starts (or when the GUI's Reload button is pressed).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::matrix;

/// Quality options shared by defaults and presets.
///
/// `None` means *auto*: choose the value that best matches the source file
/// when the conversion actually runs. A number locks the value.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Options {
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub crf: Option<u32>,
    pub vbitrate: Option<u32>,
    pub preset: Option<String>,
    pub scale: Option<u32>,
    pub quality: Option<u32>,
    pub max_size: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CategoryDefaults {
    pub format: Option<String>,
    #[serde(flatten)]
    pub options: Options,
}

impl Default for CategoryDefaults {
    fn default() -> Self {
        Self {
            format: None,
            options: Options::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    pub audio: CategoryDefaults,
    pub video: CategoryDefaults,
    pub image: CategoryDefaults,
    pub document: CategoryDefaults,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            audio: CategoryDefaults {
                format: Some("mp3".into()),
                options: Options::default(),
            },
            video: CategoryDefaults {
                format: Some("webm".into()),
                options: Options {
                    preset: Some("medium".into()),
                    ..Options::default()
                },
            },
            image: CategoryDefaults {
                format: Some("png".into()),
                options: Options {
                    scale: Some(100),
                    ..Options::default()
                },
            },
            document: CategoryDefaults {
                format: Some("pdf".into()),
                options: Options::default(),
            },
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Preset {
    pub name: String,
    pub category: String,
    pub format: String,
    pub options: Options,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PresetFile {
    #[serde(rename = "__version__")]
    pub version: u32,
    pub defaults: Defaults,
    pub presets: Vec<Preset>,
}

impl Default for PresetFile {
    fn default() -> Self {
        Self {
            version: 2,
            defaults: Defaults::default(),
            presets: shipped_presets(),
        }
    }
}

fn shipped_presets() -> Vec<Preset> {
    vec![
        Preset {
            name: "MP3 192 kbps".into(),
            category: "audio".into(),
            format: "mp3".into(),
            options: Options {
                bitrate: Some(192),
                ..Options::default()
            },
            description: Some(
                "Everyday music and podcasts. A good balance of file size and \
                 sound quality for most people."
                    .into(),
            ),
        },
        Preset {
            name: "WebM, CRF 30 (small)".into(),
            category: "video".into(),
            format: "webm".into(),
            options: Options {
                crf: Some(30),
                preset: Some("faster".into()),
                ..Options::default()
            },
            description: Some(
                "Small WebM videos (VP9) for quick sharing and chatting. \
                 Accepts more compression artifacts."
                    .into(),
            ),
        },
        Preset {
            name: "JPEG 80%".into(),
            category: "image".into(),
            format: "jpg".into(),
            options: Options {
                scale: Some(100),
                quality: Some(80),
                ..Options::default()
            },
            description: Some(
                "Everyday photos for email and the web. High quality with a \
                 noticeably smaller file than the original."
                    .into(),
            ),
        },
    ]
}

pub fn presets_file() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("file-converter")
        .join("presets.json")
}

fn builtin_format(category: &str) -> Option<&'static str> {
    match category {
        "Audio" => Some("mp3"),
        "Video" => Some("webm"),
        "Image" => Some("png"),
        "Document" => Some("pdf"),
        _ => None,
    }
}

fn ensure_defaults(data: &mut PresetFile) {
    let fallbacks = Defaults::default();
    for (category, current, fallback) in [
        ("Audio", &mut data.defaults.audio, &fallbacks.audio),
        ("Video", &mut data.defaults.video, &fallbacks.video),
        ("Image", &mut data.defaults.image, &fallbacks.image),
        ("Document", &mut data.defaults.document, &fallbacks.document),
    ] {
        if current.format.is_none() {
            current.format = Some(
                fallback
                    .format
                    .clone()
                    .or_else(|| builtin_format(category).map(str::to_string))
                    .unwrap_or_default(),
            );
        }
    }
    if data.defaults.video.options.preset.is_none() {
        data.defaults.video.options.preset = Some("medium".into());
    }
    if data.defaults.image.options.scale.is_none() {
        data.defaults.image.options.scale = Some(100);
    }
}

fn migrate(data: &mut PresetFile) -> bool {
    if data.version >= 2 {
        return false;
    }
    let mut changed = data.version != 2;
    if data.defaults.audio.options.bitrate == Some(192) {
        data.defaults.audio.options.bitrate = None;
        changed = true;
    }
    if data.defaults.video.options.crf == Some(23) {
        data.defaults.video.options.crf = None;
        changed = true;
    }
    if data.defaults.image.options.quality == Some(92) {
        data.defaults.image.options.quality = None;
        changed = true;
    }
    data.version = 2;
    changed
}

pub struct PresetStore {
    pub path: PathBuf,
    pub data: PresetFile,
    pub error: Option<String>,
}

impl PresetStore {
    pub fn load() -> Self {
        Self::from_path(presets_file())
    }

    pub fn from_path(path: PathBuf) -> Self {
        if !path.exists() {
            write_defaults(&path);
        }
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<PresetFile>(&text) {
                Ok(mut data) => {
                    let changed = migrate(&mut data);
                    ensure_defaults(&mut data);
                    if changed {
                        let _ = persist(&path, &data);
                    }
                    Self {
                        path,
                        data,
                        error: None,
                    }
                }
                Err(err) => Self {
                    path,
                    data: PresetFile::default(),
                    error: Some(err.to_string()),
                },
            },
            Err(err) => Self {
                path,
                data: PresetFile::default(),
                error: Some(err.to_string()),
            },
        }
    }

    pub fn reload(&mut self) {
        *self = Self::from_path(self.path.clone());
    }

    pub fn defaults_for(&self, category: &str) -> Options {
        let defaults = match category.to_ascii_lowercase().as_str() {
            "audio" => &self.data.defaults.audio,
            "video" => &self.data.defaults.video,
            "image" => &self.data.defaults.image,
            "document" => &self.data.defaults.document,
            _ => return Options::default(),
        };
        defaults.options.clone()
    }

    pub fn format_for(&self, category: &str) -> Option<String> {
        let defaults = match category.to_ascii_lowercase().as_str() {
            "audio" => &self.data.defaults.audio,
            "video" => &self.data.defaults.video,
            "image" => &self.data.defaults.image,
            "document" => &self.data.defaults.document,
            _ => return None,
        };
        defaults.format.clone()
    }

    pub fn default_format(&self, category: &str) -> Option<String> {
        let stored = self.format_for(category);
        if let Some(format) = stored {
            if matrix::category_outputs(category).contains(&format.as_str()) {
                return Some(format);
            }
        }
        builtin_format(category).map(str::to_string)
    }

    pub fn presets(&self) -> &[Preset] {
        &self.data.presets
    }
}

pub fn write_defaults(path: &Path) -> Option<String> {
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            return Some(err.to_string());
        }
    }
    persist(path, &PresetFile::default())
}

fn persist(path: &Path, data: &PresetFile) -> Option<String> {
    let text = match serde_json::to_string_pretty(data) {
        Ok(text) => text + "\n",
        Err(err) => return Some(err.to_string()),
    };
    fs::write(path, text).err().map(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_auto_quality() {
        let data = PresetFile::default();
        assert_eq!(data.version, 2);
        assert_eq!(data.defaults.audio.options.bitrate, None);
        assert_eq!(data.defaults.video.options.crf, None);
        assert_eq!(data.defaults.image.options.quality, None);
        assert_eq!(
            data.defaults.video.options.preset.as_deref(),
            Some("medium")
        );
    }

    #[test]
    fn migrates_old_shipped_values_to_auto() {
        let mut data = PresetFile::default();
        data.version = 1;
        data.defaults.audio.options.bitrate = Some(192);
        data.defaults.video.options.crf = Some(23);
        data.defaults.image.options.quality = Some(92);
        assert!(migrate(&mut data));
        assert_eq!(data.version, 2);
        assert_eq!(data.defaults.audio.options.bitrate, None);
        assert_eq!(data.defaults.video.options.crf, None);
        assert_eq!(data.defaults.image.options.quality, None);
    }

    #[test]
    fn user_values_survive_migration() {
        let mut data = PresetFile::default();
        data.version = 1;
        data.defaults.audio.options.bitrate = Some(320);
        migrate(&mut data);
        assert_eq!(data.defaults.audio.options.bitrate, Some(320));
    }

    #[test]
    fn default_file_round_trips_through_json() {
        let file = PresetFile::default();
        let text = serde_json::to_string_pretty(&file).unwrap();
        let parsed: PresetFile = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.defaults.audio.options.bitrate, None);
        assert_eq!(parsed.defaults.video.format.as_deref(), Some("webm"));
        assert_eq!(parsed.defaults.image.options.scale, Some(100));
        assert_eq!(parsed.presets.len(), 3);
    }

    #[test]
    fn format_categories_are_known() {
        assert_eq!(matrix::output_category("wav"), Some("Audio"));
        assert_eq!(matrix::output_category("mp4"), Some("Video"));
        assert_eq!(matrix::output_category("jpg"), Some("Image"));
        assert_eq!(matrix::output_category("pdf"), Some("Document"));
    }
}
