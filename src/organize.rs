// Tidy downloads: files in the Downloads folder go to their place by type (PDFs to Documents,
// photos to Pictures, installers to Downloads/Installers...), and optionally old ones to the
// trash. Files still downloading or just finished are left alone. Every move can be undone.

use crate::{config, desktop, theme};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};

const FILE: &str = "organize.json";
const EVERY: Duration = Duration::from_secs(5 * 60);
/// Files changed more recently than this may still be in use.
const SETTLE: Duration = Duration::from_secs(120);
const LOG_SIZE: usize = 100;

#[derive(Clone, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    pub on: bool,
    /// "pdf docx txt"
    pub exts: String,
    pub dest: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Unix seconds.
    pub at: i64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    rules: Vec<Rule>,
    /// Files left in Downloads older than this many days go to the trash. 0 = never.
    trash_after: u32,
    log: Vec<Move>,
}

impl Default for Settings {
    fn default() -> Self {
        let downloads = crate::power::downloads_dir();
        let rule = |name: &str, exts: &str, dest: PathBuf| Rule {
            name: name.into(),
            on: true,
            exts: exts.into(),
            dest,
        };
        Settings {
            rules: vec![
                rule(
                    "Documents",
                    "pdf doc docx odt rtf txt md xls xlsx ods csv ppt pptx odp epub",
                    config::user_dir("DOCUMENTS", "Documents"),
                ),
                rule(
                    "Images",
                    "jpg jpeg png gif webp heic avif svg bmp tif tiff",
                    config::user_dir("PICTURES", "Pictures"),
                ),
                rule(
                    "Videos",
                    "mp4 mkv mov avi webm m4v",
                    config::user_dir(
                        "VIDEOS",
                        if cfg!(target_os = "macos") {
                            "Movies"
                        } else {
                            "Videos"
                        },
                    ),
                ),
                rule(
                    "Music",
                    "mp3 flac wav ogg m4a aac opus",
                    config::user_dir("MUSIC", "Music"),
                ),
                rule(
                    "Archives",
                    "zip rar 7z tar gz tgz xz bz2 zst",
                    downloads.join("Archives"),
                ),
                rule(
                    "Installers",
                    "deb rpm appimage flatpakref exe msi msix dmg pkg",
                    downloads.join("Installers"),
                ),
            ],
            trash_after: 0,
            log: vec![],
        }
    }
}

struct State {
    settings: Settings,
    last: Option<Instant>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| {
            Mutex::new(State {
                settings: config::read(FILE),
                last: None,
            })
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn ext(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_lowercase()
}

/// "report.pdf" in a folder that has one already -> "report (2).pdf".
fn free_name(dir: &Path, file: &Path) -> PathBuf {
    let name = file.file_name().unwrap_or_default();
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    let ext = file
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

/// Moves a file. Across disks a rename can't work, so it's copied and the original removed;
/// if any step fails nothing is left behind twice.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() != std::io::ErrorKind::CrossesDevices => return Err(e),
        Err(_) => {}
    }
    if let Err(e) = std::fs::copy(from, to) {
        let _ = std::fs::remove_file(to);
        return Err(e);
    }
    if let Err(e) = std::fs::remove_file(from) {
        // The original is in use (open in another program): keep it, drop the copy.
        let _ = std::fs::remove_file(to);
        return Err(e);
    }
    Ok(())
}

/// Files the system keeps in every folder; never touched.
const SYSTEM_FILES: &[&str] = &["desktop.ini", "thumbs.db", ".ds_store"];

/// How long ago it arrived: the newest of its change and creation dates (some downloaders
/// keep the server's old date as the change date).
fn age(meta: &std::fs::Metadata, now: SystemTime) -> Duration {
    let newest = [meta.modified().ok(), meta.created().ok()]
        .into_iter()
        .flatten()
        .max();
    newest
        .and_then(|t| now.duration_since(t).ok())
        .unwrap_or_default()
}

/// Tidies the Downloads folder once, with these settings. Returns the moves and how many
/// files went (moved or to the trash).
/// Tidies the `downloads` folder (a parameter so the tests can use a folder of their own).
fn tidy(downloads: &Path, settings: &Settings, now: SystemTime) -> (Vec<Move>, usize) {
    // A wrong user-dirs.dirs can point "Downloads" at the home folder: never tidy that.
    if downloads.parent().is_none() || config::home().starts_with(downloads) {
        return (vec![], 0);
    }
    let Ok(entries) = std::fs::read_dir(downloads) else {
        return (vec![], 0);
    };
    let mut moves = vec![];
    let mut to_trash = vec![];
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        let name = entry.file_name().to_string_lossy().to_lowercase();
        // Folders, hidden and system files, and the ones still downloading stay.
        if !meta.is_file() || name.starts_with('.') || SYSTEM_FILES.contains(&name.as_str()) {
            continue;
        }
        let age = age(&meta, now);
        if age < SETTLE || crate::power::is_partial(&path) {
            continue;
        }
        let e = ext(&path);
        let matches = |r: &&Rule| r.exts.split_whitespace().any(|x| x.eq_ignore_ascii_case(&e));
        match settings.rules.iter().find(matches) {
            // A type whose rule is off is left alone, not sent to the trash.
            Some(rule) if !rule.on => {}
            Some(rule) => {
                if rule.dest == downloads || std::fs::create_dir_all(&rule.dest).is_err() {
                    continue;
                }
                let to = free_name(&rule.dest, &path);
                if move_file(&path, &to).is_ok() {
                    moves.push(Move {
                        from: path,
                        to,
                        at: chrono::Local::now().timestamp(),
                    });
                }
            }
            None => {
                if settings.trash_after > 0
                    && age > Duration::from_secs(settings.trash_after as u64 * 86400)
                {
                    to_trash.push(path);
                }
            }
        }
    }
    let mut count = moves.len();
    if !to_trash.is_empty() && trash::delete_all(&to_trash).is_ok() {
        count += to_trash.len();
    }
    (moves, count)
}

static RUNNING: AtomicBool = AtomicBool::new(false);
/// What the last tidy did, for the screen.
static LAST_RESULT: Mutex<Option<String>> = Mutex::new(None);

/// Tidies in its own thread (moving big files to another disk takes a while), then adds the
/// moves to the log. Returns false if one is already running.
fn start(ctx: Option<egui::Context>) -> bool {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return false;
    }
    let settings = state().settings.clone();
    std::thread::spawn(move || {
        let (moves, count) = tidy(&crate::power::downloads_dir(), &settings, SystemTime::now());
        if !moves.is_empty() {
            let mut state = state();
            state.settings.log.extend(moves);
            let extra = state.settings.log.len().saturating_sub(LOG_SIZE);
            state.settings.log.drain(..extra);
            config::write(FILE, &state.settings);
        }
        if let Ok(mut last) = LAST_RESULT.lock() {
            *last = Some(match count {
                0 => "Nothing to tidy.".into(),
                1 => "1 file tidied.".into(),
                n => format!("{n} files tidied."),
            });
        }
        RUNNING.store(false, Ordering::SeqCst);
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });
    true
}

pub fn tick() -> bool {
    {
        let mut state = state();
        if state.last.is_some_and(|at| at.elapsed() < EVERY) {
            return false;
        }
        state.last = Some(Instant::now());
    }
    start(None);
    false
}

/// Puts a moved file back where it was.
fn undo(i: usize) -> Result<(), String> {
    let mut state = state();
    let Some(m) = state.settings.log.get(i).cloned() else {
        return Ok(());
    };
    let back = free_name(m.from.parent().unwrap_or(Path::new(".")), &m.from);
    move_file(&m.to, &back).map_err(|e| e.to_string())?;
    state.settings.log.remove(i);
    config::write(FILE, &state.settings);
    Ok(())
}

#[derive(Default)]
pub struct OrganizePage {
    picked: Option<std::sync::mpsc::Receiver<(usize, PathBuf)>>,
}

impl crate::Page for OrganizePage {
    fn ui(&mut self, ui: &mut egui::Ui) {
        if let Some((i, dir)) = self.picked.as_ref().and_then(|r| r.try_recv().ok()) {
            let mut state = state();
            if let Some(rule) = state.settings.rules.get_mut(i) {
                rule.dest = dir;
            }
            config::write(FILE, &state.settings);
        }
        theme::page_header_with_switch(
            ui,
            "Tidy downloads",
            &format!(
                "Every few minutes, files in {} go to their folder by type. Turned off, it only tidies when you click.",
                config::short(&crate::power::downloads_dir())
            ),
            "organize",
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("Tidy now").color(egui::Color32::WHITE))
                        .fill(theme::ACCENT),
                )
                .clicked()
            {
                start(Some(ui.ctx().clone()));
            }
            if RUNNING.load(Ordering::SeqCst) {
                ui.add(egui::Spinner::new().color(theme::ACCENT));
            } else if let Some(message) = LAST_RESULT.lock().ok().and_then(|m| m.clone()) {
                ui.label(egui::RichText::new(message).color(theme::GREEN));
            }
        });
        ui.add_space(12.0);

        let mut settings = state().settings.clone();
        let before = serde_json::to_string(&settings).unwrap_or_default();
        let mut undo_index = None;
        let mut pick_for = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                theme::card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new("Where each type goes").strong());
                    ui.add_space(6.0);
                    egui::Grid::new("rules")
                        .num_columns(4)
                        .spacing([10.0, 8.0])
                        .show(ui, |ui| {
                            for (i, rule) in settings.rules.iter_mut().enumerate() {
                                theme::toggle(ui, &mut rule.on);
                                ui.label(&rule.name);
                                ui.add_sized(
                                    [220.0, 24.0],
                                    egui::TextEdit::singleline(&mut rule.exts),
                                )
                                .on_hover_text("File endings, separated by spaces");
                                let folder = config::short(&rule.dest);
                                if ui
                                    .button(format!("📁 {folder}"))
                                    .on_hover_text("Change the folder")
                                    .clicked()
                                {
                                    pick_for = Some(i);
                                }
                                ui.end_row();
                            }
                        });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let mut on = settings.trash_after > 0;
                        if theme::toggle(ui, &mut on).changed() {
                            settings.trash_after = if on { 30 } else { 0 };
                        }
                        ui.label("Move other files to the trash after");
                        ui.add_enabled_ui(on, |ui| {
                            egui::ComboBox::from_id_salt("trash_after")
                                .selected_text(format!("{} days", if settings.trash_after == 0 { 30 } else { settings.trash_after }))
                                .show_ui(ui, |ui| {
                                    for days in [7, 14, 30, 60, 90] {
                                        ui.selectable_value(
                                            &mut settings.trash_after,
                                            days,
                                            format!("{days} days"),
                                        );
                                    }
                                });
                        });
                    });
                });
                ui.add_space(12.0);
                if !settings.log.is_empty() {
                    theme::card().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(egui::RichText::new("Recently tidied").strong());
                        ui.add_space(4.0);
                        for (i, m) in settings.log.iter().enumerate().rev().take(15) {
                            ui.horizontal(|ui| {
                                let name = m.to.file_name().unwrap_or_default().to_string_lossy();
                                let dir =
                                    m.to.parent()
                                        .and_then(|d| d.file_name())
                                        .unwrap_or_default()
                                        .to_string_lossy();
                                ui.add(egui::Label::new(format!("{name}  »  {dir}")).truncate())
                                    .on_hover_text(m.to.display().to_string());
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("Undo").clicked() {
                                            undo_index = Some(i);
                                        }
                                    },
                                );
                            });
                        }
                    });
                }
            });
        if serde_json::to_string(&settings).unwrap_or_default() != before {
            config::write(FILE, &settings);
            state().settings = settings;
        }
        if let Some(i) = undo_index
            && let Err(error) = undo(i)
        {
            desktop::notify("Could not undo", &error);
        }
        if let Some(i) = pick_for {
            let (sender, receiver) = std::sync::mpsc::channel();
            self.picked = Some(receiver);
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Where should they go?")
                    .pick_folder()
                {
                    let _ = sender.send((i, dir));
                    ctx.request_repaint();
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tidies a made-up Downloads folder (never the real one).
    #[test]
    fn tidies_a_fake_downloads_folder() {
        let root = std::env::temp_dir().join(format!("toolswitch-organize-{}", std::process::id()));
        let downloads = root.join("dl");
        let docs = root.join("docs");
        std::fs::create_dir_all(&downloads).unwrap();

        // Tidied "an hour from now": everything is an hour old except fresh.pdf, changed then.
        let later = SystemTime::now() + Duration::from_secs(3600);
        for name in ["a.pdf", "b.PDF", "c.zip", "d.pdf.part", "fresh.pdf", "desktop.ini", "e.xyz"] {
            let f = downloads.join(name);
            std::fs::write(&f, b"x").unwrap();
            if name == "fresh.pdf" {
                std::fs::File::options()
                    .write(true)
                    .open(&f)
                    .unwrap()
                    .set_modified(later)
                    .unwrap();
            }
        }
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("a.pdf"), b"already here").unwrap();

        let mut settings = Settings {
            rules: vec![
                Rule {
                    name: "Documents".into(),
                    on: true,
                    exts: "pdf".into(),
                    dest: docs.clone(),
                },
                Rule {
                    name: "Archives".into(),
                    on: false,
                    exts: "zip".into(),
                    dest: root.join("archives"),
                },
            ],
            // Anything older than a day without a rule would go to the trash; nothing here
            // is that old, so only what the rules say happens.
            trash_after: 1,
            log: vec![],
        };
        let (moves, count) = tidy(&downloads, &settings, later);
        assert_eq!(count, 2);
        settings.log = moves;
        assert!(docs.join("a (2).pdf").exists());
        assert!(docs.join("b.PDF").exists());
        // Still downloading, too new, or no rule for it: left alone.
        assert!(downloads.join("d.pdf.part").exists());
        assert!(downloads.join("fresh.pdf").exists());
        assert!(downloads.join("c.zip").exists(), "its rule is off: left alone");
        assert!(downloads.join("desktop.ini").exists());
        assert!(downloads.join("e.xyz").exists(), "an hour old, not a day");
        assert_eq!(settings.log.len(), 2);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
