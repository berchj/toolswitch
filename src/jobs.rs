// A list of files being processed one by one in a background thread, shown as cards with
// their status. Used by the tools that turn a file into another (remove background, sign PDF).

use crate::theme;
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// What happened to a file: the new file and a short note ("-62%", "page 1").
pub type Outcome = Result<(PathBuf, String), String>;
pub type Work = Arc<dyn Fn(&Path) -> Outcome + Send + Sync>;

enum Status {
    Waiting,
    Working,
    /// The new file, its note, and a small picture of it when it's an image.
    Done(PathBuf, String, Option<egui::ColorImage>),
    Failed(String),
}

struct Job {
    file: PathBuf,
    status: Status,
    preview: Option<egui::TextureHandle>,
}

const PREVIEW: u32 = 48;

/// A small version of an image result, made in the worker thread.
fn preview(out: &Path) -> Option<egui::ColorImage> {
    let ext = out.extension()?.to_str()?.to_lowercase();
    if !["png", "jpg", "jpeg", "webp"].contains(&ext.as_str()) {
        return None;
    }
    let small = image::open(out).ok()?.thumbnail(PREVIEW * 2, PREVIEW * 2).into_rgba8();
    let size = [small.width() as usize, small.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, small.as_raw()))
}

pub struct FileJobs {
    jobs: Vec<Job>,
    queue: Sender<(usize, PathBuf, Work)>,
    updates: Receiver<(usize, Status)>,
}

impl FileJobs {
    pub fn new(ctx: &egui::Context) -> Self {
        let (queue, work) = channel::<(usize, PathBuf, Work)>();
        let (send_update, updates) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for (i, file, run) in work {
                let _ = send_update.send((i, Status::Working));
                ctx.request_repaint();
                // A broken file can make a library give up (panic); it fails alone, the rest go on.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&file)));
                let status = match outcome {
                    Ok(Ok((out, note))) => {
                        let picture = preview(&out);
                        Status::Done(out, note, picture)
                    }
                    Ok(Err(error)) => Status::Failed(error),
                    Err(_) => Status::Failed("Couldn't read it: the file looks damaged".into()),
                };
                let _ = send_update.send((i, status));
                ctx.request_repaint();
            }
        });
        FileJobs {
            jobs: vec![],
            queue,
            updates,
        }
    }

    pub fn add(&mut self, file: PathBuf, work: Work) {
        let i = self.jobs.len();
        let _ = self.queue.send((i, file.clone(), work));
        self.jobs.push(Job {
            file,
            status: Status::Waiting,
            preview: None,
        });
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Takes the news from the background thread. Call once per frame.
    pub fn poll(&mut self) {
        while let Ok((i, status)) = self.updates.try_recv() {
            if let Some(job) = self.jobs.get_mut(i) {
                job.status = status;
            }
        }
    }

    /// Forgets the finished ones.
    pub fn clear(&mut self) {
        // Indexes are positions in the list, so only a fully finished list can be emptied.
        if self
            .jobs
            .iter()
            .all(|j| matches!(j.status, Status::Done(..) | Status::Failed(_)))
        {
            self.jobs.clear();
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        for (i, job) in self.jobs.iter_mut().enumerate().rev() {
            // The picture becomes a texture once, the first time it's shown.
            if let Status::Done(_, _, picture) = &mut job.status
                && let Some(picture) = picture.take()
            {
                job.preview = Some(ui.ctx().load_texture(
                    format!("job-{i}"),
                    picture,
                    egui::TextureOptions::LINEAR,
                ));
            }
            theme::card()
                .inner_margin(egui::Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        if let Some(texture) = &job.preview {
                            checkered_preview(ui, texture);
                        }
                        let name = job.file.file_name().unwrap_or_default().to_string_lossy();
                        ui.add(egui::Label::new(name).truncate());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            match &job.status {
                                Status::Waiting => {
                                    ui.label(egui::RichText::new("Waiting…").color(theme::MUTED));
                                }
                                Status::Working => {
                                    ui.label(egui::RichText::new("Working…").color(theme::ACCENT));
                                    ui.add(egui::Spinner::new().size(14.0).color(theme::ACCENT));
                                }
                                Status::Done(out, note, _) => {
                                    if ui
                                        .small_button("📂")
                                        .on_hover_text(format!("Show {}", out.display()))
                                        .clicked()
                                        && let Some(dir) = out.parent()
                                    {
                                        theme::open_folder(dir);
                                    }
                                    let name =
                                        out.file_name().unwrap_or_default().to_string_lossy();
                                    ui.label(
                                        egui::RichText::new(format!("✔ {name}  {note}"))
                                            .size(13.0)
                                            .color(theme::GREEN),
                                    );
                                }
                                Status::Failed(error) => {
                                    ui.label(
                                        egui::RichText::new(error).size(13.0).color(theme::WARNING),
                                    );
                                }
                            }
                        });
                    });
                });
            ui.add_space(6.0);
        }
    }
}

/// The result's picture on a checkerboard, so a transparent background shows as such.
fn checkered_preview(ui: &mut egui::Ui, texture: &egui::TextureHandle) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(PREVIEW as f32, PREVIEW as f32), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let cell = 6.0;
    for y in 0..(PREVIEW / 6) {
        for x in 0..(PREVIEW / 6) {
            let light = (x + y) % 2 == 0;
            let color = egui::Color32::from_gray(if light { 200 } else { 160 });
            let min = rect.min + egui::vec2(x as f32 * cell, y as f32 * cell);
            painter.rect_filled(egui::Rect::from_min_size(min, egui::vec2(cell, cell)), 0.0, color);
        }
    }
    let size = texture.size_vec2();
    let scale = (rect.width() / size.x).min(rect.height() / size.y);
    let shown = egui::Rect::from_center_size(rect.center(), size * scale);
    painter.image(
        texture.id(),
        shown,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

/// The system's file chooser in a thread, so the window keeps drawing. The files arrive
/// through `sender`.
pub fn pick_files(
    ctx: &egui::Context,
    sender: Sender<Vec<PathBuf>>,
    title: &'static str,
    filter: (&'static str, &'static [&'static str]),
) {
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        if let Some(files) = rfd::FileDialog::new()
            .set_title(title)
            .add_filter(filter.0, filter.1)
            .pick_files()
        {
            let _ = sender.send(files);
            ctx.request_repaint();
        }
    });
}

/// Files and folders (searched recursively, skipping hidden ones and links) -> the files
/// `keep` accepts.
pub fn collect(paths: &[PathBuf], keep: fn(&Path) -> bool) -> Vec<PathBuf> {
    fn walk(p: &Path, keep: fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
        if p.is_dir() {
            let Ok(entries) = std::fs::read_dir(p) else {
                return;
            };
            let mut names: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            names.sort();
            for n in names {
                let hidden = n
                    .file_name()
                    .is_some_and(|f| f.to_string_lossy().starts_with('.'));
                // Links inside a folder aren't followed: one pointing back up would never end.
                let link = std::fs::symlink_metadata(&n).is_ok_and(|m| m.is_symlink());
                if !hidden && !link {
                    walk(&n, keep, out);
                }
            }
        } else if keep(p) && !out.contains(&p.to_path_buf()) {
            out.push(p.to_path_buf());
        }
    }
    let mut out = vec![];
    for p in paths {
        walk(p, keep, &mut out);
    }
    out
}
