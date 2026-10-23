// Text from screen, in two steps:
//   1. the window turns into a full-screen screenshot and you drag over the text;
//   2. it becomes a small window that reads it in the background, copies it, and shows it
//      so you can fix it and copy it again.

use crate::{ocr, screen, theme};
use eframe::egui;
use std::sync::mpsc::{Receiver, Sender, channel};

/// The text read, or why it couldn't be.
pub type Reading = Result<String, String>;

enum Stage {
    Idle,
    Selecting {
        image: image::RgbImage,
        texture: egui::TextureHandle,
        drag_from: Option<egui::Pos2>,
    },
    Reading,
    Read {
        text: String,
        /// The text box was changed after it was copied.
        edited: bool,
    },
    Failed(String),
}

/// What the app has to do after a frame.
pub enum Step {
    Stay,
    /// Canceled or closed: hide the window.
    Hide,
    /// An area was picked: turn the window into the small result window.
    ShowResult,
    /// Take another screenshot.
    Retry,
    Copy(String),
}

pub struct OcrView {
    stage: Stage,
    results: (Sender<Reading>, Receiver<Reading>),
}

impl OcrView {
    pub fn new() -> Self {
        Self {
            stage: Stage::Idle,
            results: channel(),
        }
    }

    /// Takes the screenshot. Returns where the monitor is, to cover it with the window.
    pub fn start(&mut self, ctx: &egui::Context) -> Option<screen::Monitor> {
        let (monitor, image) = screen::capture_under_mouse()?;
        let size = [image.width() as usize, image.height() as usize];
        let color = egui::ColorImage::from_rgb(size, image.as_raw());
        let texture = ctx.load_texture("ocr-shot", color, egui::TextureOptions::LINEAR);
        self.stage = Stage::Selecting {
            image,
            texture,
            drag_from: None,
        };
        Some(monitor)
    }

    /// The text read since the last call, to be copied right away.
    pub fn poll(&mut self) -> Option<Reading> {
        let reading = self.results.1.try_recv().ok()?;
        self.stage = match &reading {
            Ok(text) => Stage::Read {
                text: text.clone(),
                edited: false,
            },
            Err(error) => Stage::Failed(error.clone()),
        };
        Some(reading)
    }

    /// Step 1: the screenshot to drag over.
    pub fn select_ui(&mut self, ui: &mut egui::Ui) -> Step {
        let Stage::Selecting {
            image,
            texture,
            drag_from,
        } = &mut self.stage
        else {
            return Step::Hide;
        };
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.stage = Stage::Idle;
            return Step::Hide;
        }

        let full = ui.max_rect();
        let response = ui.allocate_rect(full, egui::Sense::drag());
        let painter = ui.painter();
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(texture.id(), full, uv, egui::Color32::WHITE);
        painter.rect_filled(full, 0.0, egui::Color32::from_black_alpha(130));
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);

        if response.drag_started() {
            *drag_from = response.interact_pointer_pos();
        }
        let selection = drag_from
            .zip(ui.input(|i| i.pointer.latest_pos()))
            .map(|(a, b)| egui::Rect::from_two_pos(a, b));
        // From window points to screenshot pixels.
        let scale = image.width() as f32 / full.width();

        match selection {
            Some(rect) => {
                // The selected part shows without the shade, with its size under it.
                let uv = egui::Rect::from_min_max(
                    ((rect.min - full.min) / full.size()).to_pos2(),
                    ((rect.max - full.min) / full.size()).to_pos2(),
                );
                painter.image(texture.id(), rect, uv, egui::Color32::WHITE);
                painter.rect_stroke(
                    rect,
                    4.0,
                    egui::Stroke::new(2.0, theme::ACCENT),
                    egui::StrokeKind::Outside,
                );
                let size = format!(
                    "{} × {}",
                    (rect.width() * scale) as u32,
                    (rect.height() * scale) as u32
                );
                theme::badge(
                    painter,
                    rect.left_bottom() + egui::vec2(0.0, 8.0),
                    &size,
                    theme::ACCENT,
                );
            }
            None => instructions(painter, full),
        }

        if response.drag_stopped() {
            let Some(rect) = selection.filter(|r| r.width() > 4.0 && r.height() > 4.0) else {
                // A click without dragging: keep selecting.
                *drag_from = None;
                return Step::Stay;
            };
            let x = ((rect.min.x - full.min.x) * scale).max(0.0) as u32;
            let y = ((rect.min.y - full.min.y) * scale).max(0.0) as u32;
            let w = ((rect.width() * scale) as u32).min(image.width() - x);
            let h = ((rect.height() * scale) as u32).min(image.height() - y);
            let crop = image::imageops::crop_imm(image, x, y, w, h).to_image();
            let (sender, ctx) = (self.results.0.clone(), ui.ctx().clone());
            std::thread::spawn(move || {
                let reading = std::panic::catch_unwind(|| ocr::read(&crop))
                    .unwrap_or_else(|_| Err("The text reader failed".into()));
                let _ = sender.send(reading);
                ctx.request_repaint();
            });
            self.stage = Stage::Reading;
            return Step::ShowResult;
        }
        Step::Stay
    }

    /// Step 2: the small window with the text.
    pub fn result_ui(&mut self, ui: &mut egui::Ui) -> Step {
        let mut step = Step::Stay;
        let (escape, copy_keys) = ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter),
            )
        });
        if escape {
            return Step::Hide;
        }

        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(20, 18))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("🔍").size(18.0));
                    ui.label(egui::RichText::new("Text from screen").size(17.0).strong());
                });
                ui.add_space(12.0);
                let buttons_height = 44.0;
                match &mut self.stage {
                    Stage::Reading | Stage::Idle | Stage::Selecting { .. } => {
                        ui.add_space(50.0);
                        ui.vertical_centered(|ui| {
                            ui.add(egui::Spinner::new().size(28.0).color(theme::ACCENT));
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new("Reading the text…").color(theme::MUTED));
                        });
                    }
                    Stage::Read { text, .. } if text.is_empty() => {
                        message(
                            ui,
                            "No text found there",
                            "Try again selecting just the text, a bit bigger.",
                        );
                    }
                    Stage::Read { text, edited } => {
                        let height = (ui.available_height() - buttons_height).max(60.0);
                        egui::ScrollArea::vertical()
                            .max_height(height)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let edit = ui.add(
                                    egui::TextEdit::multiline(text)
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(6)
                                        .margin(egui::Margin::same(10)),
                                );
                                if edit.changed() {
                                    *edited = true;
                                }
                            });
                    }
                    Stage::Failed(error) => message(ui, "Could not read the text", error),
                }

                ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        if let Stage::Read { text, edited } = &mut self.stage {
                            if text.is_empty() {
                                if ui.button("Try again").clicked() {
                                    step = Step::Retry;
                                }
                            } else {
                                let copy = ui.add(
                                    egui::Button::new(
                                        egui::RichText::new("Copy").color(egui::Color32::WHITE),
                                    )
                                    .fill(theme::ACCENT),
                                );
                                if copy.clicked() || copy_keys {
                                    step = Step::Copy(text.clone());
                                    *edited = false;
                                }
                                ui.label(if *edited {
                                    egui::RichText::new("Edited · Copy to use the changes")
                                        .size(13.0)
                                        .color(theme::MUTED)
                                } else {
                                    egui::RichText::new("✔ Copied. Paste it with Ctrl+V")
                                        .size(13.0)
                                        .color(theme::GREEN)
                                });
                            }
                        } else if matches!(self.stage, Stage::Failed(_))
                            && ui.button("Try again").clicked()
                        {
                            step = Step::Retry;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Close").on_hover_text("Esc").clicked() {
                                step = Step::Hide;
                            }
                        });
                    });
                });
            });
        step
    }
}

/// The card at the top of the screenshot that says what to do.
fn instructions(painter: &egui::Painter, full: egui::Rect) {
    let card = egui::Rect::from_center_size(
        full.center_top() + egui::vec2(0.0, 70.0),
        egui::vec2(420.0, 76.0),
    );
    painter.rect_filled(card, theme::RADIUS, theme::CARD);
    painter.rect_stroke(
        card,
        theme::RADIUS,
        egui::Stroke::new(1.0, theme::BORDER_HOVER),
        egui::StrokeKind::Inside,
    );
    painter.text(
        card.center() - egui::vec2(0.0, 12.0),
        egui::Align2::CENTER_CENTER,
        "🔍  Drag over the text you want to copy",
        egui::FontId::proportional(18.0),
        theme::TEXT,
    );
    painter.text(
        card.center() + egui::vec2(0.0, 16.0),
        egui::Align2::CENTER_CENTER,
        "Esc to cancel",
        egui::FontId::proportional(13.0),
        theme::MUTED,
    );
}

fn message(ui: &mut egui::Ui, title: &str, hint: &str) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new(title).size(16.0));
        ui.add_space(4.0);
        ui.label(egui::RichText::new(hint).color(theme::MUTED));
    });
}
