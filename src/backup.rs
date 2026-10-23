// Backup: the folders you pick are copied by themselves every so often to another disk or
// folder. Only new or changed files are copied, and nothing is ever deleted there, so a file
// deleted by mistake is still in the backup. If the backup disk isn't plugged in, it says so.

use crate::{config, desktop, theme};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

const FILE: &str = "backup.json";
const HOURS: &[(u64, &str)] = &[
    (1, "every hour"),
    (6, "every 6 hours"),
    (12, "every 12 hours"),
    (24, "every day"),
    (168, "every week"),
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    sources: Vec<PathBuf>,
    dest: Option<PathBuf>,
    every_hours: u64,
    /// Unix seconds of the last finished backup.
    last_run: i64,
    last_result: String,
    /// When the "plug in the disk" warning was last shown.
    last_missing_warning: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            sources: vec![],
            dest: None,
            every_hours: 24,
            last_run: 0,
            last_result: String::new(),
            last_missing_warning: 0,
        }
    }
}

fn settings() -> MutexGuard<'static, Settings> {
    static SETTINGS: OnceLock<Mutex<Settings>> = OnceLock::new();
    SETTINGS
        .get_or_init(|| Mutex::new(config::read(FILE)))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

static RUNNING: AtomicBool = AtomicBool::new(false);
static FILES_DONE: AtomicU64 = AtomicU64::new(0);

fn now() -> i64 {
    chrono::Local::now().timestamp()
}

#[derive(Default)]
struct Totals {
    copied: u64,
    bytes: u64,
    failed: u64,
}

/// Copies what's new or changed from `from` into `to`, recursively.
fn copy_tree(from: &Path, to: &Path, totals: &mut Totals) {
    let Ok(entries) = std::fs::read_dir(from) else {
        totals.failed += 1;
        return;
    };
    if std::fs::create_dir_all(to).is_err() {
        totals.failed += 1;
        return;
    }
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            copy_tree(&src, &dst, totals);
            continue;
        }
        let changed = match std::fs::metadata(&dst) {
            Ok(old) => {
                old.len() != meta.len()
                    || meta
                        .modified()
                        .ok()
                        .zip(old.modified().ok())
                        // USB disks (FAT, exFAT, NTFS) keep dates less exactly: 2 s is "the same".
                        .is_some_and(|(new, old)| {
                            new.duration_since(old)
                                .is_ok_and(|d| d > std::time::Duration::from_secs(2))
                        })
            }
            Err(_) => true,
        };
        if !changed {
            continue;
        }
        match std::fs::copy(&src, &dst) {
            Ok(bytes) => {
                // The copy keeps the original's date, so it isn't copied again next time.
                if let Ok(modified) = meta.modified()
                    && let Ok(file) = std::fs::File::options().write(true).open(&dst)
                {
                    let _ = file.set_modified(modified);
                }
                totals.copied += 1;
                totals.bytes += bytes;
                FILES_DONE.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => totals.failed += 1,
        }
    }
}

/// Is `path` the same as `dir` or inside it? Compared on the real paths (links resolved).
fn inside(path: &Path, dir: &Path) -> bool {
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    real(path).starts_with(real(dir))
}

/// The folder each source gets in the backup: its own name, plus its parent's when two
/// share a name ("Docs (work)", "Docs (home)"), so they never mix. Always the same for
/// the same list, so the next backup finds them.
fn folder_names(sources: &[PathBuf]) -> Vec<String> {
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            // "/" or "D:\": named after the drive.
            .unwrap_or_else(|| {
                let drive: String = p
                    .to_string_lossy()
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect();
                if drive.is_empty() { "disk".into() } else { format!("disk {drive}") }
            })
    };
    sources
        .iter()
        .map(|source| {
            let own = name(source);
            let shared = sources.iter().filter(|s| name(s) == own).count() > 1;
            match source.parent().and_then(|p| p.file_name()) {
                Some(parent) if shared => format!("{own} ({})", parent.to_string_lossy()),
                _ => own,
            }
        })
        .collect()
}

/// Runs a backup in its own thread (it can take a while). Returns false if one is running
/// or there's nothing to do.
fn start(ctx: Option<egui::Context>) -> bool {
    let (sources, dest) = {
        let s = settings();
        (s.sources.clone(), s.dest.clone())
    };
    let Some(dest) = dest else { return false };
    if sources.is_empty() || RUNNING.swap(true, Ordering::SeqCst) {
        return false;
    }
    FILES_DONE.store(0, Ordering::Relaxed);
    std::thread::spawn(move || {
        let mut totals = Totals::default();
        let mut skipped = 0;
        for (source, name) in sources.iter().zip(folder_names(&sources)) {
            // A backup inside what it backs up would copy itself forever.
            if inside(&dest, source) || inside(source, &dest) {
                skipped += 1;
                continue;
            }
            copy_tree(source, &dest.join(name), &mut totals);
        }
        totals.failed += skipped;
        let result = if skipped > 0 {
            "The backup folder can't be inside a folder it backs up (or the other way around)".to_string()
        } else if totals.failed > 0 {
            format!(
                "{} files copied ({}), {} couldn't be copied",
                totals.copied,
                crate::compress::format_size(totals.bytes),
                totals.failed
            )
        } else if totals.copied == 0 {
            "Up to date, nothing new to copy".to_string()
        } else {
            format!(
                "{} files copied ({})",
                totals.copied,
                crate::compress::format_size(totals.bytes)
            )
        };
        {
            let mut s = settings();
            s.last_run = now();
            s.last_result = result.clone();
            config::write(FILE, &*s);
        }
        if totals.failed > 0 {
            desktop::notify("💾 Backup finished with problems", &result);
        }
        RUNNING.store(false, Ordering::SeqCst);
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });
    true
}

pub fn tick() -> bool {
    let mut s = settings();
    let Some(dest) = s.dest.clone() else {
        return false;
    };
    if s.sources.is_empty() || now() - s.last_run < s.every_hours as i64 * 3600 {
        return false;
    }
    if !dest.exists() {
        if now() - s.last_missing_warning > 24 * 3600 {
            s.last_missing_warning = now();
            config::write(FILE, &*s);
            desktop::notify(
                "💾 Plug in your backup disk",
                &format!("The backup goes to {}, which isn't there.", dest.display()),
            );
        }
        return false;
    }
    drop(s);
    start(None)
}

#[derive(Default)]
pub struct BackupPage {
    picked: Option<std::sync::mpsc::Receiver<(bool, PathBuf)>>,
}

impl BackupPage {
    /// Folder chooser in a thread. `dest`: choosing where the backup goes.
    fn pick(&mut self, ctx: &egui::Context, dest: bool) {
        let (sender, receiver) = std::sync::mpsc::channel();
        self.picked = Some(receiver);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let title = if dest {
                "Where should the backup go?"
            } else {
                "Which folder should be backed up?"
            };
            if let Some(dir) = rfd::FileDialog::new().set_title(title).pick_folder() {
                let _ = sender.send((dest, dir));
                ctx.request_repaint();
            }
        });
    }
}

impl crate::Page for BackupPage {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Some((dest, dir)) = self.picked.as_ref().and_then(|r| r.try_recv().ok()) {
            let mut s = settings();
            if dest {
                s.dest = Some(dir);
            } else if !s.sources.contains(&dir) {
                s.sources.push(dir);
            }
            config::write(FILE, &*s);
        }
        let running = RUNNING.load(Ordering::SeqCst);
        if running {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        theme::page_header_with_switch(
            ui,
            "Backup",
            "Copies your folders by themselves to another disk. Only what's new or changed is copied, and nothing is deleted there.",
            "backup",
        );
        ui.add_space(16.0);

        let mut s = settings().clone();
        let before = serde_json::to_string(&s).unwrap_or_default();
        let mut remove = None;
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new("Folders to back up").strong());
            ui.add_space(4.0);
            if s.sources.is_empty() {
                ui.label(egui::RichText::new("None yet.").color(theme::MUTED));
            }
            for (i, source) in s.sources.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("📁 {}", source.display()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("✖")
                            .on_hover_text("Stop backing it up")
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                });
            }
            ui.add_space(4.0);
            if ui.button("➕ Add a folder").clicked() {
                self.pick(&ctx, false);
            }
        });
        ui.add_space(10.0);
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new("Where it goes").strong());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                match &s.dest {
                    Some(dest) if dest.exists() => {
                        ui.label(format!("💾 {}", dest.display()));
                    }
                    Some(dest) => {
                        ui.label(
                            egui::RichText::new(format!("💾 {} (not plugged in)", dest.display()))
                                .color(theme::WARNING),
                        );
                    }
                    None => {
                        ui.label(
                            egui::RichText::new("Pick a folder on another disk or a USB drive.")
                                .color(theme::MUTED),
                        );
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Choose…").clicked() {
                        self.pick(&ctx, true);
                    }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Back up");
                egui::ComboBox::from_id_salt("every")
                    .selected_text(
                        HOURS
                            .iter()
                            .find(|h| h.0 == s.every_hours)
                            .map_or("every day", |h| h.1),
                    )
                    .show_ui(ui, |ui| {
                        for &(hours, label) in HOURS {
                            ui.selectable_value(&mut s.every_hours, hours, label);
                        }
                    });
            });
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let ready = s.dest.as_ref().is_some_and(|d| d.exists()) && !s.sources.is_empty();
            if ui
                .add_enabled(
                    ready && !running,
                    egui::Button::new(
                        egui::RichText::new("Back up now").color(egui::Color32::WHITE),
                    )
                    .fill(theme::ACCENT),
                )
                .clicked()
            {
                start(Some(ctx.clone()));
            }
            if running {
                ui.add(egui::Spinner::new().color(theme::ACCENT));
                ui.label(format!(
                    "Copying… {} files so far",
                    FILES_DONE.load(Ordering::Relaxed)
                ));
            } else if s.last_run > 0 {
                ui.label(
                    egui::RichText::new(format!(
                        "Last backup {}: {}",
                        crate::reminders::when(s.last_run),
                        s.last_result
                    ))
                    .color(theme::MUTED),
                );
            }
        });
        if let Some(i) = remove {
            s.sources.remove(i);
        }
        if serde_json::to_string(&s).unwrap_or_default() != before {
            config::write(FILE, &s);
            *settings() = s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_only_new_or_changed_files() {
        let root = std::env::temp_dir().join(format!("toolswitch-backup-{}", std::process::id()));
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), b"one").unwrap();
        std::fs::write(src.join("sub/b.txt"), b"two").unwrap();
        let mut totals = Totals::default();
        copy_tree(&src, &dst, &mut totals);
        assert_eq!(totals.copied, 2);
        assert_eq!(std::fs::read(dst.join("sub/b.txt")).unwrap(), b"two");

        let mut again = Totals::default();
        copy_tree(&src, &dst, &mut again);
        assert_eq!(again.copied, 0, "nothing changed, nothing copied");

        std::fs::write(src.join("a.txt"), b"one, longer").unwrap();
        std::fs::remove_file(src.join("sub/b.txt")).unwrap();
        let mut third = Totals::default();
        copy_tree(&src, &dst, &mut third);
        assert_eq!(third.copied, 1);
        assert!(
            dst.join("sub/b.txt").exists(),
            "deleted files stay in the backup"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn names_never_mix() {
        let names = folder_names(&[
            PathBuf::from("/home/me/work/Docs"),
            PathBuf::from("/home/me/home/Docs"),
            PathBuf::from("/home/me/Photos"),
            PathBuf::from("/"),
        ]);
        assert_eq!(names, ["Docs (work)", "Docs (home)", "Photos", "disk"]);
        assert!(inside(Path::new("/home/me/Backup"), Path::new("/home/me")));
        assert!(!inside(Path::new("/media/usb"), Path::new("/home/me")));
    }
}
