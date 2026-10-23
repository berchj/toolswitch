// Colors, spacing and the small widgets shared by every tool: a dark zinc palette with
// rounded cards, toggle switches and keyboard keys.

use eframe::egui::{self, Color32};

pub const BACKGROUND: Color32 = Color32::from_rgb(0x09, 0x09, 0x0b);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x12, 0x12, 0x15);
pub const CARD: Color32 = Color32::from_rgb(0x18, 0x18, 0x1b);
pub const CARD_HOVER: Color32 = Color32::from_rgb(0x1f, 0x1f, 0x23);
pub const BORDER: Color32 = Color32::from_rgb(0x27, 0x27, 0x2a);
pub const BORDER_HOVER: Color32 = Color32::from_rgb(0x3f, 0x3f, 0x46);
pub const TEXT: Color32 = Color32::from_rgb(0xf4, 0xf4, 0xf5);
pub const MUTED: Color32 = Color32::from_rgb(0xa1, 0xa1, 0xaa);
pub const FAINT: Color32 = Color32::from_rgb(0x71, 0x71, 0x7a);
pub const ACCENT: Color32 = Color32::from_rgb(0x3b, 0x82, 0xf6);
pub const GREEN: Color32 = Color32::from_rgb(0x10, 0xb9, 0x81);
pub const WARNING: Color32 = Color32::from_rgb(0xf5, 0x9e, 0x0b);
pub const STAR: Color32 = Color32::from_rgb(0xfa, 0xcc, 0x15);

pub const RADIUS: f32 = 12.0;

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.animation_time = 0.2;
        style.interaction.selectable_labels = false;
        style.spacing.scroll.bar_width = 6.0;
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));

        let v = &mut style.visuals;
        v.panel_fill = BACKGROUND;
        v.window_fill = CARD;
        v.extreme_bg_color = SIDEBAR;
        v.faint_bg_color = CARD;
        v.override_text_color = Some(TEXT);
        v.selection.bg_fill = ACCENT;
        v.selection.stroke = egui::Stroke::new(1.0, TEXT);
        v.window_stroke = egui::Stroke::new(1.0, BORDER);
        v.window_corner_radius = RADIUS.into();
        v.menu_corner_radius = 8.into();
        v.text_edit_bg_color = Some(CARD);
        for (w, fill, stroke) in [
            (&mut v.widgets.noninteractive, CARD, BORDER),
            (&mut v.widgets.inactive, CARD, BORDER),
            (&mut v.widgets.hovered, CARD_HOVER, BORDER_HOVER),
            (&mut v.widgets.active, BORDER, BORDER_HOVER),
            (&mut v.widgets.open, CARD_HOVER, BORDER_HOVER),
        ] {
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
            w.bg_stroke = egui::Stroke::new(1.0, stroke);
            w.corner_radius = 8.into();
            w.expansion = 0.0;
        }
        v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, BORDER);
        // Slider rails and checkboxes use bg_fill; buttons keep the card color.
        v.widgets.inactive.bg_fill = BORDER;
    });
}

/// A card: rounded, with a subtle border.
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .corner_radius(RADIUS)
        .inner_margin(egui::Margin::same(16))
}

/// Title and one line under it, at the top of each screen.
pub fn page_header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.label(egui::RichText::new(title).size(22.0).strong());
    ui.add_space(2.0);
    ui.label(egui::RichText::new(subtitle).color(MUTED));
}

/// A tool's on/off switch with a label, for the top of its screen. Returns true if it changed.
pub fn tool_switch(ui: &mut egui::Ui, tool: &str) -> bool {
    let mut on = crate::config::enabled(tool);
    let mut changed = false;
    ui.horizontal(|ui| {
        if toggle(ui, &mut on).changed() {
            crate::config::set_enabled(tool, on);
            crate::shortcuts::register_all();
            changed = true;
        }
        ui.label(egui::RichText::new(if on { "On" } else { "Off" }).color(MUTED));
    });
    changed
}

/// Title and subtitle on the left, the tool's on/off switch on the right.
pub fn page_header_with_switch(ui: &mut egui::Ui, title: &str, subtitle: &str, tool: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width() - 90.0);
            page_header(ui, title, subtitle);
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed = tool_switch(ui, tool);
        });
    });
    changed
}

/// An on/off switch that slides.
pub fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = egui::vec2(36.0, 20.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool(response.id, *on);
    let off = if response.hovered() {
        BORDER_HOVER
    } else {
        BORDER
    };
    let painter = ui.painter();
    painter.rect_filled(rect, rect.height() / 2.0, lerp(off, ACCENT, t));
    let r = rect.height() / 2.0 - 3.0;
    let x = egui::lerp(rect.left() + r + 3.0..=rect.right() - r - 3.0, t);
    painter.circle_filled(egui::pos2(x, rect.center().y), r, Color32::WHITE);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// A shortcut as keyboard keys, [Super] + [Shift] + [V]. Click it to record new keys;
/// while recording it says so.
pub fn shortcut_keys(ui: &mut egui::Ui, accel: &str, recording: bool) -> egui::Response {
    let hover_id = ui.next_auto_id();
    let hovered = ui
        .ctx()
        .read_response(hover_id)
        .is_some_and(|r| r.hovered());
    let response = ui
        .scope_builder(
            egui::UiBuilder::new()
                .id(hover_id)
                .sense(egui::Sense::click()),
            |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if recording {
                        key(ui, "Press your keys…", ACCENT, ACCENT);
                        return;
                    }
                    let border = if hovered { BORDER_HOVER } else { BORDER };
                    let text = if hovered { TEXT } else { MUTED };
                    // Laid out from the right, the keys go in backwards to read in order.
                    let mut keys: Vec<&str> = accel.split('+').collect();
                    if ui.layout().main_dir() == egui::Direction::RightToLeft {
                        keys.reverse();
                    }
                    for (i, k) in keys.into_iter().enumerate() {
                        if i > 0 {
                            ui.label(egui::RichText::new("+").size(12.0).color(FAINT));
                        }
                        key(ui, k, border, text);
                    }
                });
            },
        )
        .response;
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(if recording {
            "Esc cancels · Backspace goes back to the default"
        } else {
            "Click to change"
        })
}

fn key(ui: &mut egui::Ui, text: &str, border: Color32, color: Color32) {
    egui::Frame::new()
        .fill(SIDEBAR)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .size(12.0)
                        .monospace()
                        .color(color),
                )
                .selectable(false),
            )
        });
}

/// A small rounded label with a color, like "Current".
pub fn badge(painter: &egui::Painter, pos: egui::Pos2, text: &str, fill: Color32) -> egui::Rect {
    let galley = painter.layout_no_wrap(
        text.into(),
        egui::FontId::proportional(11.0),
        Color32::WHITE,
    );
    let rect = egui::Rect::from_min_size(pos, galley.size() + egui::vec2(14.0, 6.0));
    painter.rect_filled(rect, rect.height() / 2.0, fill);
    painter.galley(rect.min + egui::vec2(7.0, 3.0), galley, Color32::WHITE);
    rect
}

/// The big area to drop files on, with a dashed border. Click it to pick files instead.
pub fn drop_zone(ui: &mut egui::Ui, hovering: bool, title: &str, hint: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 150.0),
        egui::Sense::click(),
    );
    let active = ui
        .ctx()
        .animate_bool(response.id, hovering || response.hovered());
    let color = BORDER_HOVER.lerp_to_gamma(ACCENT, active);
    let painter = ui.painter();
    painter.rect_filled(rect, RADIUS, CARD.lerp_to_gamma(CARD_HOVER, active));
    // Dashed border: four dashed sides, inset so the round corners stay clean.
    let r = rect.shrink(1.0);
    let c = RADIUS;
    for side in [
        [
            r.left_top() + egui::vec2(c, 0.0),
            r.right_top() - egui::vec2(c, 0.0),
        ],
        [
            r.left_bottom() + egui::vec2(c, 0.0),
            r.right_bottom() - egui::vec2(c, 0.0),
        ],
        [
            r.left_top() + egui::vec2(0.0, c),
            r.left_bottom() - egui::vec2(0.0, c),
        ],
        [
            r.right_top() + egui::vec2(0.0, c),
            r.right_bottom() - egui::vec2(0.0, c),
        ],
    ] {
        painter.extend(egui::Shape::dashed_line(
            &side,
            egui::Stroke::new(1.5, color),
            6.0,
            5.0,
        ));
    }
    painter.text(
        rect.center() - egui::vec2(0.0, 30.0),
        egui::Align2::CENTER_CENTER,
        "📥",
        egui::FontId::proportional(28.0),
        MUTED.lerp_to_gamma(ACCENT, active),
    );
    painter.text(
        rect.center() + egui::vec2(0.0, 8.0),
        egui::Align2::CENTER_CENTER,
        if hovering { "Drop them here" } else { title },
        egui::FontId::proportional(16.0),
        TEXT,
    );
    painter.text(
        rect.center() + egui::vec2(0.0, 32.0),
        egui::Align2::CENTER_CENTER,
        hint,
        egui::FontId::proportional(12.0),
        FAINT,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Files dropped on the window this frame, and whether some are being dragged over it.
pub fn dropped_files(ctx: &egui::Context) -> (Vec<std::path::PathBuf>, bool) {
    ctx.input(|i| {
        (
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect(),
            !i.raw.hovered_files.is_empty(),
        )
    })
}

/// Shows a folder in the system's file manager.
pub fn open_folder(dir: &std::path::Path) {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut open = crate::desktop::command(program);
    open.arg(dir);
    crate::desktop::start(open);
}

/// The app's icon as a PNG file (drawn in assets/icon.svg).
pub const APP_ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

/// The app's icon as a texture, loaded once.
pub fn logo(ctx: &egui::Context) -> egui::TextureHandle {
    let id = egui::Id::new("app-logo");
    if let Some(texture) = ctx.data(|d| d.get_temp(id)) {
        return texture;
    }
    let image = egui::ColorImage::from_rgba_unmultiplied([64, 64], &app_icon(64));
    let texture = ctx.load_texture("app-logo", image, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, texture.clone()));
    texture
}

/// The app's icon, `size` pixels square, as RGBA.
pub fn app_icon(size: u32) -> Vec<u8> {
    let icon = image::load_from_memory_with_format(APP_ICON_PNG, image::ImageFormat::Png)
        .expect("valid icon")
        .into_rgba8();
    if icon.width() == size {
        return icon.into_raw();
    }
    image::imageops::resize(&icon, size, size, image::imageops::FilterType::Lanczos3).into_raw()
}
