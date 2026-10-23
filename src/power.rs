// Shut down timer: shut down, restart or sleep in a while, at a time, or when the downloads
// finish. A notification warns a minute before, and it can be canceled until then.

use crate::{config, desktop, theme};
use chrono::{Local, NaiveTime, TimeZone};
use eframe::egui;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Copy, PartialEq)]
pub enum Action {
    ShutDown,
    Restart,
    Sleep,
}

impl Action {
    fn label(self) -> &'static str {
        match self {
            Action::ShutDown => "Shut down",
            Action::Restart => "Restart",
            Action::Sleep => "Sleep",
        }
    }
}

#[derive(Clone, Copy)]
enum When {
    /// The clock time, so time asleep counts (a monotonic clock stops while suspended).
    At(SystemTime),
    /// Once nothing has been downloading for a couple of minutes.
    DownloadsDone {
        quiet_since: Option<Instant>,
    },
}

struct Timer {
    action: Action,
    when: When,
    warned: bool,
}

fn timer() -> MutexGuard<'static, Option<Timer>> {
    static TIMER: OnceLock<Mutex<Option<Timer>>> = OnceLock::new();
    TIMER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

pub fn set_in(action: Action, minutes: u64) {
    *timer() = Some(Timer {
        action,
        when: When::At(SystemTime::now() + Duration::from_secs(minutes * 60)),
        warned: false,
    });
}

pub fn cancel() {
    *timer() = None;
}

/// Files still being downloaded by a browser have these endings.
const PARTIAL: &[&str] = &[
    "part",
    "crdownload",
    "download",
    "partial",
    "tmp",
    "opdownload",
];

pub fn downloads_dir() -> PathBuf {
    config::user_dir("DOWNLOAD", "Downloads")
}

/// A file a browser is still downloading.
pub fn is_partial(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|x| x.to_str())
        .is_some_and(|x| PARTIAL.contains(&x.to_lowercase().as_str()))
}

/// A download in progress: a partial file that changed in the last few minutes. An old one
/// left behind by a canceled download doesn't count.
fn downloading() -> bool {
    let recent = |e: &std::fs::DirEntry| {
        e.metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(600))
    };
    std::fs::read_dir(downloads_dir())
        .is_ok_and(|entries| entries.flatten().any(|e| is_partial(&e.path()) && recent(&e)))
}

pub fn tick() -> bool {
    let mut guard = timer();
    let Some(t) = guard.as_mut() else {
        return false;
    };
    let now = Instant::now();
    let wall = SystemTime::now();
    let due = match &mut t.when {
        When::At(at) => {
            if !t.warned && *at <= wall + Duration::from_secs(60) {
                t.warned = true;
                // Woken up after the time: there's still a minute to cancel.
                *at = (*at).max(wall + Duration::from_secs(60));
                desktop::notify(
                    &format!("⏻ {} in 1 minute", t.action.label()),
                    "Open toolswitch to cancel. Save your work.",
                );
            }
            *at <= wall
        }
        When::DownloadsDone { quiet_since } => {
            if downloading() {
                *quiet_since = None;
                false
            } else {
                let since = *quiet_since.get_or_insert(now);
                // Two quiet minutes: done. The warning gives one more to cancel.
                if now.duration_since(since) >= Duration::from_secs(120) {
                    let action = t.action;
                    t.when = When::At(wall + Duration::from_secs(60));
                    t.warned = true;
                    desktop::notify(
                        &format!("⏻ Downloads finished: {} in 1 minute", action.label()),
                        "Open toolswitch to cancel.",
                    );
                }
                false
            }
        }
    };
    if due {
        let action = t.action;
        *guard = None;
        drop(guard);
        if let Err(error) = run(action) {
            desktop::notify("Could not do it", &error);
        }
    }
    true
}

/// The command for each system. On Linux systemd asks no password for the user at the screen.
fn run(action: Action) -> Result<(), String> {
    let (program, args): (&str, &[&str]) = if cfg!(windows) {
        match action {
            Action::ShutDown => ("shutdown", &["/s", "/t", "0"]),
            Action::Restart => ("shutdown", &["/r", "/t", "0"]),
            Action::Sleep => ("rundll32.exe", &["powrprof.dll,SetSuspendState", "0,1,0"]),
        }
    } else if cfg!(target_os = "macos") {
        match action {
            Action::ShutDown => (
                "osascript",
                &["-e", "tell application \"System Events\" to shut down"],
            ),
            Action::Restart => (
                "osascript",
                &["-e", "tell application \"System Events\" to restart"],
            ),
            Action::Sleep => ("pmset", &["sleepnow"]),
        }
    } else {
        match action {
            Action::ShutDown => ("systemctl", &["poweroff"]),
            Action::Restart => ("systemctl", &["reboot"]),
            Action::Sleep => ("systemctl", &["suspend"]),
        }
    };
    let status = crate::desktop::command(program)
        .args(args)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed"))
    }
}

/// "Shut down in 42 min", for the screen and the tray.
pub fn status() -> Option<String> {
    let guard = timer();
    let t = guard.as_ref()?;
    Some(match t.when {
        When::At(at) => {
            let secs = at
                .duration_since(SystemTime::now())
                .unwrap_or_default()
                .as_secs();
            let left = if secs >= 3600 {
                format!("{} h {} min", secs / 3600, secs % 3600 / 60)
            } else if secs >= 60 {
                format!("{} min", secs.div_ceil(60))
            } else {
                format!("{secs} s")
            };
            format!("{} in {left}", t.action.label())
        }
        When::DownloadsDone { .. } => format!("{} when the downloads finish", t.action.label()),
    })
}

pub struct PowerPage {
    action: Action,
    time: String,
}

impl Default for PowerPage {
    fn default() -> Self {
        PowerPage {
            action: Action::ShutDown,
            time: "23:00".into(),
        }
    }
}

impl crate::Page for PowerPage {
    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.ctx().request_repaint_after(Duration::from_secs(1));
        theme::page_header(
            ui,
            "Shut down timer",
            "Turn the computer off, restart it or put it to sleep later. You can cancel until the last minute.",
        );
        ui.add_space(16.0);

        if let Some(status) = status() {
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("🔌  {status}"))
                            .size(20.0)
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Cancel").color(egui::Color32::WHITE),
                                )
                                .fill(theme::ACCENT),
                            )
                            .clicked()
                        {
                            cancel();
                        }
                    });
                });
            });
            ui.add_space(12.0);
        }

        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                for action in [Action::ShutDown, Action::Restart, Action::Sleep] {
                    ui.radio_value(&mut self.action, action, action.label());
                    ui.add_space(8.0);
                }
            });
            ui.add_space(10.0);
            ui.label(egui::RichText::new("In").color(theme::MUTED));
            ui.horizontal_wrapped(|ui| {
                for (label, minutes) in [
                    ("15 min", 15),
                    ("30 min", 30),
                    ("1 hour", 60),
                    ("2 hours", 120),
                    ("3 hours", 180),
                ] {
                    if ui.button(label).clicked() {
                        set_in(self.action, minutes);
                    }
                }
            });
            ui.add_space(8.0);
            ui.label(egui::RichText::new("At").color(theme::MUTED));
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.time)
                        .desired_width(70.0)
                        .hint_text("23:00"),
                );
                let parsed = NaiveTime::parse_from_str(self.time.trim(), "%H:%M").ok();
                if ui
                    .add_enabled(parsed.is_some(), egui::Button::new("Set"))
                    .clicked()
                    && let Some(time) = parsed
                {
                    let now = Local::now();
                    let mut at = Local
                        .from_local_datetime(&now.date_naive().and_time(time))
                        .earliest()
                        .unwrap_or(now);
                    if at <= now {
                        at += chrono::Duration::days(1);
                    }
                    let minutes = ((at - now).num_seconds().max(0) as u64).div_ceil(60);
                    set_in(self.action, minutes);
                }
            });
            ui.add_space(8.0);
            if ui
                .button("📥 When the downloads finish")
                .on_hover_text(format!(
                    "Waits until nothing is being downloaded in {}",
                    downloads_dir().display()
                ))
                .clicked()
            {
                *timer() = Some(Timer {
                    action: self.action,
                    when: When::DownloadsDone { quiet_since: None },
                    warned: false,
                });
            }
        });
    }
}
