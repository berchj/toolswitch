// Updates: one button that updates the system and the apps with whatever this computer uses
// (apt, dnf, pacman, Flatpak, Snap on Linux; winget on Windows; Homebrew and Software Update
// on macOS). Once a day it checks and says if there's something new. When an update needs
// the administrator password, the system asks for it.

use crate::{config, desktop, theme};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

const FILE: &str = "updates.json";
const CHECK_EVERY: i64 = 24 * 3600;
const LOG_LINES: usize = 300;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Manager {
    Apt,
    Dnf,
    Pacman,
    Flatpak,
    Snap,
    Winget,
    Brew,
    SoftwareUpdate,
}

impl Manager {
    const ALL: [Manager; 8] = [
        Manager::Apt,
        Manager::Dnf,
        Manager::Pacman,
        Manager::Flatpak,
        Manager::Snap,
        Manager::Winget,
        Manager::Brew,
        Manager::SoftwareUpdate,
    ];

    fn name(self) -> &'static str {
        match self {
            Manager::Apt => "System (apt)",
            Manager::Dnf => "System (dnf)",
            Manager::Pacman => "System (pacman)",
            Manager::Flatpak => "Flatpak apps",
            Manager::Snap => "Snap apps",
            Manager::Winget => "Windows apps (winget)",
            Manager::Brew => "Homebrew",
            Manager::SoftwareUpdate => "macOS",
        }
    }

    fn program(self) -> &'static str {
        match self {
            Manager::Apt => "apt-get",
            Manager::Dnf => "dnf",
            Manager::Pacman => "pacman",
            Manager::Flatpak => "flatpak",
            Manager::Snap => "snap",
            Manager::Winget => "winget",
            Manager::Brew => "brew",
            Manager::SoftwareUpdate => "softwareupdate",
        }
    }

    fn installed(self) -> bool {
        which(self.program())
    }

    /// How many updates are waiting, without changing anything.
    fn pending(self) -> Option<usize> {
        let lines = |program: &str, args: &[&str]| -> Option<Vec<String>> {
            // In English whatever the system's language: the output is read word by word.
            let out = crate::desktop::command(program)
                .args(args)
                .env("LC_ALL", "C")
                .env("LANG", "C")
                .output()
                .ok()?;
            Some(
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect(),
            )
        };
        Some(match self {
            Manager::Apt => lines("apt", &["list", "--upgradable"])?
                .iter()
                .filter(|l| l.contains("[upgradable"))
                .count(),
            Manager::Dnf => lines("dnf", &["-q", "check-update"])?
                .iter()
                .filter(|l| !l.trim().is_empty())
                .count(),
            Manager::Pacman => {
                if !which("checkupdates") {
                    return None;
                }
                lines("checkupdates", &[])?.len()
            }
            Manager::Flatpak => lines(
                "flatpak",
                &["remote-ls", "--updates", "--columns=application"],
            )?
            .iter()
            .filter(|l| !l.trim().is_empty())
            .count(),
            Manager::Snap => lines("snap", &["refresh", "--list"])?
                .iter()
                .skip(1)
                .filter(|l| !l.trim().is_empty())
                .count(),
            Manager::Winget => {
                let all = lines("winget", &["upgrade", "--accept-source-agreements"])?;
                // A table: the rows after the "-----" line, minus the summary at the end.
                let start = all.iter().position(|l| l.starts_with("---"))? + 1;
                all[start..]
                    .iter()
                    .filter(|l| !l.trim().is_empty() && !l.contains("upgrades available"))
                    .count()
            }
            Manager::Brew => lines("brew", &["outdated", "--quiet"])?
                .iter()
                .filter(|l| !l.trim().is_empty())
                .count(),
            Manager::SoftwareUpdate => lines("softwareupdate", &["-l"])?
                .iter()
                .filter(|l| l.trim_start().starts_with("* Label:"))
                .count(),
        })
    }

    /// The command that installs the updates.
    fn update_command(self) -> (String, Vec<String>) {
        let s = |items: &[&str]| items.iter().map(|i| i.to_string()).collect::<Vec<_>>();
        match self {
            Manager::Apt => (
                "pkexec".into(),
                s(&[
                    "sh",
                    "-c",
                    "apt-get update && DEBIAN_FRONTEND=noninteractive apt-get -y upgrade",
                ]),
            ),
            Manager::Dnf => ("pkexec".into(), s(&["dnf", "-y", "upgrade"])),
            Manager::Pacman => ("pkexec".into(), s(&["pacman", "-Syu", "--noconfirm"])),
            Manager::Flatpak => ("flatpak".into(), s(&["update", "-y", "--noninteractive"])),
            Manager::Snap => ("pkexec".into(), s(&["snap", "refresh"])),
            Manager::Winget => (
                "winget".into(),
                s(&[
                    "upgrade",
                    "--all",
                    "--silent",
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                ]),
            ),
            Manager::Brew => ("brew".into(), s(&["upgrade"])),
            Manager::SoftwareUpdate => (
                "osascript".into(),
                s(&[
                    "-e",
                    "do shell script \"softwareupdate -ia\" with administrator privileges",
                ]),
            ),
        }
    }
}

/// Is this program in the PATH? On macOS an app started from Finder or at login has a short
/// PATH, so Homebrew's folders are looked at too.
fn which(program: &str) -> bool {
    let exts: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ""]
    } else {
        &[""]
    };
    let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_default();
    if cfg!(target_os = "macos") {
        dirs.extend(["/opt/homebrew/bin".into(), "/usr/local/bin".into()]);
    }
    // winget is a Windows "app alias": a link that is_file() may not see, so exists() it is.
    dirs.iter().any(|dir| {
        exts.iter()
            .any(|ext| dir.join(format!("{program}{ext}")).exists())
    })
}

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    last_check: i64,
}

#[derive(Default)]
struct State {
    saved: Saved,
    /// Each manager found and its waiting updates (None: couldn't tell).
    found: Vec<(Manager, Option<usize>)>,
    log: Vec<String>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| {
            Mutex::new(State {
                saved: config::read(FILE),
                ..Default::default()
            })
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

static BUSY: AtomicBool = AtomicBool::new(false);

fn log(line: impl Into<String>) {
    let mut s = state();
    s.log.push(line.into());
    let extra = s.log.len().saturating_sub(LOG_LINES);
    s.log.drain(..extra);
}

/// Looks for updates in a thread. `tell`: a notification if there are some.
fn check(tell: bool, ctx: Option<egui::Context>) {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let found: Vec<(Manager, Option<usize>)> = Manager::ALL
            .into_iter()
            .filter(|m| m.installed())
            .map(|m| (m, m.pending()))
            .collect();
        let total: usize = found.iter().filter_map(|f| f.1).sum();
        {
            let mut s = state();
            s.found = found;
            s.saved.last_check = chrono::Local::now().timestamp();
            config::write(FILE, &s.saved);
        }
        if tell && total > 0 {
            desktop::notify(
                &format!("⟳ {total} updates available"),
                "Install them from toolswitch › Updates.",
            );
        }
        BUSY.store(false, Ordering::SeqCst);
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });
}

/// Installs the updates of these managers one after the other, showing what they print.
fn update(managers: Vec<Manager>, ctx: egui::Context) {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        for m in managers {
            let (program, args) = m.update_command();
            log(format!("━━ {} ━━", m.name()));
            let child = crate::desktop::command(&program)
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(error) => {
                    log(format!("Couldn't start {program}: {error}"));
                    continue;
                }
            };
            // Errors are read in their own thread so neither pipe fills up and blocks.
            let errors = child.stderr.take().map(|err| {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    for line in BufReader::new(err).lines().map_while(Result::ok) {
                        log(line);
                        ctx.request_repaint();
                    }
                })
            });
            if let Some(out) = child.stdout.take() {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    log(line);
                    ctx.request_repaint();
                }
            }
            if let Some(errors) = errors {
                let _ = errors.join();
            }
            match child.wait() {
                Ok(status) if status.success() => log("✔ Done"),
                Ok(_) => log("✖ It didn't finish (canceled, or the password was wrong?)"),
                Err(error) => log(format!("✖ {error}")),
            }
        }
        BUSY.store(false, Ordering::SeqCst);
        check(false, Some(ctx.clone()));
        ctx.request_repaint();
    });
}

pub fn tick() -> bool {
    let due = chrono::Local::now().timestamp() - state().saved.last_check >= CHECK_EVERY;
    if due {
        check(true, None);
    }
    due
}

#[derive(Default)]
pub struct UpdatesPage {
    looked: bool,
}

impl crate::Page for UpdatesPage {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // The first time the screen opens, it looks right away.
        if !self.looked {
            self.looked = true;
            if state().found.is_empty() {
                check(false, Some(ctx.clone()));
            }
        }
        let busy = BUSY.load(Ordering::SeqCst);
        if busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
        theme::page_header_with_switch(
            ui,
            "Updates",
            "Keeps the system and your apps up to date with one click. On, it checks once a day and tells you.",
            "updates",
        );
        ui.add_space(16.0);

        let (found, log_lines, last_check) = {
            let s = state();
            (s.found.clone(), s.log.clone(), s.saved.last_check)
        };
        let total: usize = found.iter().filter_map(|f| f.1).sum();
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let title = if busy && found.is_empty() {
                    "Looking for updates…".to_string()
                } else if total == 0 {
                    "✔ Everything is up to date".to_string()
                } else {
                    format!("{total} updates available")
                };
                ui.label(egui::RichText::new(title).size(18.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let all: Vec<Manager> = found
                        .iter()
                        .filter(|f| f.1 != Some(0))
                        .map(|f| f.0)
                        .collect();
                    if ui
                        .add_enabled(
                            !busy && total > 0,
                            egui::Button::new(
                                egui::RichText::new("Update all").color(egui::Color32::WHITE),
                            )
                            .fill(theme::ACCENT),
                        )
                        .clicked()
                    {
                        update(all, ctx.clone());
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("⟳ Check"))
                        .clicked()
                    {
                        check(false, Some(ctx.clone()));
                    }
                    if busy {
                        ui.add(egui::Spinner::new().color(theme::ACCENT));
                    }
                });
            });
            if last_check > 0 {
                ui.label(
                    egui::RichText::new(format!("Checked {}", crate::reminders::when(last_check)))
                        .size(12.0)
                        .color(theme::FAINT),
                );
            }
            ui.add_space(6.0);
            for (manager, pending) in &found {
                ui.horizontal(|ui| {
                    ui.label(manager.name());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if pending.is_some_and(|n| n > 0)
                            && ui.add_enabled(!busy, egui::Button::new("Update")).clicked()
                        {
                            update(vec![*manager], ctx.clone());
                        }
                        let text = match pending {
                            Some(0) => "up to date".to_string(),
                            Some(1) => "1 update".to_string(),
                            Some(n) => format!("{n} updates"),
                            None => "can't tell".to_string(),
                        };
                        ui.label(egui::RichText::new(text).size(13.0).color(theme::MUTED));
                    });
                });
            }
        });
        if !log_lines.is_empty() {
            ui.add_space(10.0);
            egui::Frame::new()
                .fill(theme::SIDEBAR)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(theme::RADIUS)
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for line in &log_lines {
                                ui.label(egui::RichText::new(line).monospace().size(12.0));
                            }
                        });
                });
        }
    }
}
