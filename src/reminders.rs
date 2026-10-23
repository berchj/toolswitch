// Quick reminders: write "in 20 min call Juan" or "at 15:30 meeting" and a notification shows
// up at that time. No calendar, no account. Saved in reminders.json.

use crate::{config, desktop, theme};
use chrono::{DateTime, Duration, Local, NaiveTime, TimeZone};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, MutexGuard, OnceLock};

const FILE: &str = "reminders.json";
/// Past reminders stay in the list this long, faded.
const KEEP_DONE: i64 = 24 * 3600;

#[derive(Clone, Serialize, Deserialize)]
pub struct Reminder {
    pub text: String,
    /// Unix time, in seconds.
    pub at: i64,
    pub done: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    reminders: Vec<Reminder>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| Mutex::new(config::read(FILE)))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn save(state: &State) {
    config::write(FILE, state);
}

pub fn add(text: String, at: DateTime<Local>) {
    let mut state = state();
    state.reminders.push(Reminder {
        text,
        at: at.timestamp(),
        done: false,
    });
    state.reminders.sort_by_key(|r| r.at);
    save(&state);
}

/// The next one that hasn't gone off, for the tray.
pub fn next() -> Option<Reminder> {
    state().reminders.iter().find(|r| !r.done).cloned()
}

/// Called by the background thread: shows the ones that are due.
pub fn tick() -> bool {
    let now = Local::now().timestamp();
    let mut state = state();
    let mut changed = false;
    for r in state
        .reminders
        .iter_mut()
        .filter(|r| !r.done && r.at <= now)
    {
        desktop::notify(&format!("⏰ {}", r.text), "Reminder from toolswitch");
        r.done = true;
        changed = true;
    }
    let before = state.reminders.len();
    state
        .reminders
        .retain(|r| !r.done || now - r.at < KEEP_DONE);
    changed |= state.reminders.len() != before;
    if changed {
        save(&state);
    }
    changed
}

// ---------- reading "in 20 min call Juan" ----------

/// The time and the text of a reminder written in plain words, in English or Spanish:
/// "in 20 min call Juan", "20m call Juan", "en 2 horas", "at 15:30 meeting",
/// "a las 9 dentista", "tomorrow 9:00 dentist", "mañana a las 8 gym".
pub fn parse(input: &str, now: DateTime<Local>) -> Option<(DateTime<Local>, String)> {
    let words: Vec<&str> = input.split_whitespace().collect();
    let lower: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
    let mut i = 0;
    let mut tomorrow = false;
    if matches!(
        lower.first().map(String::as_str),
        Some("tomorrow" | "mañana" | "manana")
    ) {
        tomorrow = true;
        i = 1;
    }
    let at_word = |w: &str| matches!(w, "at" | "a" | "las" | "@");
    while lower
        .get(i)
        .is_some_and(|w| at_word(w) || w == "in" || w == "en")
    {
        i += 1;
    }
    let first = lower.get(i)?;

    // A duration: "20 min", "20m", "1h", "2 horas", "1.5 h". After "at" / "a las" it's a time.
    let previous = i.checked_sub(1).map(|p| lower[p].as_str());
    let explicit_at = previous.is_some_and(at_word);
    if !tomorrow
        && !explicit_at
        && let Some((amount, unit, used)) =
            split_amount(first, lower.get(i + 1).map(String::as_str))
    {
        if let Some(unit_secs) = unit.and_then(unit_seconds) {
            let at = later(now, amount * unit_secs as f64)?;
            return Some((at, rest(&words, i + used)));
        }
        if unit.is_none() && matches!(previous, Some("in" | "en")) {
            // "in 20 call Juan": minutes
            let at = later(now, amount * 60.0)?;
            return Some((at, rest(&words, i + 1)));
        }
    }

    // A time of day: "15:30", "9", "9am", "9:15pm", "21h"
    let (time, used) = clock_time(first, lower.get(i + 1).map(String::as_str))?;
    let mut day = now.date_naive();
    if tomorrow {
        day = day.succ_opt()?;
    }
    let mut at = Local.from_local_datetime(&day.and_time(time)).earliest()?;
    if !tomorrow && at <= now {
        at += Duration::days(1);
    }
    Some((at, rest(&words, i + used)))
}

/// `now` plus some seconds; None when it's absurd (over ten years), so a huge number typed
/// by mistake can't overflow.
fn later(now: DateTime<Local>, seconds: f64) -> Option<DateTime<Local>> {
    if !(0.0..=10.0 * 365.0 * 86400.0).contains(&seconds) {
        return None;
    }
    now.checked_add_signed(Duration::try_seconds(seconds.round() as i64)?)
}

fn rest(words: &[&str], from: usize) -> String {
    let text = words.get(from..).unwrap_or_default().join(" ");
    // "to call Juan" / "para llamar" read better without the joining word.
    let text = text.trim();
    for joiner in ["to ", "para ", "que "] {
        if let Some(stripped) = text.strip_prefix(joiner) {
            return stripped.to_string();
        }
    }
    text.to_string()
}

/// "20" + "min" -> (20, Some("min"), 2); "20m" -> (20, Some("m"), 1); "15:30" -> None
fn split_amount<'a>(word: &'a str, next: Option<&'a str>) -> Option<(f64, Option<&'a str>, usize)> {
    if word.contains(':') {
        return None;
    }
    let digits_end = word
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .unwrap_or(word.len());
    let amount: f64 = word[..digits_end].replace(',', ".").parse().ok()?;
    let suffix = &word[digits_end..];
    if !suffix.is_empty() {
        return Some((amount, Some(suffix), 1));
    }
    match next {
        Some(unit) if unit_seconds(unit).is_some() => Some((amount, Some(unit), 2)),
        _ => Some((amount, None, 1)),
    }
}

fn unit_seconds(unit: &str) -> Option<i64> {
    let unit = unit.trim_end_matches(['.', ',']);
    Some(match unit {
        "s" | "sec" | "secs" | "second" | "seconds" | "seg" | "segundo" | "segundos" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" | "minuto" | "minutos" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" | "hora" | "horas" => 3600,
        "d" | "day" | "days" | "dia" | "día" | "dias" | "días" => 86400,
        _ => return None,
    })
}

/// "15:30" | "9" | "9am" | "9:15pm" | "21h" (+ "pm" as the next word) -> the time and words used.
fn clock_time(word: &str, next: Option<&str>) -> Option<(NaiveTime, usize)> {
    let mut used = 1;
    let mut text = word.to_string();
    if let Some(next) = next
        && matches!(next, "am" | "pm" | "hs" | "h")
    {
        text.push_str(next);
        used = 2;
    }
    let (body, pm, am) = if let Some(b) = text.strip_suffix("pm") {
        (b.to_string(), true, false)
    } else if let Some(b) = text.strip_suffix("am") {
        (b.to_string(), false, true)
    } else {
        (
            text.trim_end_matches("hs")
                .trim_end_matches('h')
                .to_string(),
            false,
            false,
        )
    };
    let (h, m) = match body.split_once([':', '.']) {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (body.parse::<u32>().ok()?, 0),
    };
    let h = match (h, pm, am) {
        (12, false, true) => 0,
        (h, true, _) if h < 12 => h + 12,
        (h, _, _) => h,
    };
    Some((NaiveTime::from_hms_opt(h, m, 0)?, used))
}

/// "today 15:30", "tomorrow 9:00", "Fri 9:00", plus "in 20 min" when it's close.
pub fn when(at: i64) -> String {
    let Some(at) = Local.timestamp_opt(at, 0).single() else {
        return String::new();
    };
    let now = Local::now();
    let day = if at.date_naive() == now.date_naive() {
        "today".to_string()
    } else if Some(at.date_naive()) == now.date_naive().succ_opt() {
        "tomorrow".to_string()
    } else {
        at.format("%a %d %b").to_string()
    };
    let left = at - now;
    let soon = if left > Duration::zero() && left < Duration::hours(3) {
        let mins = (left.num_seconds() + 59) / 60;
        if mins >= 60 {
            format!(" · in {} h {} min", mins / 60, mins % 60)
        } else {
            format!(" · in {mins} min")
        }
    } else {
        String::new()
    };
    format!("{day} {}{soon}", at.format("%H:%M"))
}

// ---------- the screen ----------

#[derive(Default)]
pub struct RemindersPage {
    input: String,
    focus: bool,
    error: bool,
}

impl crate::Page for RemindersPage {
    fn opened(&mut self) {
        self.focus = true;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(20));
        theme::page_header_with_switch(
            ui,
            "Reminders",
            "Write what and when. A notification shows up at that time.",
            "reminders",
        );
        ui.add_space(16.0);

        let now = Local::now();
        let parsed = parse(&self.input, now);
        let mut add_now = false;
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .hint_text("in 20 min call Juan  ·  at 15:30 meeting  ·  tomorrow 9:00 dentist")
                    .desired_width(ui.available_width() - 70.0)
                    .margin(egui::Margin::symmetric(10, 8)),
            );
            if std::mem::take(&mut self.focus) {
                edit.request_focus();
            }
            if edit.changed() {
                self.error = false;
            }
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Add").clicked() || enter {
                add_now = true;
            }
        });
        ui.add_space(4.0);
        let hint = match &parsed {
            Some((at, text)) => egui::RichText::new(format!(
                "⏰ {}  ·  {}",
                when(at.timestamp()),
                if text.is_empty() { "Reminder" } else { text }
            ))
            .color(theme::ACCENT),
            None if self.error => egui::RichText::new(
                "Say when: \"in 20 min …\", \"at 15:30 …\" or \"tomorrow 9:00 …\"",
            )
            .color(theme::WARNING),
            None => egui::RichText::new(" ").color(theme::MUTED),
        };
        ui.label(hint.size(13.0));

        // Quick ones for the text already written.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Quick:").size(13.0).color(theme::FAINT));
            for (label, mins) in [("5 min", 5), ("15 min", 15), ("30 min", 30), ("1 hour", 60)] {
                if ui.small_button(label).clicked() {
                    let text = if self.input.trim().is_empty() {
                        "Reminder".to_string()
                    } else {
                        self.input.trim().to_string()
                    };
                    add(text, now + Duration::minutes(mins));
                    self.input.clear();
                }
            }
        });

        if add_now {
            match parsed {
                Some((at, text)) => {
                    add(
                        if text.is_empty() {
                            "Reminder".into()
                        } else {
                            text
                        },
                        at,
                    );
                    self.input.clear();
                    self.focus = true;
                }
                None => self.error = true,
            }
        }

        ui.add_space(16.0);
        let reminders = state().reminders.clone();
        if reminders.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("⏰").size(32.0).color(theme::FAINT));
                ui.label(egui::RichText::new("No reminders").size(16.0));
                ui.label(
                    egui::RichText::new("They also work from the shortcut, from anywhere.")
                        .color(theme::MUTED),
                );
            });
            return;
        }
        let mut remove = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, r) in reminders.iter().enumerate() {
                    theme::card()
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let color = if r.done { theme::FAINT } else { theme::TEXT };
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    ui.add(
                                        egui::Label::new(egui::RichText::new(&r.text).color(color))
                                            .truncate(),
                                    );
                                    let when_text = if r.done {
                                        format!("Done · {}", when(r.at))
                                    } else {
                                        when(r.at)
                                    };
                                    ui.label(
                                        egui::RichText::new(when_text).size(12.0).color(
                                            if r.done { theme::FAINT } else { theme::ACCENT },
                                        ),
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("🗑").on_hover_text("Delete").clicked()
                                        {
                                            remove = Some(i);
                                        }
                                    },
                                );
                            });
                        });
                    ui.add_space(6.0);
                }
            });
        if let Some(i) = remove {
            let mut state = state();
            if i < state.reminders.len() {
                state.reminders.remove(i);
                save(&state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    fn at(h: u32, m: u32) -> DateTime<Local> {
        Local
            .from_local_datetime(&Local::now().date_naive().and_hms_opt(h, m, 0).unwrap())
            .unwrap()
    }

    #[test]
    fn parses() {
        let now = at(10, 0);
        let (t, text) = parse("in 20 min call Juan", now).unwrap();
        assert_eq!((t - now).num_minutes(), 20);
        assert_eq!(text, "call Juan");
        let (t, text) = parse("en 2 horas llamar a mamá", now).unwrap();
        assert_eq!((t - now).num_minutes(), 120);
        assert_eq!(text, "llamar a mamá");
        let (t, _) = parse("20m tea", now).unwrap();
        assert_eq!((t - now).num_minutes(), 20);
        let (t, text) = parse("at 15:30 meeting", now).unwrap();
        assert_eq!((t.hour(), t.minute()), (15, 30));
        assert_eq!(text, "meeting");
        let (t, text) = parse("a las 9 dentista", now).unwrap();
        assert_eq!((t.hour(), (t - now).num_hours()), (9, 23));
        assert_eq!(text, "dentista");
        let (t, _) = parse("tomorrow 9:00 dentist", now).unwrap();
        assert_eq!(
            (t.hour(), t.date_naive()),
            (9, now.date_naive().succ_opt().unwrap())
        );
        let (t, _) = parse("mañana a las 8pm gym", now).unwrap();
        assert_eq!(t.hour(), 20);
        let (t, text) = parse("at 3 pm call", now).unwrap();
        assert_eq!((t.hour(), text.as_str()), (15, "call"));
        assert!(parse("call Juan", now).is_none());
        assert!(parse("in 999999999999 min call", now).is_none());
        assert!(parse("in 9999999999999999999999 h", now).is_none());
        assert!(parse("", now).is_none());
    }
}
