// Breaks: every so often a notification says to rest your eyes or stand up. Two ways:
// "Eyes" (every 20 min look 20 s at something far away) and "Pomodoro" (25 min work, 5 min
// break, a long one every 4). The count starts over after the computer sleeps, but not when
// the app just restarts.

use crate::{config, desktop, theme};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

const FILE: &str = "breaks.json";
/// Where the count is, so restarting the app doesn't start it over.
const COUNT_FILE: &str = "breaks-count.json";
/// Closed longer than this, the count starts over: nobody was at the computer.
const RESUME_WITHIN: i64 = 120;

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Mode {
    Eyes,
    Pomodoro,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    mode: Mode,
    /// Eyes: minutes between reminders.
    every: u64,
    work: u64,
    short_break: u64,
    long_break: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            mode: Mode::Eyes,
            every: 20,
            work: 25,
            short_break: 5,
            long_break: 15,
        }
    }
}

/// The count on disk, in seconds since 1970 (an Instant means nothing to the next run).
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedCount {
    until: i64,
    on_break: bool,
    done: u32,
    paused_until: Option<i64>,
    saved_at: i64,
}

fn unix_now() -> i64 {
    chrono::Local::now().timestamp()
}

fn to_unix(t: Instant) -> i64 {
    let now = Instant::now();
    if t >= now {
        unix_now() + (t - now).as_secs() as i64
    } else {
        unix_now() - (now - t).as_secs() as i64
    }
}

fn from_unix(secs: i64) -> Instant {
    Instant::now() + Duration::from_secs((secs - unix_now()).max(0) as u64)
}

/// Where the count is.
struct Run {
    settings: Settings,
    /// When the current stretch (work or break) ends.
    until: Instant,
    on_break: bool,
    /// Pomodoro: work stretches done, for the long break.
    done: u32,
    paused_until: Option<Instant>,
    /// The last turn the background thread gave it. None or long ago: it was off until now.
    last_tick: Option<Instant>,
}

impl Run {
    fn restart(&mut self) {
        let work = match self.settings.mode {
            Mode::Eyes => self.settings.every,
            Mode::Pomodoro => self.settings.work,
        };
        self.until = Instant::now() + Duration::from_secs(work * 60);
        self.on_break = false;
    }
}

fn run() -> MutexGuard<'static, Run> {
    static RUN: OnceLock<Mutex<Run>> = OnceLock::new();
    RUN.get_or_init(|| {
        let mut run = Run {
            settings: config::read(FILE),
            until: Instant::now(),
            on_break: false,
            done: 0,
            paused_until: None,
            last_tick: None,
        };
        let saved: SavedCount = config::read(COUNT_FILE);
        if (0..=RESUME_WITHIN).contains(&(unix_now() - saved.saved_at)) {
            run.until = from_unix(saved.until);
            run.on_break = saved.on_break;
            run.done = saved.done;
            run.paused_until = saved.paused_until.map(from_unix);
            run.last_tick = Some(Instant::now());
        } else {
            run.restart();
        }
        Mutex::new(run)
    })
    .lock()
    .unwrap_or_else(|e| e.into_inner())
}

pub fn tick(woke_up: bool) -> bool {
    let changed = step(woke_up);
    let run = run();
    config::write(
        COUNT_FILE,
        &SavedCount {
            until: to_unix(run.until),
            on_break: run.on_break,
            done: run.done,
            paused_until: run.paused_until.map(to_unix),
            saved_at: unix_now(),
        },
    );
    changed
}

fn step(woke_up: bool) -> bool {
    let mut run = run();
    let now = Instant::now();
    if let Some(until) = run.paused_until {
        if now < until {
            return false;
        }
        run.paused_until = None;
        run.restart();
        return true;
    }
    // Just turned on, or the computer slept: the count starts now.
    let fresh = run
        .last_tick
        .is_none_or(|t| now.duration_since(t) > Duration::from_secs(60));
    run.last_tick = Some(now);
    if woke_up || fresh {
        run.restart();
        return true;
    }
    if now < run.until {
        return false;
    }
    let s = run.settings.clone();
    match (s.mode, run.on_break) {
        (Mode::Eyes, _) => {
            desktop::notify(
                "👀 Rest your eyes",
                "Look at something far away for 20 seconds.",
            );
            run.restart();
        }
        (Mode::Pomodoro, false) => {
            run.done += 1;
            let long = run.done.is_multiple_of(4);
            let minutes = if long { s.long_break } else { s.short_break };
            desktop::notify(
                "☕ Time for a break",
                &format!("Stand up and move for {minutes} minutes."),
            );
            run.on_break = true;
            run.until = now + Duration::from_secs(minutes * 60);
        }
        (Mode::Pomodoro, true) => {
            desktop::notify(
                "🍅 Back to work",
                &format!("Next break in {} minutes.", s.work),
            );
            run.restart();
        }
    }
    true
}

/// "Next break in 12 min", for the screen and the tray.
pub fn status() -> String {
    if !config::enabled("breaks") {
        return "Breaks are off".into();
    }
    let run = run();
    if let Some(until) = run.paused_until {
        return format!("Paused · back in {}", left(until));
    }
    if run.on_break {
        format!("On a break · back to work in {}", left(run.until))
    } else {
        format!("Next break in {}", left(run.until))
    }
}

fn left(until: Instant) -> String {
    let secs = until.saturating_duration_since(Instant::now()).as_secs();
    if secs >= 3600 {
        format!("{} h {} min", secs / 3600, secs % 3600 / 60)
    } else if secs >= 60 {
        format!("{} min", secs.div_ceil(60))
    } else {
        format!("{secs} s")
    }
}

pub fn pause(minutes: u64) {
    run().paused_until = Some(Instant::now() + Duration::from_secs(minutes * 60));
}

pub fn take_now() {
    run().until = Instant::now();
}

#[derive(Default)]
pub struct BreaksPage;

impl crate::Page for BreaksPage {
    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.ctx().request_repaint_after(Duration::from_secs(1));
        theme::page_header_with_switch(
            ui,
            "Breaks",
            "A reminder to rest your eyes or stand up, so long sessions hurt less.",
            "breaks",
        );
        ui.add_space(16.0);

        let on = config::enabled("breaks");
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(status()).size(20.0).strong());
            ui.add_space(8.0);
            if on {
                ui.horizontal(|ui| {
                    if ui.button("Take it now").clicked() {
                        take_now();
                    }
                    if ui
                        .button("Skip")
                        .on_hover_text("Start the count over")
                        .clicked()
                    {
                        run().restart();
                    }
                    for (label, minutes) in [("Pause 1 h", 60), ("Pause 3 h", 180)] {
                        if ui.button(label).clicked() {
                            pause(minutes);
                        }
                    }
                });
            } else {
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("Turn on").color(egui::Color32::WHITE))
                            .fill(theme::ACCENT),
                    )
                    .clicked()
                {
                    config::set_enabled("breaks", true);
                }
            }
        });
        ui.add_space(12.0);

        let mut run = run();
        let mut settings = run.settings.clone();
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.radio_value(&mut settings.mode, Mode::Eyes, "Eyes");
                ui.add_space(12.0);
                ui.radio_value(&mut settings.mode, Mode::Pomodoro, "Pomodoro");
            });
            ui.add_space(8.0);
            match settings.mode {
                Mode::Eyes => {
                    ui.label(
                        egui::RichText::new(
                            "The 20-20-20 rule: every 20 minutes, look 20 seconds at something 6 meters away.",
                        )
                        .color(theme::MUTED),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Every");
                        minutes_box(ui, "every", &mut settings.every, &[15, 20, 30, 45, 60]);
                    });
                }
                Mode::Pomodoro => {
                    ui.label(
                        egui::RichText::new(
                            "Work, then a short break. Every 4 rounds, a longer one.",
                        )
                        .color(theme::MUTED),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Work");
                        minutes_box(ui, "work", &mut settings.work, &[15, 25, 45, 50, 60, 90]);
                        ui.label("Break");
                        minutes_box(ui, "short", &mut settings.short_break, &[3, 5, 10, 15]);
                        ui.label("Long break");
                        minutes_box(ui, "long", &mut settings.long_break, &[10, 15, 20, 30]);
                    });
                }
            }
        });
        let changed = settings.mode != run.settings.mode
            || settings.every != run.settings.every
            || settings.work != run.settings.work
            || settings.short_break != run.settings.short_break
            || settings.long_break != run.settings.long_break;
        if changed {
            config::write(FILE, &settings);
            run.settings = settings;
            run.done = 0;
            run.restart();
        }
    }
}

fn minutes_box(ui: &mut egui::Ui, id: &str, value: &mut u64, options: &[u64]) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(format!("{value} min"))
        .width(80.0)
        .show_ui(ui, |ui| {
            for &m in options {
                ui.selectable_value(value, m, format!("{m} min"));
            }
        });
}
