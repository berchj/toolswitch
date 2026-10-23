// Clean up: shows what takes space (the trash, caches, old downloads, very big files) and
// frees it with a click. Files go to the trash, so they can be brought back; only emptying
// the trash and clearing caches delete for good, and those ask first.

use crate::{config, theme};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, SystemTime};

const OLD_DOWNLOAD: Duration = Duration::from_secs(30 * 24 * 3600);
const BIG_FILE: u64 = 500 << 20;
const BIG_FILES_SHOWN: usize = 20;

#[derive(Default)]
struct Scan {
    trash: u64,
    trash_items: usize,
    /// Folders inside the cache folder, biggest first.
    caches: Vec<(PathBuf, u64)>,
    old_downloads: Vec<(PathBuf, u64)>,
    big_files: Vec<(PathBuf, u64)>,
}

fn size(bytes: u64) -> String {
    crate::compress::format_size(bytes)
}

/// The size of a file or everything in a folder. Links aren't followed.
fn disk_usage(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|entries| entries.flatten().map(|e| disk_usage(&e.path())).sum())
        .unwrap_or(0)
}

/// The system's cache folder for all apps.
fn cache_root() -> PathBuf {
    if cfg!(windows) {
        std::env::temp_dir()
    } else {
        config::cache_dir()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }
}

fn trash_size() -> (u64, usize) {
    #[cfg(any(windows, all(unix, not(target_os = "macos"))))]
    {
        let Ok(items) = trash::os_limited::list() else {
            return (0, 0);
        };
        let count = items.len();
        #[cfg(windows)]
        let bytes = items
            .iter()
            .filter_map(|i| trash::os_limited::metadata(i).ok())
            .map(|m| match m.size {
                trash::TrashItemSize::Bytes(b) => b,
                trash::TrashItemSize::Entries(_) => 0,
            })
            .sum();
        #[cfg(not(windows))]
        let bytes = trash::os_limited::trash_folders()
            .map(|dirs| dirs.iter().map(|d| disk_usage(&d.join("files"))).sum())
            .unwrap_or(0);
        (bytes, count)
    }
    #[cfg(target_os = "macos")]
    {
        let dir = config::home().join(".Trash");
        let count = std::fs::read_dir(&dir).map_or(0, |e| e.count());
        (disk_usage(&dir), count)
    }
}

fn empty_trash() -> Result<(), String> {
    #[cfg(any(windows, all(unix, not(target_os = "macos"))))]
    {
        let items = trash::os_limited::list().map_err(|e| e.to_string())?;
        trash::os_limited::purge_all(items).map_err(|e| e.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        let dir = config::home().join(".Trash");
        for entry in std::fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let path = entry.path();
            let _ = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
        }
        Ok(())
    }
}

/// Files over BIG_FILE in the home folder, skipping hidden folders (settings, caches).
fn big_files(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, u64)>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            big_files(&path, depth - 1, out);
        } else if meta.len() >= BIG_FILE {
            out.push((path, meta.len()));
        }
    }
}

fn scan() -> Scan {
    let (trash, trash_items) = trash_size();

    let root = cache_root();
    let mut caches: Vec<(PathBuf, u64)> = std::fs::read_dir(&root)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                // toolswitch's own AI models would just be downloaded again.
                .filter(|p| *p != config::cache_dir())
                .map(|p| {
                    let bytes = disk_usage(&p);
                    (p, bytes)
                })
                .filter(|(_, bytes)| *bytes > 1 << 20)
                .collect()
        })
        .unwrap_or_default();
    caches.sort_by_key(|c| std::cmp::Reverse(c.1));

    let now = SystemTime::now();
    let mut old_downloads: Vec<(PathBuf, u64)> = std::fs::read_dir(crate::power::downloads_dir())
        .map(|entries| {
            entries
                .flatten()
                // Files only: a folder's date says nothing about what's inside it.
                .filter(|e| e.metadata().is_ok_and(|m| m.is_file()))
                .filter(|e| {
                    e.metadata()
                        .and_then(|m| m.modified())
                        .is_ok_and(|t| now.duration_since(t).unwrap_or_default() > OLD_DOWNLOAD)
                })
                .map(|e| {
                    let bytes = disk_usage(&e.path());
                    (e.path(), bytes)
                })
                .collect()
        })
        .unwrap_or_default();
    old_downloads.sort_by_key(|c| std::cmp::Reverse(c.1));

    let mut big = vec![];
    big_files(&config::home(), 8, &mut big);
    big.sort_by_key(|c| std::cmp::Reverse(c.1));
    big.truncate(BIG_FILES_SHOWN);

    Scan {
        trash,
        trash_items,
        caches,
        old_downloads,
        big_files: big,
    }
}

pub struct CleanupPage {
    scan: Option<Scan>,
    scanning: bool,
    results: (Sender<Scan>, Receiver<Scan>),
    /// The action clicked once, waiting for the second click.
    confirm: Option<String>,
    message: Option<(String, bool)>,
}

impl Default for CleanupPage {
    fn default() -> Self {
        CleanupPage {
            scan: None,
            scanning: false,
            results: channel(),
            confirm: None,
            message: None,
        }
    }
}

impl CleanupPage {
    fn start_scan(&mut self, ctx: &egui::Context) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        let (sender, ctx) = (self.results.0.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = sender.send(scan());
            ctx.request_repaint();
        });
    }

    /// A button that needs two clicks: "Empty" then "Sure?".
    fn confirmed(&mut self, ui: &mut egui::Ui, key: &str, label: &str) -> bool {
        let sure = self.confirm.as_deref() == Some(key);
        let text = if sure { "Sure?" } else { label };
        let mut button = egui::Button::new(egui::RichText::new(text).size(13.0));
        if sure {
            button = button.fill(theme::WARNING.gamma_multiply(0.4));
        }
        if ui.add(button).clicked() {
            if sure {
                self.confirm = None;
                return true;
            }
            self.confirm = Some(key.to_string());
        }
        false
    }

    fn done(&mut self, result: Result<(), String>, what: &str, ctx: &egui::Context) {
        self.message = Some(match result {
            Ok(()) => (format!("✔ {what}"), true),
            Err(error) => (format!("Couldn't: {error}"), false),
        });
        self.start_scan(ctx);
    }
}

impl crate::Page for CleanupPage {
    fn opened(&mut self) {
        // Looked at again each time the screen opens (in the background).
        self.scan = None;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Ok(scan) = self.results.1.try_recv() {
            self.scan = Some(scan);
            self.scanning = false;
        }
        if self.scan.is_none() {
            self.start_scan(&ctx);
        }
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                theme::page_header(
                    ui,
                    "Clean up",
                    "What takes space on your computer. Files go to the trash, so you can bring them back.",
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.scanning {
                    ui.add(egui::Spinner::new().color(theme::ACCENT));
                } else if ui.button("⟳ Look again").clicked() {
                    self.start_scan(&ctx);
                }
            });
        });
        ui.add_space(12.0);
        if let Some((text, ok)) = &self.message {
            ui.label(egui::RichText::new(text).color(if *ok {
                theme::GREEN
            } else {
                theme::WARNING
            }));
            ui.add_space(6.0);
        }
        let Some(scan) = self.scan.take() else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.add(egui::Spinner::new().size(28.0).color(theme::ACCENT));
                ui.label(egui::RichText::new("Looking at your disk…").color(theme::MUTED));
            });
            return;
        };

        let mut action: Option<(Result<(), String>, String)> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Trash
                section(
                    ui,
                    "🗑  Trash",
                    &format!("{} · {} items", size(scan.trash), scan.trash_items),
                    |ui| {
                        if scan.trash_items > 0 && self.confirmed(ui, "trash", "Empty") {
                            action = Some((empty_trash(), "Trash emptied".into()));
                        }
                    },
                );

                // Caches
                let cache_total: u64 = scan.caches.iter().map(|c| c.1).sum();
                section(
                    ui,
                    "📦  App caches",
                    &format!("{} · apps make them again when needed", size(cache_total)),
                    |_| {},
                );
                for (path, bytes) in scan.caches.iter().take(10) {
                    item_row(ui, path, *bytes, |ui| {
                        let key = format!("cache:{}", path.display());
                        if self.confirmed(ui, &key, "Delete") {
                            action = Some((
                                std::fs::remove_dir_all(path)
                                    .or_else(|_| std::fs::remove_file(path))
                                    .map_err(|e| e.to_string()),
                                format!("Deleted {}", name(path)),
                            ));
                        }
                    });
                }
                ui.add_space(8.0);

                // Old downloads
                let old_total: u64 = scan.old_downloads.iter().map(|c| c.1).sum();
                section(
                    ui,
                    "📥  Old downloads",
                    &format!(
                        "{} · {} files not touched in 30 days",
                        size(old_total),
                        scan.old_downloads.len()
                    ),
                    |ui| {
                        if !scan.old_downloads.is_empty()
                            && self.confirmed(ui, "downloads", "Move all to trash")
                        {
                            let paths: Vec<&PathBuf> =
                                scan.old_downloads.iter().map(|d| &d.0).collect();
                            action = Some((
                                trash::delete_all(paths).map_err(|e| e.to_string()),
                                "Old downloads moved to the trash".into(),
                            ));
                        }
                    },
                );
                for (path, bytes) in scan.old_downloads.iter().take(5) {
                    item_row(ui, path, *bytes, |_| {});
                }
                if scan.old_downloads.len() > 5 {
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "and {} more",
                                scan.old_downloads.len() - 5
                            ))
                            .size(13.0)
                            .color(theme::MUTED),
                        );
                    });
                }
                ui.add_space(8.0);

                // Big files
                section(
                    ui,
                    "🐘  Big files",
                    &format!("over {} MB in your folders", BIG_FILE >> 20),
                    |_| {},
                );
                if scan.big_files.is_empty() {
                    ui.label(egui::RichText::new("None.").color(theme::MUTED));
                }
                for (path, bytes) in &scan.big_files {
                    item_row(ui, path, *bytes, |ui| {
                        if ui
                            .small_button("📂")
                            .on_hover_text("Show in its folder")
                            .clicked()
                            && let Some(dir) = path.parent()
                        {
                            theme::open_folder(dir);
                        }
                        let key = format!("big:{}", path.display());
                        if self.confirmed(ui, &key, "Move to trash") {
                            action = Some((
                                trash::delete(path).map_err(|e| e.to_string()),
                                format!("{} moved to the trash", name(path)),
                            ));
                        }
                    });
                }
            });
        self.scan = Some(scan);
        if let Some((result, what)) = action {
            self.done(result, &what, &ctx);
        }
    }
}

fn name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn section(ui: &mut egui::Ui, title: &str, detail: &str, buttons: impl FnOnce(&mut egui::Ui)) {
    theme::card()
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(egui::RichText::new(title).strong());
                    ui.label(egui::RichText::new(detail).size(13.0).color(theme::MUTED));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), buttons);
            });
        });
    ui.add_space(4.0);
}

fn item_row(ui: &mut egui::Ui, path: &Path, bytes: u64, buttons: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.add(egui::Label::new(name(path)).truncate())
            .on_hover_text(path.display().to_string());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            buttons(ui);
            ui.label(
                egui::RichText::new(size(bytes))
                    .size(13.0)
                    .color(theme::MUTED),
            );
        });
    });
}
