// The clipboard history window: search, pick with the keyboard or mouse, pin, delete,
// and record a new shortcut.

use crate::clipboard::{self, Entry, History};
use crate::{Action, config, shortcuts, theme};
use eframe::egui;
use std::time::{Duration, Instant};

const ROW_HEIGHT: f32 = 52.0;
const COPIED_FOR: Duration = Duration::from_millis(1200);

pub struct ClipboardView {
    history: History,
    // On X11 copied text lives only while this handle exists, so it's kept for the app's life.
    clipboard: Option<arboard::Clipboard>,
    search: String,
    selected: usize,
    focus_search: bool,
    // Selection moved with the keyboard: scroll it into view.
    scroll_to_selected: bool,
    recording: bool,
    shortcut_error: bool,
    copied: Option<(String, Instant)>,
    /// Small pictures of the copied images, by image id (None: couldn't be read).
    thumbs: std::collections::HashMap<String, Option<egui::TextureHandle>>,
}

impl ClipboardView {
    pub fn new(history: History) -> Self {
        ClipboardView {
            history,
            clipboard: None,
            search: String::new(),
            selected: 0,
            focus_search: true,
            scroll_to_selected: false,
            recording: false,
            shortcut_error: false,
            copied: None,
            thumbs: Default::default(),
        }
    }

    /// The window was just shown: start fresh.
    pub fn opened(&mut self) {
        self.search.clear();
        self.selected = 0;
        self.focus_search = true;
        self.scroll_to_selected = true;
        self.recording = false;
        self.shortcut_error = false;
    }

    pub fn start_recording(&mut self) {
        self.recording = true;
        self.shortcut_error = false;
    }

    pub fn is_recording(&self) -> bool {
        self.recording
    }

    pub fn copy(&mut self, text: &str) {
        clipboard::copy(&mut self.clipboard, text);
    }

    fn shown(&self) -> Vec<Entry> {
        let q = self.search.trim().to_lowercase();
        self.history
            .entries()
            .into_iter()
            .filter(|e| {
                q.is_empty()
                    || e.text.to_lowercase().contains(&q)
                    // "image" or "imagen" finds the pictures.
                    || (e.image.is_some() && ("image".starts_with(&q) || "imagen".starts_with(&q)))
            })
            .collect()
    }

    /// `popup`: the small window opened by the shortcut, instead of a screen of the main one.
    pub fn ui(&mut self, ui: &mut egui::Ui, popup: bool) -> Action {
        let mut action = Action::default();
        let ctx = ui.ctx().clone();

        if self.recording {
            match shortcuts::record(&ctx) {
                Some(shortcuts::Recorded::Keys(accel)) => self.save_shortcut(Some(accel)),
                Some(shortcuts::Recorded::Default) => self.save_shortcut(None),
                Some(shortcuts::Recorded::Cancel) => self.recording = false,
                None => {}
            }
            if self.recording {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            ctx.input_mut(|i| i.events.clear());
        }

        let shown = self.shown();
        self.selected = self.selected.min(shown.len().saturating_sub(1));
        let current = shown.get(self.selected).cloned();

        // Keyboard: arrows move, Enter picks, Delete removes, Ctrl+P pins, Esc hides.
        if !self.recording {
            let (down, up, enter, delete, escape, pin) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                    // With text in the search box, Delete edits the search.
                    self.search.is_empty()
                        && i.consume_key(egui::Modifiers::NONE, egui::Key::Delete),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::P),
                )
            });
            if !shown.is_empty() && (down || up) {
                let n = shown.len();
                self.selected = if down {
                    (self.selected + 1) % n
                } else {
                    (self.selected + n - 1) % n
                };
                self.scroll_to_selected = true;
            }
            if let Some(entry) = &current {
                if enter {
                    self.pick(entry, &mut action);
                } else if delete {
                    self.history.remove(entry.key());
                    action.changed = true;
                } else if pin {
                    self.toggle_pin(entry.key(), &mut action);
                }
            }
            if escape {
                action.hide = true;
            }
        }

        let margin = if popup {
            egui::Margin::symmetric(16, 14)
        } else {
            egui::Margin::ZERO
        };
        egui::Frame::new().inner_margin(margin).show(ui, |ui| {
            if !popup {
                theme::page_header(
                    ui,
                    "Clipboard",
                    "Everything you copy. Click to copy it again, ⭐ keeps it at the top.",
                );
                ui.add_space(16.0);
            }
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("🔍  Search")
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::symmetric(10, 8)),
            );
            if search.changed() {
                self.selected = 0;
            }
            if std::mem::take(&mut self.focus_search) {
                search.request_focus();
            }
            ui.add_space(10.0);

            if config::load().clipboard_paused {
                ui.label(
                    egui::RichText::new(
                        "⏸ Recording is paused (turn it back on from the tray icon)",
                    )
                    .color(theme::WARNING),
                );
                ui.add_space(8.0);
            }

            let footer_height = 40.0;
            let list_height = (ui.available_height() - footer_height).max(60.0);
            egui::ScrollArea::vertical()
                .max_height(list_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if shown.is_empty() {
                        ui.add_space(60.0);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new("📋").size(32.0).color(theme::FAINT));
                            ui.add_space(6.0);
                            let (title, hint) = if self.search.is_empty() {
                                (
                                    "Nothing here yet",
                                    "Copy some text or an image and it will show up here.",
                                )
                            } else {
                                ("Nothing matches", "Try other words.")
                            };
                            ui.label(egui::RichText::new(title).size(16.0));
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(hint).color(theme::MUTED));
                        });
                    }
                    for (i, entry) in shown.iter().enumerate() {
                        let just_copied = self
                            .copied
                            .as_ref()
                            .is_some_and(|(t, at)| t == entry.key() && at.elapsed() < COPIED_FOR);
                        let row = self.row(ui, entry, i == self.selected, just_copied);
                        if row.hovered && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                            self.selected = i;
                        }
                        if row.pin {
                            self.toggle_pin(entry.key(), &mut action);
                        } else if row.delete {
                            self.history.remove(entry.key());
                            action.changed = true;
                        } else if row.clicked {
                            self.pick(entry, &mut action);
                        }
                        ui.add_space(6.0);
                    }
                });

            ui.add_space(8.0);
            self.footer(ui);
        });
        action
    }

    fn pick(&mut self, entry: &Entry, action: &mut Action) {
        match &entry.image {
            Some(picture) => clipboard::copy_image(&mut self.clipboard, picture),
            None => self.copy(&entry.text),
        }
        self.copied = Some((entry.key().to_string(), Instant::now()));
        action.hide = true;
    }

    /// The small picture of a copied image, read from the cache the first time.
    fn thumb(&mut self, ctx: &egui::Context, id: &str) -> Option<egui::TextureHandle> {
        self.thumbs
            .entry(id.to_string())
            .or_insert_with(|| {
                let image = image::open(clipboard::thumb_file(id)).ok()?.into_rgba8();
                let size = [image.width() as usize, image.height() as usize];
                Some(ctx.load_texture(
                    id,
                    egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
                    egui::TextureOptions::LINEAR,
                ))
            })
            .clone()
    }

    // Keeps the same entry selected after the list is re-ordered.
    fn toggle_pin(&mut self, key: &str, action: &mut Action) {
        self.history.toggle_pin(key);
        self.selected = self
            .shown()
            .iter()
            .position(|e| e.key() == key)
            .unwrap_or(0);
        self.scroll_to_selected = true;
        action.changed = true;
    }

    fn save_shortcut(&mut self, accel: Option<String>) {
        self.recording = false;
        self.shortcut_error = !shortcuts::set("clipboard", accel);
        self.focus_search = true;
    }

    fn row(
        &mut self,
        ui: &mut egui::Ui,
        entry: &Entry,
        selected: bool,
        just_copied: bool,
    ) -> RowResponse {
        let width = ui.available_width();
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, ROW_HEIGHT), egui::Sense::click());
        if selected && std::mem::take(&mut self.scroll_to_selected) {
            response.scroll_to_me(None);
        }
        let (fill, stroke) = if just_copied {
            (theme::CARD_HOVER, theme::ACCENT)
        } else if selected {
            (theme::CARD_HOVER, theme::BORDER_HOVER)
        } else {
            (theme::CARD, theme::BORDER)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, 8.0, fill);
        painter.rect_stroke(
            rect,
            8.0,
            egui::Stroke::new(1.0, stroke),
            egui::StrokeKind::Inside,
        );

        // Actions on the right, shown on hover (a pinned entry always shows its star).
        // They're added after the row, so they get the click first.
        let show_actions = selected || ui.rect_contains_pointer(rect);
        let mut actions_left = rect.right() - 8.0;
        let mut action_button = |ui: &mut egui::Ui, icon: &str, color, tip: &str| {
            let r = egui::Rect::from_center_size(
                egui::pos2(actions_left - 15.0, rect.center().y),
                egui::vec2(28.0, 28.0),
            );
            actions_left -= 30.0;
            ui.put(
                r,
                egui::Button::new(egui::RichText::new(icon).size(15.0).color(color)).frame(false),
            )
            .on_hover_text(tip)
            .on_hover_cursor(egui::CursorIcon::PointingHand)
        };
        let (mut delete, mut star) = (None, None);
        if show_actions {
            delete = Some(action_button(ui, "🗑", theme::MUTED, "Delete (Delete key)"));
        }
        if show_actions || entry.pinned {
            let (icon, color, tip) = if entry.pinned {
                ("★", theme::STAR, "Unpin (Ctrl+P)")
            } else {
                (
                    "☆",
                    theme::MUTED,
                    "Pin: keep it at the top, never deleted (Ctrl+P)",
                )
            };
            star = Some(action_button(ui, icon, color, tip));
        }
        let star_rect = egui::Rect::from_min_max(egui::pos2(actions_left, rect.top()), rect.max);

        // An image shows its small picture on the left.
        let mut left = rect.min.x + 12.0;
        if let Some(picture) = &entry.image
            && let Some(texture) = self.thumb(ui.ctx(), &picture.id)
        {
            let h = rect.height() - 12.0;
            let size = texture.size_vec2();
            let w = (size.x * h / size.y.max(1.0)).min(h * 2.0);
            let thumb = egui::Rect::from_min_size(egui::pos2(left, rect.min.y + 6.0), egui::vec2(w, h));
            ui.painter().image(
                texture.id(),
                thumb,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            left = thumb.right() + 10.0;
        }
        let text_rect = egui::Rect::from_min_max(
            egui::pos2(left, rect.min.y + 8.0),
            egui::pos2(star_rect.left() - 6.0, rect.max.y - 8.0),
        );
        let (preview, what) = match &entry.image {
            Some(picture) => (
                "Image".to_string(),
                format!("{} × {}", picture.width, picture.height),
            ),
            None => (
                entry.text.split_whitespace().collect::<Vec<_>>().join(" "),
                summary(&entry.text),
            ),
        };
        let meta = if just_copied {
            egui::RichText::new("Copied").color(theme::ACCENT)
        } else {
            egui::RichText::new(format!("{what} · {}", clipboard::ago(entry.time)))
                .color(theme::MUTED)
        };
        ui.scope_builder(egui::UiBuilder::new().max_rect(text_rect), |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add(
                egui::Label::new(preview)
                    .truncate()
                    .selectable(false)
                    .sense(egui::Sense::hover()),
            );
            ui.add(egui::Label::new(meta.size(12.0)).selectable(false));
        });

        let hovered =
            response.hovered() || [&star, &delete].into_iter().flatten().any(|r| r.hovered());
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        RowResponse {
            clicked: response.clicked(),
            pin: star.is_some_and(|r| r.clicked()),
            delete: delete.is_some_and(|r| r.clicked()),
            hovered,
        }
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Shortcut").color(theme::MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let keys = shortcuts::get("clipboard");
                if theme::shortcut_keys(ui, &keys, self.recording).clicked() {
                    if self.recording {
                        self.recording = false;
                    } else {
                        self.start_recording();
                    }
                }
            });
        });
        if self.shortcut_error {
            ui.label(
                egui::RichText::new("That shortcut couldn't be set. Try another one.")
                    .size(12.0)
                    .color(theme::WARNING),
            );
        }
    }
}

struct RowResponse {
    clicked: bool,
    pin: bool,
    delete: bool,
    hovered: bool,
}

fn summary(text: &str) -> String {
    let chars = text.chars().count();
    match text.lines().count() {
        0 | 1 => format!("{chars} characters"),
        lines => format!("{lines} lines · {chars} characters"),
    }
}
