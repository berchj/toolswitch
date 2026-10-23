// The Compress tab: drop files or folders (or pick them), and each one is compressed in a
// background thread. Each result says how much smaller it got.

use crate::compress::{self, Format, Settings};
use crate::shortcut_editor::ShortcutEditor;
use crate::theme;
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

enum Status {
    Waiting,
    Working,
    Done(compress::Outcome),
    Failed(String),
}

struct Job {
    /// Never reused, so updates find their job even after Clear list.
    id: usize,
    file: PathBuf,
    status: Status,
}

pub struct CompressView {
    settings: Settings,
    jobs: Vec<Job>,
    next_id: usize,
    work: Sender<(usize, PathBuf, Settings)>,
    updates: Receiver<(usize, Status)>,
    picked: (Sender<Vec<PathBuf>>, Receiver<Vec<PathBuf>>),
    show_shortcuts: bool,
    pub shortcuts: ShortcutEditor,
}

impl CompressView {
    pub fn new(ctx: &egui::Context) -> Self {
        let (work, queue) = channel::<(usize, PathBuf, Settings)>();
        let (send_update, updates) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for (i, file, settings) in queue {
                let _ = send_update.send((i, Status::Working));
                ctx.request_repaint();
                // A broken file can make a library give up (panic); it fails alone, the rest go on.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    compress::compress(&file, &settings)
                }));
                let status = match outcome {
                    Ok(Ok(outcome)) => Status::Done(outcome),
                    Ok(Err(error)) => Status::Failed(error),
                    Err(_) => Status::Failed("the file looks damaged".into()),
                };
                let _ = send_update.send((i, status));
                ctx.request_repaint();
            }
        });
        CompressView {
            settings: compress::load(),
            jobs: vec![],
            next_id: 0,
            work,
            updates,
            picked: channel(),
            show_shortcuts: false,
            shortcuts: ShortcutEditor::default(),
        }
    }

    /// Files from a drop, the file chooser, the right-click menu or the command line.
    pub fn add(&mut self, paths: &[PathBuf]) {
        for file in compress::collect(paths, &self.settings.suffix) {
            let id = self.next_id;
            self.next_id += 1;
            let _ = self.work.send((id, file.clone(), self.settings.clone()));
            self.jobs.push(Job {
                id,
                file,
                status: Status::Waiting,
            });
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.shortcuts.poll(&ctx);
        while let Ok((id, status)) = self.updates.try_recv() {
            if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                job.status = status;
            }
        }
        while let Ok(files) = self.picked.1.try_recv() {
            self.add(&files);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            self.add(&dropped);
        }
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());

        egui::Frame::new().show(ui, |ui| {
                theme::page_header(
                    ui,
                    "Compress",
                    "A smaller copy is saved next to each file as name-min. The original is never touched.",
                );
                ui.add_space(16.0);
                self.drop_zone(ui, hovering);
                ui.add_space(12.0);
                self.settings_row(ui);
                ui.add_space(12.0);
                let footer = if self.show_shortcuts { 80.0 } else { 36.0 };
                egui::ScrollArea::vertical()
                    .max_height((ui.available_height() - footer).max(80.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.list(ui));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if !self.jobs.is_empty() && ui.button("Clear list").clicked() {
                        self.jobs
                            .retain(|j| matches!(j.status, Status::Waiting | Status::Working));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .selectable_label(self.show_shortcuts, "⌨ Shortcuts")
                            .clicked()
                        {
                            self.show_shortcuts = !self.show_shortcuts;
                        }
                    });
                });
                if self.show_shortcuts {
                    self.shortcuts.row(ui, "compress", "Open this tab");
                }
            });
    }

    fn drop_zone(&mut self, ui: &mut egui::Ui, hovering: bool) {
        let response = theme::drop_zone(
            ui,
            hovering,
            "Drop images, PDFs or Office files here",
            "or click to choose · JPG, PNG, PDF, Word, PowerPoint, Excel · folders too",
        );
        if response.clicked() {
            let (sender, ctx) = (self.picked.0.clone(), ui.ctx().clone());
            std::thread::spawn(move || {
                let mut exts: Vec<&str> = compress::IMAGE_EXTS.to_vec();
                exts.extend(compress::DOC_EXTS);
                if let Some(files) = rfd::FileDialog::new()
                    .set_title("Compress files")
                    .add_filter("Images and documents", &exts)
                    .pick_files()
                {
                    let _ = sender.send(files);
                    ctx.request_repaint();
                }
            });
        }
    }

    fn settings_row(&mut self, ui: &mut egui::Ui) {
        let before = (
            self.settings.quality,
            self.settings.max_width,
            self.settings.format,
        );
        theme::card()
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Quality").color(theme::MUTED));
                    ui.add(egui::Slider::new(&mut self.settings.quality, 30..=95));
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("Format").color(theme::MUTED));
                    egui::ComboBox::from_id_salt("format")
                        .selected_text(format_label(self.settings.format))
                        .show_ui(ui, |ui| {
                            for f in [Format::Same, Format::Jpeg, Format::Png] {
                                ui.selectable_value(&mut self.settings.format, f, format_label(f));
                            }
                        });
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("Max width").color(theme::MUTED));
                    egui::ComboBox::from_id_salt("width")
                        .selected_text(width_label(self.settings.max_width))
                        .show_ui(ui, |ui| {
                            for w in [0, 3840, 2560, 1920, 1280, 800] {
                                ui.selectable_value(
                                    &mut self.settings.max_width,
                                    w,
                                    width_label(w),
                                );
                            }
                        });
                });
            });
        if before
            != (
                self.settings.quality,
                self.settings.max_width,
                self.settings.format,
            )
        {
            compress::save(&self.settings);
        }
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        for job in self.jobs.iter().rev() {
            theme::card()
                .inner_margin(egui::Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let name = job.file.file_name().unwrap_or_default().to_string_lossy();
                        ui.add(egui::Label::new(name).truncate());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Status::Done(compress::Outcome { out: Some(out), .. }) =
                                &job.status
                                && ui
                                    .small_button("📂")
                                    .on_hover_text(format!("Show {}", out.display()))
                                    .clicked()
                                && let Some(dir) = out.parent()
                            {
                                theme::open_folder(dir);
                            }
                            let (text, color) = status_text(&job.status);
                            ui.label(egui::RichText::new(text).size(13.0).color(color));
                            if matches!(job.status, Status::Working) {
                                ui.add(egui::Spinner::new().size(14.0).color(theme::ACCENT));
                            }
                        });
                    });
                });
            ui.add_space(6.0);
        }
    }
}

fn status_text(status: &Status) -> (String, egui::Color32) {
    match status {
        Status::Waiting => ("Waiting…".into(), theme::MUTED),
        Status::Working => ("Compressing…".into(), theme::ACCENT),
        Status::Failed(error) => (format!("Couldn't read it: {error}"), theme::WARNING),
        Status::Done(compress::Outcome { out: None, .. }) => {
            ("✔  Already optimized".into(), theme::MUTED)
        }
        Status::Done(o) => {
            let saved = 100.0 - o.after as f64 * 100.0 / o.before.max(1) as f64;
            (
                format!(
                    "✔  {} » {}   −{saved:.0}%",
                    compress::format_size(o.before),
                    compress::format_size(o.after)
                ),
                theme::GREEN,
            )
        }
    }
}

fn format_label(format: Format) -> &'static str {
    match format {
        Format::Same => "Same as original",
        Format::Jpeg => "JPG",
        Format::Png => "PNG",
    }
}

fn width_label(width: u32) -> String {
    if width == 0 {
        "Keep size".into()
    } else {
        format!("{width} px")
    }
}
