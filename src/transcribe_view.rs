// The Audio to text screen: drop voice notes, recordings or videos; the text is shown here and
// saved next to each one as name.txt.

use crate::jobs::{self, FileJobs, Work};
use crate::transcribe::{self, Quality};
use crate::{config, theme};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    quality: Quality,
}

fn settings_file() -> PathBuf {
    config::dir().join("transcribe.json")
}

/// A file's name and its text.
type Transcript = (String, String);

/// The file being listened to and how far it got (0..1).
type Progress = Arc<Mutex<Option<(String, f32)>>>;

pub struct TranscribePage {
    settings: Settings,
    jobs: Option<FileJobs>,
    picked: (Sender<Vec<PathBuf>>, Receiver<Vec<PathBuf>>),
    progress: Progress,
    texts: (Sender<Transcript>, Receiver<Transcript>),
    /// The last text read and the file it came from.
    last: Option<(String, String)>,
}

impl Default for TranscribePage {
    fn default() -> Self {
        TranscribePage {
            settings: std::fs::read(settings_file())
                .ok()
                .and_then(|raw| serde_json::from_slice(&raw).ok())
                .unwrap_or_default(),
            jobs: None,
            picked: channel(),
            progress: Default::default(),
            texts: channel(),
            last: None,
        }
    }
}

impl TranscribePage {
    fn add(&mut self, files: &[PathBuf], ctx: &egui::Context) {
        let quality = self.settings.quality;
        let progress = self.progress.clone();
        let texts = self.texts.0.clone();
        let repaint = ctx.clone();
        let work: Work = Arc::new(move |file| {
            let name = file.file_name().unwrap_or_default().to_string_lossy().to_string();
            let report = |p: f32| {
                *progress.lock().unwrap() = Some((name.clone(), p));
                repaint.request_repaint();
            };
            let result = transcribe::text_of(file, quality, &report);
            *progress.lock().unwrap() = None;
            let text = result?;
            let out = transcribe::output_path(file);
            std::fs::write(&out, format!("{text}\n")).map_err(|e| e.to_string())?;
            let _ = texts.send((name, text));
            Ok((out, String::new()))
        });
        let jobs = self.jobs.get_or_insert_with(|| FileJobs::new(ctx));
        for file in jobs::collect(files, transcribe::supported) {
            jobs.add(file, work.clone());
        }
    }
}

impl crate::Page for TranscribePage {
    fn add_files(&mut self, files: &[PathBuf]) {
        // Files from the right-click menu arrive before the screen draws: they wait for it.
        let _ = self.picked.0.send(files.to_vec());
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Some(jobs) = &mut self.jobs {
            jobs.poll();
        }
        let (dropped, hovering) = theme::dropped_files(&ctx);
        if !dropped.is_empty() {
            self.add(&dropped, &ctx);
        }
        while let Ok(files) = self.picked.1.try_recv() {
            self.add(&files, &ctx);
        }
        while let Ok(text) = self.texts.1.try_recv() {
            self.last = Some(text);
        }

        theme::page_header(
            ui,
            "Audio to text",
            "Voice notes, recordings and videos to text, in almost any language. Saved next to each one as name.txt. Runs on your computer, nothing is uploaded.",
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let before = self.settings.quality;
            ui.radio_value(&mut self.settings.quality, Quality::Fast, "Fast")
                .on_hover_text("Quicker, with more mistakes. Downloads 77 MB the first time.");
            ui.radio_value(&mut self.settings.quality, Quality::Best, "Best quality")
                .on_hover_text("Far fewer mistakes, a little slower. Downloads 250 MB the first time.");
            if self.settings.quality != before {
                let _ = config::write_json(&settings_file(), &self.settings);
            }
        });
        ui.add_space(10.0);
        if theme::drop_zone(
            ui,
            hovering,
            "Drop audio or video files here",
            "or click to choose · WhatsApp voice notes, MP3, M4A, OGG, WAV, FLAC, MP4, MKV, WebM · folders too",
        )
        .clicked()
        {
            jobs::pick_files(
                &ctx,
                self.picked.0.clone(),
                "Audio to text",
                ("Audio and video", transcribe::EXTS),
            );
        }
        ui.add_space(10.0);

        if let Some((name, done)) = self.progress.lock().unwrap().clone() {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0).color(theme::ACCENT));
                let what = if done == 0.0 { "Preparing" } else { "Listening to" };
                ui.add(egui::Label::new(format!("{what} {name}")).truncate());
            });
            ui.add(egui::ProgressBar::new(done).show_percentage());
            ui.add_space(8.0);
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some((name, text)) = &self.last {
                    theme::card().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(egui::RichText::new(name).strong()).truncate());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Copy").clicked() {
                                    ctx.copy_text(text.clone());
                                }
                            });
                        });
                        ui.add_space(6.0);
                        let mut shown = text.as_str();
                        ui.add(
                            egui::TextEdit::multiline(&mut shown)
                                .desired_width(f32::INFINITY)
                                .desired_rows(6),
                        );
                    });
                    ui.add_space(10.0);
                }
                if let Some(jobs) = &mut self.jobs {
                    jobs.ui(ui);
                    if !jobs.is_empty() && ui.button("Clear list").clicked() {
                        jobs.clear();
                    }
                }
            });
    }
}
