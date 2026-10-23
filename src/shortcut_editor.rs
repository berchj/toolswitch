// A row per shortcut: what it does and its keys, which record new ones when clicked.

use crate::{shortcuts, theme};
use eframe::egui;

#[derive(Default)]
pub struct ShortcutEditor {
    /// Shortcut whose new keys are being recorded.
    recording: Option<&'static str>,
    /// Shortcut whose last change failed.
    error: Option<&'static str>,
}

impl ShortcutEditor {
    pub fn record(&mut self, id: &'static str) {
        self.recording = Some(id);
        self.error = None;
    }

    /// Call once per frame before drawing: takes the keys while recording.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let Some(id) = self.recording else {
            return;
        };
        let ok = match shortcuts::record(ctx) {
            Some(shortcuts::Recorded::Keys(accel)) => shortcuts::set(id, Some(accel)),
            Some(shortcuts::Recorded::Default) => shortcuts::set(id, None),
            Some(shortcuts::Recorded::Cancel) => true,
            None => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                ctx.input_mut(|i| i.events.clear());
                return;
            }
        };
        self.recording = None;
        self.error = (!ok).then_some(id);
        ctx.input_mut(|i| i.events.clear());
    }

    /// What the shortcut does on the left, its keys on the right. Clicking the keys records new ones.
    pub fn row(&mut self, ui: &mut egui::Ui, id: &'static str, what: &str) {
        let recording = self.recording == Some(id);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(what).color(theme::MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::shortcut_keys(ui, &shortcuts::get(id), recording).clicked() {
                    if recording {
                        self.recording = None;
                    } else {
                        self.record(id);
                    }
                }
            });
        });
        if self.error == Some(id) {
            ui.label(
                egui::RichText::new("That shortcut couldn't be set. Try another one.")
                    .size(12.0)
                    .color(theme::WARNING),
            );
        }
    }
}
