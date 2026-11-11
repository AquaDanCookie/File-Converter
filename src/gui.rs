//! Native desktop GUI (egui/eframe).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use eframe::egui;

use crate::engine::{self, ConversionResult, Job, ProgressFn, Status};
use crate::presets::{Options, PresetStore};
use crate::{matrix, probe};

#[derive(Clone)]
struct FileEntry {
    path: PathBuf,
    category: Option<&'static str>,
}

#[derive(Default, Clone)]
struct AutoHints {
    audio_source_bps: Option<f64>,
    audio_kbps: Option<u32>,
    video_crf: Option<u32>,
    image_quality: Option<u32>,
}

enum WorkerMsg {
    Progress(f64, String),
    Done(Vec<ConversionResult>),
}

struct PendingDialog {
    title: String,
    text: String,
    level: rfd::MessageLevel,
}

struct ConverterApp {
    store: PresetStore,
    files: Vec<FileEntry>,
    selected: usize,
    output_dir: Option<PathBuf>,

    targets: Vec<String>,
    target_cats: BTreeMap<String, Vec<&'static str>>,
    target: String,

    preset_index: usize,

    audio_bitrate: Option<u32>,
    sample_rate: Option<u32>,
    audio_max_size: String,

    rate_control_cbr: bool,
    crf_auto: bool,
    crf: u32,
    vbitrate: u32,
    preset: String,
    video_max_size: String,

    quality_auto: bool,
    quality: u32,
    scale: u32,
    image_max_size: String,

    doc_max_size: String,

    info_cache: HashMap<PathBuf, String>,
    info_text: String,
    auto_hints: AutoHints,

    busy: bool,
    progress: f32,
    status: String,
    cancel: Arc<AtomicBool>,
    receiver: Option<Receiver<WorkerMsg>>,
    dialog: Option<PendingDialog>,
}

impl ConverterApp {
    fn new(cc: &eframe::CreationContext<'_>, paths: Vec<PathBuf>) -> Self {
        cc.egui_ctx.set_theme(egui::ThemePreference::System);
        let mut app = Self {
            store: PresetStore::load(),
            files: Vec::new(),
            selected: 0,
            output_dir: None,
            targets: Vec::new(),
            target_cats: BTreeMap::new(),
            target: String::new(),
            preset_index: 0,
            audio_bitrate: None,
            sample_rate: None,
            audio_max_size: String::new(),
            rate_control_cbr: false,
            crf_auto: true,
            crf: 23,
            vbitrate: 2000,
            preset: "medium".into(),
            video_max_size: String::new(),
            quality_auto: true,
            quality: 92,
            scale: 100,
            image_max_size: String::new(),
            doc_max_size: String::new(),
            info_cache: HashMap::new(),
            info_text: String::new(),
            auto_hints: AutoHints::default(),
            busy: false,
            progress: 0.0,
            status: String::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            receiver: None,
            dialog: None,
        };
        if !paths.is_empty() {
            app.add_files(paths);
        }
        app
    }

    // ------------------------------------------------------------- files

    fn add_files(&mut self, paths: Vec<PathBuf>) {
        let already_selected = self.selected;
        let mut added = 0;
        for path in paths {
            if !path.exists() || self.files.iter().any(|entry| entry.path == path) {
                continue;
            }
            let category = matrix::detect_category(&path);
            self.files.push(FileEntry { path, category });
            added += 1;
        }
        if added == 0 {
            return;
        }
        if self.files.len() == added {
            self.selected = 0;
        } else {
            self.selected = already_selected.min(self.files.len() - 1);
        }
        self.rebuild_targets();
        self.refresh_info();
    }

    fn remove_selected(&mut self) {
        if self.selected < self.files.len() {
            self.files.remove(self.selected);
        }
        if self.files.is_empty() {
            self.selected = 0;
            self.info_text.clear();
        } else {
            self.selected = self.selected.min(self.files.len() - 1);
        }
        self.rebuild_targets();
        self.refresh_info();
    }

    fn clear_files(&mut self) {
        self.files.clear();
        self.selected = 0;
        self.info_text.clear();
        self.targets.clear();
        self.target_cats.clear();
        self.target.clear();
        self.auto_hints = AutoHints::default();
    }

    fn pick_files(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .set_title("Select files to convert")
            .set_directory(dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
            .pick_files()
        {
            self.add_files(paths);
        }
    }

    fn choose_output_dir(&self, title: &str) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new().set_title(title);
        if let Some(directory) = self
            .output_dir
            .as_ref()
            .filter(|directory| directory.is_dir())
            .cloned()
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
        {
            dialog = dialog.set_directory(directory);
        }
        dialog.pick_folder()
    }

    fn pick_output_dir(&mut self) {
        if let Some(directory) = self.choose_output_dir("Choose output folder") {
            self.output_dir = Some(directory);
        }
    }

    fn start_one_time(&mut self) {
        if let Some(directory) = self.choose_output_dir("Convert to folder") {
            self.start_with_outdir(Some(directory));
        }
    }

    fn selected_entry(&self) -> Option<&FileEntry> {
        self.files.get(self.selected)
    }

    fn first_source_for_target(&self) -> Option<&Path> {
        if self.target.is_empty() {
            return self.selected_entry().map(|entry| entry.path.as_path());
        }
        if let Some(entry) = self.selected_entry() {
            if entry
                .category
                .map(|category| matrix::allows_target(category, &self.target))
                .unwrap_or(false)
            {
                return Some(entry.path.as_path());
            }
        }
        self.files
            .iter()
            .find(|entry| {
                entry
                    .category
                    .map(|category| matrix::allows_target(category, &self.target))
                    .unwrap_or(false)
            })
            .map(|entry| entry.path.as_path())
            .or_else(|| self.selected_entry().map(|entry| entry.path.as_path()))
    }

    fn refresh_info(&mut self) {
        let Some(entry) = self.selected_entry() else {
            self.info_text.clear();
            return;
        };
        let path = entry.path.clone();
        if !self.info_cache.contains_key(&path) {
            self.info_cache
                .insert(path.clone(), engine::media_info_text(&path));
        }
        self.info_text = self.info_cache.get(&path).cloned().unwrap_or_default();
        self.refresh_auto_hints();
    }

    fn refresh_auto_hints(&mut self) {
        let source = self.first_source_for_target().map(Path::to_path_buf);
        self.auto_hints = AutoHints::default();
        let Some(path) = source else {
            return;
        };
        match self
            .option_category(&self.target)
            .to_ascii_lowercase()
            .as_str()
        {
            "audio" => {
                self.auto_hints.audio_source_bps = probe::audio_bitrate_bps(&path);
                self.auto_hints.audio_kbps = engine::auto_bitrate_kbps(&path);
            }
            "video" => {
                self.auto_hints.video_crf = Some(engine::auto_crf(&path));
            }
            "image" => {
                self.auto_hints.image_quality = Some(engine::auto_quality(&path));
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ targets

    fn option_category(&self, fmt: &str) -> String {
        let category = matrix::output_category(fmt).unwrap_or("");
        if category == "Document"
            && self
                .target_cats
                .get(fmt)
                .map(|categories| categories.iter().any(|item| *item == "Image"))
                .unwrap_or(false)
        {
            return "Image".into();
        }
        category.to_string()
    }

    fn rebuild_targets(&mut self) {
        let mut order: Vec<String> = Vec::new();
        let mut categories: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();
        for entry in &self.files {
            let Some(category) = entry.category else {
                continue;
            };
            for format in matrix::targets_for(category) {
                if !order.contains(&format) {
                    order.push(format.clone());
                }
                let list = categories.entry(format).or_default();
                if !list.contains(&category) {
                    list.push(category);
                }
            }
        }
        self.targets = order;
        self.target_cats = categories;

        let previous = self.target.clone();
        if let Some(primary) = self.files.iter().find_map(|entry| entry.category) {
            let mut chosen = self.store.default_format(primary).unwrap_or_default();
            if !self.targets.contains(&chosen) {
                chosen = self.targets.first().cloned().unwrap_or_default();
            }
            if self.targets.contains(&previous) {
                chosen = previous;
            }
            self.target = chosen;
            self.preset_index = 0;
            self.apply_defaults();
        } else {
            self.target.clear();
        }
        self.refresh_auto_hints();
    }

    fn apply_defaults(&mut self) {
        let category = self.option_category(&self.target).to_ascii_lowercase();
        let defaults = self.store.defaults_for(&category);
        self.audio_max_size = max_size_text(&defaults);
        self.video_max_size = max_size_text(&defaults);
        self.image_max_size = max_size_text(&defaults);
        self.doc_max_size = max_size_text(&defaults);

        match category.as_str() {
            "audio" => {
                self.audio_bitrate = defaults.bitrate;
                self.sample_rate = defaults.sample_rate;
            }
            "video" => {
                self.crf_auto = defaults.crf.is_none();
                self.crf = defaults.crf.unwrap_or(23).clamp(0, 51);
                self.rate_control_cbr = defaults.vbitrate.is_some();
                self.vbitrate = defaults.vbitrate.unwrap_or(2000).max(50);
                self.preset = defaults.preset.unwrap_or_else(|| "medium".into());
            }
            "image" => {
                self.quality_auto = defaults.quality.is_none();
                self.quality = defaults.quality.unwrap_or(92).clamp(1, 100);
                self.scale = defaults.scale.unwrap_or(100).clamp(25, 100);
            }
            _ => {}
        }
    }

    fn apply_preset(&mut self, index: usize) {
        if index == 0 {
            self.apply_defaults();
            self.refresh_auto_hints();
            return;
        }
        let Some(preset) = self.store.presets().get(index - 1).cloned() else {
            return;
        };
        if self.targets.iter().any(|format| format == &preset.format) {
            self.target = preset.format.clone();
        }
        self.apply_defaults();
        let category = preset.category.to_ascii_lowercase();
        let options = &preset.options;
        match category.as_str() {
            "audio" => {
                self.audio_bitrate = options.bitrate;
                self.sample_rate = options.sample_rate;
                self.audio_max_size = max_size_text(options);
            }
            "video" => {
                self.crf_auto = options.crf.is_none();
                self.crf = options.crf.unwrap_or(23).clamp(0, 51);
                self.rate_control_cbr = options.vbitrate.is_some();
                self.vbitrate = options.vbitrate.unwrap_or(2000).max(50);
                if let Some(name) = &options.preset {
                    self.preset = name.clone();
                }
                self.video_max_size = max_size_text(options);
            }
            "image" => {
                self.quality_auto = options.quality.is_none();
                self.quality = options.quality.unwrap_or(92).clamp(1, 100);
                self.scale = options.scale.unwrap_or(100).clamp(25, 100);
                self.image_max_size = max_size_text(options);
            }
            "document" => {
                self.doc_max_size = max_size_text(options);
            }
            _ => {}
        }
        self.preset_index = index;
        self.refresh_auto_hints();
    }

    // ------------------------------------------------------------ options

    fn collect_options(&self) -> Options {
        let category = self.option_category(&self.target).to_ascii_lowercase();
        let mut options = Options::default();
        match category.as_str() {
            "audio" => {
                options.bitrate = self.audio_bitrate;
                options.sample_rate = self.sample_rate;
                options.max_size = parse_max_size(&self.audio_max_size);
            }
            "video" => {
                options.preset = Some(self.preset.clone());
                if self.rate_control_cbr {
                    options.vbitrate = Some(self.vbitrate.max(50));
                } else if !self.crf_auto {
                    options.crf = Some(self.crf.min(51));
                }
                options.max_size = parse_max_size(&self.video_max_size);
            }
            "image" => {
                options.scale = Some(self.scale.clamp(25, 100));
                if !self.quality_auto {
                    options.quality = Some(self.quality.clamp(1, 100));
                }
                options.max_size = parse_max_size(&self.image_max_size);
            }
            "document" => {
                options.max_size = parse_max_size(&self.doc_max_size);
            }
            _ => {}
        }
        options
    }

    // ------------------------------------------------------------- actions

    fn start(&mut self) {
        self.start_with_outdir(None);
    }

    fn start_with_outdir(&mut self, one_time_outdir: Option<PathBuf>) {
        if self.target.is_empty() {
            self.dialog = Some(PendingDialog {
                title: "File Converter".into(),
                text: "No convertible files. Add audio, video, image or office files.".into(),
                level: rfd::MessageLevel::Info,
            });
            return;
        }

        let outdir = one_time_outdir.or_else(|| self.output_dir.clone());
        if let Some(directory) = &outdir {
            if !directory.is_dir() {
                self.dialog = Some(PendingDialog {
                    title: "File Converter".into(),
                    text: format!(
                        "The output folder no longer exists:\n{}",
                        directory.display()
                    ),
                    level: rfd::MessageLevel::Warning,
                });
                return;
            }
        }

        let target = self.target.clone();
        let options = self.collect_options();
        let mut jobs = Vec::new();
        let mut skipped = Vec::new();
        for entry in &self.files {
            let name = entry
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            match entry.category {
                None => skipped.push(format!("{name}: unsupported input format")),
                Some(category) if !matrix::allows_target(category, &target) => {
                    skipped.push(format!("{name}: not convertible to {target}"));
                }
                Some(category) => jobs.push(Job {
                    source: entry.path.clone(),
                    target: target.clone(),
                    category: category.to_string(),
                    options: options.clone(),
                    outdir: outdir.clone(),
                }),
            }
        }

        if jobs.is_empty() {
            self.dialog = Some(PendingDialog {
                title: "File Converter".into(),
                text: format!(
                    "No files can be converted to {target}.\n\n{}",
                    skipped.join("\n")
                ),
                level: rfd::MessageLevel::Warning,
            });
            return;
        }

        let missing = engine::dependency_report(&jobs);
        if !missing.is_empty() {
            self.dialog = Some(PendingDialog {
                title: "Missing dependencies".into(),
                text: format!(
                    "These packages are required for the selected conversion:\n\n{}\n\nInstall them with:\n  {}",
                    missing
                        .iter()
                        .map(|package| format!("  - {package}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    engine::PACMAN_HINT
                ),
                level: rfd::MessageLevel::Warning,
            });
            return;
        }

        self.busy = true;
        self.progress = 0.0;
        self.status = "Starting...".into();
        self.cancel.store(false, Ordering::SeqCst);

        let (sender, receiver) = mpsc::channel();
        let progress_sender = sender.clone();
        let cancel = Arc::clone(&self.cancel);
        let progress: Arc<ProgressFn> = Arc::new(move |pct, message| {
            let _ = progress_sender.send(WorkerMsg::Progress(pct, message));
        });
        thread::spawn(move || {
            let results = engine::run_batch(jobs, cancel, progress, 2);
            let _ = sender.send(WorkerMsg::Done(results));
        });
        self.receiver = Some(receiver);
    }

    fn stop(&mut self) {
        if self.busy {
            self.cancel.store(true, Ordering::SeqCst);
            self.status = "Cancelling...".into();
        }
    }

    fn poll_worker(&mut self, ctx: &egui::Context) {
        let mut messages = Vec::new();
        if let Some(receiver) = &self.receiver {
            loop {
                match receiver.try_recv() {
                    Ok(message) => messages.push(message),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break,
                }
            }
        }
        let mut finished = None;
        for message in messages {
            match message {
                WorkerMsg::Progress(pct, text) => {
                    self.progress = pct as f32;
                    self.status = text;
                }
                WorkerMsg::Done(results) => finished = Some(results),
            }
        }
        if let Some(results) = finished {
            self.receiver = None;
            self.finish(results);
        }
        if self.busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn finish(&mut self, results: Vec<ConversionResult>) {
        self.busy = false;
        self.progress = 100.0;
        self.status = "Finished.".into();

        let mut ok = Vec::new();
        let mut skipped = Vec::new();
        let mut failed = Vec::new();
        for result in results {
            let source = result
                .job
                .source
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            match result.status {
                Status::Ok => {
                    let output = result
                        .output
                        .as_ref()
                        .and_then(|path| path.file_name())
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    ok.push(format!(
                        "{source} -> {output}{}",
                        engine::quality_note(&result.job)
                    ));
                }
                Status::Skipped => skipped.push(format!("{source}: {}", result.message)),
                Status::Cancelled => failed.push(format!("{source}: cancelled")),
                Status::Error => failed.push(format!(
                    "{source}: {}",
                    if result.message.is_empty() {
                        "failed"
                    } else {
                        &result.message
                    }
                )),
            }
        }

        let mut lines = Vec::new();
        if !ok.is_empty() {
            lines.push(format!("Converted ({}):", ok.len()));
            lines.extend(ok.iter().map(|line| format!("  - {line}")));
        }
        if !skipped.is_empty() {
            lines.push(format!("Skipped ({}):", skipped.len()));
            lines.extend(skipped.iter().map(|line| format!("  - {line}")));
        }
        if !failed.is_empty() {
            lines.push(format!("Failed ({}):", failed.len()));
            lines.extend(failed.iter().map(|line| format!("  - {line}")));
        }
        if lines.is_empty() {
            lines.push("Nothing was converted.".into());
        }
        self.dialog = Some(PendingDialog {
            title: "File Converter - finished".into(),
            text: lines.join("\n"),
            level: if failed.is_empty() {
                rfd::MessageLevel::Info
            } else {
                rfd::MessageLevel::Warning
            },
        });
    }

    // ------------------------------------------------------------------ UI

    fn show_files(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("files_panel")
            .resizable(true)
            .default_height(180.0)
            .min_height(100.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.heading("Files");
                    ui.add_space(8.0);
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Add files…"))
                        .clicked()
                    {
                        self.pick_files();
                    }
                    if ui
                        .add_enabled(
                            !self.busy && !self.files.is_empty(),
                            egui::Button::new("Remove"),
                        )
                        .clicked()
                    {
                        self.remove_selected();
                    }
                    if ui
                        .add_enabled(
                            !self.busy && !self.files.is_empty(),
                            egui::Button::new("Clear"),
                        )
                        .clicked()
                    {
                        self.clear_files();
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.files.is_empty() {
                            ui.weak(
                                "Drop files here or use 'Add files…'. Supported: audio, \
                                 video, images, PDF and Office documents.",
                            );
                        }
                        let files = self.files.clone();
                        let mut clicked = None;
                        for (index, entry) in files.iter().enumerate() {
                            let name = entry.path.file_name().unwrap_or_default().to_string_lossy();
                            let label =
                                format!("{name}    [{}]", entry.category.unwrap_or("unsupported"));
                            let response = ui.selectable_label(self.selected == index, label);
                            if response.clicked() {
                                clicked = Some(index);
                            }
                            if entry.category.is_none() {
                                response.on_hover_text("Unsupported input format");
                            }
                        }
                        if let Some(index) = clicked {
                            self.selected = index;
                            self.refresh_info();
                        }
                    });
            });
    }

    fn show_options(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("options_panel")
            .resizable(true)
            .default_height(340.0)
            .min_height(180.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                let presets = self.store.presets().to_vec();
                let selected_name = if self.preset_index == 0 {
                    "Defaults".to_string()
                } else {
                    presets
                        .get(self.preset_index - 1)
                        .map(|preset| preset.name.clone())
                        .unwrap_or_else(|| "Defaults".into())
                };
                let mut preset_index = self.preset_index;
                ui.horizontal(|ui| {
                    ui.label("Presets:");
                    egui::ComboBox::from_id_salt("preset_combo")
                        .selected_text(selected_name)
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut preset_index, 0, "Defaults");
                            for (index, preset) in presets.iter().enumerate() {
                                ui.selectable_value(
                                    &mut preset_index,
                                    index + 1,
                                    &preset.name,
                                );
                            }
                        });
                    if ui.button("Reload").clicked() {
                        self.store.reload();
                        self.preset_index = 0;
                        self.rebuild_targets();
                        if let Some(error) = self.store.error.clone() {
                            self.dialog = Some(PendingDialog {
                                title: "Presets".into(),
                                text: format!(
                                    "Could not read presets.json:\n{error}\nUsing built-in defaults."
                                ),
                                level: rfd::MessageLevel::Warning,
                            });
                        }
                    }
                });
                if preset_index != self.preset_index {
                    self.preset_index = preset_index;
                    self.apply_preset(preset_index);
                }

                let targets = self.targets.clone();
                let mut target = self.target.clone();
                ui.horizontal(|ui| {
                    ui.label("Convert to:");
                    egui::ComboBox::from_id_salt("target_combo")
                        .selected_text(target.to_ascii_uppercase())
                        .width(180.0)
                        .show_ui(ui, |ui| {
                            for format in &targets {
                                ui.selectable_value(
                                    &mut target,
                                    format.clone(),
                                    format.to_ascii_uppercase(),
                                );
                            }
                        });
                });
                if target != self.target && !target.is_empty() {
                    self.target = target;
                    self.preset_index = 0;
                    self.apply_defaults();
                    self.refresh_auto_hints();
                }

                ui.separator();
                let category = self.option_category(&self.target).to_ascii_lowercase();
                ui.add_enabled_ui(!self.busy, |ui| match category.as_str() {
                    "audio" => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Bitrate:");
                            let mut bitrate = self.audio_bitrate;
                            egui::ComboBox::from_id_salt("bitrate_combo")
                                .selected_text(match bitrate {
                                    None => "Auto (match source)".to_string(),
                                    Some(rate) => format!("{rate} kbps"),
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut bitrate,
                                        None,
                                        "Auto (match source)",
                                    );
                                    for rate in engine::AUDIO_BITRATE_OPTIONS {
                                        ui.selectable_value(
                                            &mut bitrate,
                                            Some(*rate),
                                            format!("{rate} kbps"),
                                        );
                                    }
                                });
                            self.audio_bitrate = bitrate;

                            ui.add_space(12.0);
                            ui.label("Sample Rate:");
                            let mut sample_rate = self.sample_rate;
                            egui::ComboBox::from_id_salt("sample_rate_combo")
                                .selected_text(match sample_rate {
                                    None => "Keep Source".to_string(),
                                    Some(hz) => format!(
                                        "{} kHz",
                                        engine::format_number(hz as f64 / 1000.0)
                                    ),
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut sample_rate,
                                        None,
                                        "Keep Source",
                                    );
                                    for hz in engine::AUDIO_SAMPLE_RATES {
                                        ui.selectable_value(
                                            &mut sample_rate,
                                            Some(*hz),
                                            format!(
                                                "{} kHz",
                                                engine::format_number(*hz as f64 / 1000.0)
                                            ),
                                        );
                                    }
                                });
                            self.sample_rate = sample_rate;
                        });
                        if self.audio_bitrate.is_none() {
                            let hint = match (
                                self.auto_hints.audio_kbps,
                                self.auto_hints.audio_source_bps,
                            ) {
                                (Some(target), Some(source)) => format!(
                                    "Auto -> {target} kbps (source {:.0} kbps)",
                                    source / 1000.0
                                ),
                                (Some(target), None) => {
                                    format!("Auto -> {target} kbps")
                                }
                                _ => {
                                    "Auto -> 192 kbps (source bitrate unavailable)".into()
                                }
                            };
                            ui.weak(hint);
                        }
                        max_size_row(ui, &mut self.audio_max_size);
                    }
                    "video" => {
                        let mut cbr = self.rate_control_cbr;
                        ui.horizontal(|ui| {
                            ui.label("Rate Control:");
                            ui.selectable_value(&mut cbr, false, "Adaptive (CRF)");
                            ui.selectable_value(&mut cbr, true, "Constant Bitrate");
                        });
                        self.rate_control_cbr = cbr;
                        if cbr {
                            ui.horizontal(|ui| {
                                ui.label("Video Bitrate:");
                                ui.add(
                                    egui::DragValue::new(&mut self.vbitrate)
                                        .range(50..=100_000)
                                        .suffix(" kbps"),
                                );
                            });
                        } else {
                            let mut auto = self.crf_auto;
                            ui.checkbox(&mut auto, "Auto (match source)");
                            self.crf_auto = auto;
                            ui.add_enabled_ui(!self.crf_auto, |ui| {
                                ui.add(
                                    egui::Slider::new(&mut self.crf, 0..=51).text("CRF"),
                                );
                            });
                            if self.crf_auto {
                                let hint = self
                                    .auto_hints
                                    .video_crf
                                    .map(|crf| format!("Auto -> CRF {crf}"))
                                    .unwrap_or_else(|| "Auto -> CRF 23".into());
                                ui.weak(hint);
                            }
                        }
                        ui.horizontal(|ui| {
                            ui.label("Preset:");
                            let mut preset = self.preset.clone();
                            egui::ComboBox::from_id_salt("video_preset_combo")
                                .selected_text(preset.clone())
                                .show_ui(ui, |ui| {
                                    for name in engine::VIDEO_PRESETS {
                                        ui.selectable_value(
                                            &mut preset,
                                            (*name).to_string(),
                                            *name,
                                        );
                                    }
                                });
                            self.preset = preset;
                        });
                        max_size_row(ui, &mut self.video_max_size);
                    }
                    "image" => {
                        ui.add(
                            egui::Slider::new(&mut self.scale, 25..=100)
                                .text("Scale %"),
                        );
                        let mut auto = self.quality_auto;
                        ui.checkbox(&mut auto, "Auto (match source)");
                        self.quality_auto = auto;
                        ui.add_enabled_ui(!self.quality_auto, |ui| {
                            ui.add(
                                egui::Slider::new(&mut self.quality, 1..=100)
                                    .text("Quality"),
                            );
                        });
                        if self.quality_auto {
                            let hint = self
                                .auto_hints
                                .image_quality
                                .map(|quality| format!("Auto -> quality {quality}"))
                                .unwrap_or_else(|| "Auto -> quality 95".into());
                            ui.weak(hint);
                        }
                        max_size_row(ui, &mut self.image_max_size);
                    }
                    "document" => {
                        max_size_row(ui, &mut self.doc_max_size);
                    }
                    _ => {
                        ui.weak("Add supported files to choose a target format.");
                    }
                });

                let hint = match category.as_str() {
                    "audio" => {
                        "Auto matches the source quality. Max File Size is \
                         enforced for lossy targets; FLAC/WAV cannot be shrunk."
                    }
                    "video" => {
                        "Adaptive (CRF) keeps quality constant; constant \
                         bitrate caps the size."
                    }
                    "image" => {
                        "Scale 100% keeps the original size. With a Max File \
                         Size set, quality is lowered first, then the scale."
                    }
                    "document" => {
                        "With a Max File Size set, the PDF is re-compressed \
                         until it fits."
                    }
                    _ => "",
                };
                if !hint.is_empty() {
                    ui.add_space(2.0);
                    ui.weak(hint);
                }
            });
    }

    fn show_media(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("media_panel")
            .resizable(true)
            .default_height(170.0)
            .min_height(70.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.heading("Media Info");
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.info_text.is_empty() {
                            ui.weak("Select a file in the list to see its media information.");
                        } else {
                            ui.add(
                                egui::Label::new(egui::RichText::new(&self.info_text).monospace())
                                    .wrap(),
                            );
                        }
                    });
            });
    }

    fn show_footer(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("footer_panel").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.add(egui::ProgressBar::new(self.progress / 100.0).show_percentage());
            if !self.status.is_empty() {
                ui.weak(&self.status);
            }
            ui.horizontal(|ui| {
                ui.label("Save to:");
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Output folder..."))
                    .on_hover_text("Choose where converted files are written")
                    .clicked()
                {
                    self.pick_output_dir();
                }
                if self.output_dir.is_some()
                    && ui
                        .add_enabled(!self.busy, egui::Button::new("Reset"))
                        .on_hover_text("Save next to each source file")
                        .clicked()
                {
                    self.output_dir = None;
                }
                let destination = self
                    .output_dir
                    .as_deref()
                    .map(display_dir)
                    .unwrap_or_else(|| "Same folder as source".into());
                let response = ui.add(
                    egui::Label::new(egui::RichText::new(destination.as_str()).monospace())
                        .truncate(),
                );
                if self.output_dir.is_some() {
                    response.on_hover_text(destination);
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.busy && !self.targets.is_empty(),
                        egui::Button::new("Convert"),
                    )
                    .clicked()
                {
                    self.start();
                }
                if ui
                    .add_enabled(
                        !self.busy && !self.targets.is_empty(),
                        egui::Button::new("Convert to folder..."),
                    )
                    .on_hover_text(
                        "Choose a folder and convert immediately; does not change Save to",
                    )
                    .clicked()
                {
                    self.start_one_time();
                }
                if ui
                    .add_enabled(self.busy, egui::Button::new("Cancel"))
                    .clicked()
                {
                    self.stop();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
            ui.add_space(4.0);
        });
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.add_files(dropped);
        }
    }

    fn flush_dialog(&mut self) {
        if let Some(dialog) = self.dialog.take() {
            rfd::MessageDialog::new()
                .set_title(dialog.title)
                .set_description(dialog.text)
                .set_level(dialog.level)
                .show();
        }
    }
}

impl eframe::App for ConverterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker(ctx);
        self.handle_dropped_files(ctx);
        self.show_files(ctx);
        self.show_options(ctx);
        self.show_media(ctx);
        self.show_footer(ctx);
        egui::CentralPanel::default().show(ctx, |_ui| {});
        self.flush_dialog();
    }
}

fn display_dir(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".into();
            }
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

fn max_size_row(ui: &mut egui::Ui, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label("Max File Size:");
        ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(110.0)
                .hint_text("no limit"),
        );
        ui.label("MB");
    });
}

fn max_size_text(options: &Options) -> String {
    let Some(value) = options.max_size else {
        return String::new();
    };
    if value >= 1000.0 && (value / 1000.0).fract().abs() < 1e-9 {
        format!("{} GB", (value / 1000.0) as i64)
    } else {
        engine::format_number(value)
    }
}

fn parse_max_size(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let multiplier = if lower.contains("gib") {
        1024.0_f64.powi(3)
    } else if lower.contains("mib") {
        1024.0_f64.powi(2)
    } else if lower.contains("gb") {
        1000.0_f64.powi(3)
    } else if lower.contains("mb") {
        1000.0_f64.powi(2)
    } else {
        1000.0_f64.powi(2)
    };
    let digits: String = text
        .chars()
        .filter(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == ',')
        .collect();
    let value: f64 = digits.replace(',', ".").parse().ok()?;
    if value <= 0.0 {
        return None;
    }
    Some(value * multiplier / 1_000_000.0)
}

pub fn run_gui(paths: Vec<PathBuf>) -> eframe::Result<()> {
    let icon =
        eframe::icon_data::from_png_bytes(include_bytes!("../assets/fileconverter-256.png")).ok();
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([700.0, 780.0])
        .with_min_inner_size([540.0, 560.0])
        .with_title("File Converter")
        .with_app_id("fileconverter");
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "fileconverter",
        options,
        Box::new(move |cc| Ok(Box::new(ConverterApp::new(cc, paths)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_decimal_and_binary_sizes() {
        assert_eq!(parse_max_size("20"), Some(20.0));
        assert_eq!(parse_max_size("500 MB"), Some(500.0));
        assert_eq!(parse_max_size("1.5 GB"), Some(1500.0));
        assert!((parse_max_size("1024 MiB").unwrap() - 1073.741824).abs() < 1e-6);
        assert_eq!(parse_max_size(""), None);
        assert_eq!(parse_max_size("0"), None);
    }

    #[test]
    fn max_size_text_uses_gb_for_exact_multiples() {
        let options = Options {
            max_size: Some(2000.0),
            ..Options::default()
        };
        assert_eq!(max_size_text(&options), "2 GB");
    }
}
