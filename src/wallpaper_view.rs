// The Wallpapers tab, like wallswitch's window: a grid of every image. Click adds it to (or takes
// it out of) the carousel, double-click sets it now. Thumbnails are made in a background thread
// and cached in toolswitch's cache folder.

use crate::shortcut_editor::ShortcutEditor;
use crate::{theme, wallpaper};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

const CARD: egui::Vec2 = egui::vec2(176.0, 110.0);
const GAP: f32 = 10.0;
const THUMB: (u32, u32) = (352, 220);
const INTERVALS: &[u64] = &[1, 5, 10, 15, 30, 60, 120, 240, 480, 1440];

enum Picked {
    Folder(PathBuf),
    Images(Vec<PathBuf>),
}

pub struct WallpaperView {
    images: Vec<PathBuf>,
    playlist: HashSet<PathBuf>,
    current: Option<PathBuf>,
    rotate: bool,
    interval: u64,
    /// When the list was last read from disk and GNOME.
    loaded: Option<Instant>,
    /// None while the thumbnail is being made (or couldn't be).
    thumbs: HashMap<PathBuf, Option<egui::TextureHandle>>,
    thumb_requests: Sender<PathBuf>,
    thumb_results: Receiver<(PathBuf, Option<egui::ColorImage>)>,
    picked: (Sender<Picked>, Receiver<Picked>),
    show_shortcuts: bool,
    pub shortcuts: ShortcutEditor,
}

impl WallpaperView {
    pub fn new(ctx: &egui::Context) -> Self {
        let (thumb_requests, requests) = channel::<PathBuf>();
        let (results, thumb_results) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for path in requests {
                let _ = results.send((path.clone(), thumbnail(&path)));
                ctx.request_repaint();
            }
        });
        WallpaperView {
            images: vec![],
            playlist: HashSet::new(),
            current: None,
            rotate: false,
            interval: 15,
            loaded: None,
            thumbs: HashMap::new(),
            thumb_requests,
            thumb_results,
            picked: channel(),
            show_shortcuts: false,
            shortcuts: ShortcutEditor::default(),
        }
    }

    /// Reads everything again next frame (the tab was just opened, or the list changed).
    pub fn reload(&mut self) {
        self.loaded = None;
    }

    fn load(&mut self) {
        let settings = wallpaper::load();
        self.images = wallpaper::scan(&settings);
        self.playlist = settings.playlist.into_iter().collect();
        self.rotate = settings.rotate;
        self.interval = settings.interval;
        self.current = wallpaper::current();
        self.loaded = Some(Instant::now());
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.shortcuts.poll(&ctx);
        while let Ok((path, image)) = self.thumb_results.try_recv() {
            let texture = image.map(|img| {
                ctx.load_texture(path.to_string_lossy(), img, egui::TextureOptions::LINEAR)
            });
            self.thumbs.insert(path, texture);
        }
        while let Ok(picked) = self.picked.1.try_recv() {
            match picked {
                Picked::Folder(dir) => wallpaper::add_dir(dir),
                Picked::Images(files) => wallpaper::add_files(files),
            }
            self.reload();
        }
        // The automatic change moves the current wallpaper: look again every few seconds.
        if self
            .loaded
            .is_none_or(|at| at.elapsed() > Duration::from_secs(3))
        {
            self.load();
        }
        ctx.request_repaint_after(Duration::from_secs(3));

        self.header(ui);
        ui.add_space(16.0);
        let footer = if self.show_shortcuts { 200.0 } else { 52.0 };
        let grid_height = (ui.available_height() - footer).max(120.0);
        egui::ScrollArea::vertical()
            .max_height(grid_height)
            .auto_shrink([false, false])
            .show(ui, |ui| self.grid(ui));
        ui.add_space(10.0);
        self.footer(ui);
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                let summary = match self.playlist.len() {
                    0 => "Carousel empty: all images rotate. Click an image to add it.".to_string(),
                    n => format!("{n} in carousel · double-click an image to set it now"),
                };
                theme::page_header(ui, "Wallpapers", &summary);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("➕ Images").clicked() {
                    self.pick(false, ui.ctx());
                }
                if ui.button("📁 Folder").clicked() {
                    self.pick(true, ui.ctx());
                }
            });
        });
    }

    /// The system's file chooser, in a thread so the window keeps drawing.
    fn pick(&self, folder: bool, ctx: &egui::Context) {
        let (sender, ctx) = (self.picked.0.clone(), ctx.clone());
        std::thread::spawn(move || {
            let dialog = rfd::FileDialog::new();
            let picked = if folder {
                dialog
                    .set_title("Add a folder of wallpapers")
                    .pick_folder()
                    .map(Picked::Folder)
            } else {
                dialog
                    .set_title("Add wallpapers")
                    .add_filter("Images", &["jpg", "jpeg", "png", "webp", "bmp"])
                    .pick_files()
                    .map(Picked::Images)
            };
            if let Some(picked) = picked {
                let _ = sender.send(picked);
                ctx.request_repaint();
            }
        });
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        if self.images.is_empty() {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("🖼").size(32.0).color(theme::FAINT));
                ui.add_space(6.0);
                ui.label(egui::RichText::new("No images yet").size(16.0));
                ui.label(
                    egui::RichText::new("Use 📁 Folder to add a folder of wallpapers.")
                        .color(theme::MUTED),
                );
            });
            return;
        }
        // As many columns as fit, then the cards grow to fill the row (same shape).
        let width = ui.available_width();
        let columns = (((width + GAP) / (CARD.x + GAP)).floor() as usize).max(1);
        let card_w = (width - GAP * (columns - 1) as f32) / columns as f32;
        let size = egui::vec2(card_w, card_w * CARD.y / CARD.x);
        let images = self.images.clone();
        for row in images.chunks(columns) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for path in row {
                    self.card(ui, path, size);
                }
            });
            ui.add_space(GAP);
        }
    }

    fn card(&mut self, ui: &mut egui::Ui, path: &Path, size: egui::Vec2) {
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        let in_carousel = self.playlist.contains(path);
        let is_current = self.current.as_deref() == Some(path);

        // Thumbnails are only asked for once the card is on screen.
        if ui.is_rect_visible(rect) && !self.thumbs.contains_key(path) {
            self.thumbs.insert(path.to_path_buf(), None);
            let _ = self.thumb_requests.send(path.to_path_buf());
        }
        let painter = ui.painter_at(rect.expand(2.0));
        match self.thumbs.get(path) {
            Some(Some(texture)) => {
                egui::Image::new(texture)
                    .corner_radius(10.0)
                    .paint_at(ui, rect);
            }
            _ => _ = painter.rect_filled(rect, 10.0, theme::CARD),
        }
        let border = if is_current {
            Some(theme::GREEN)
        } else if in_carousel {
            Some(theme::ACCENT)
        } else if response.hovered() {
            Some(theme::BORDER_HOVER)
        } else {
            None
        };
        if let Some(color) = border {
            painter.rect_stroke(
                rect,
                10.0,
                egui::Stroke::new(2.0, color),
                egui::StrokeKind::Inside,
            );
        }
        // Badges in the top left corner.
        let mut badge_at = rect.min + egui::vec2(6.0, 6.0);
        if is_current {
            badge_at.x = theme::badge(&painter, badge_at, "Current", theme::GREEN).right() + 4.0;
        }
        if in_carousel {
            theme::badge(&painter, badge_at, "In carousel", theme::ACCENT);
        }

        // Hovered: "Set now" and "Remove" at the bottom. Added after the card, so they get the click.
        let mut button_clicked = false;
        if ui.rect_contains_pointer(rect) {
            let bar =
                egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 28.0), rect.max);
            painter.rect_filled(
                bar,
                egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 10,
                    se: 10,
                },
                egui::Color32::from_black_alpha(170),
            );
            let half = bar.width() / 2.0;
            let set = ui.put(
                egui::Rect::from_min_size(bar.min, egui::vec2(half, bar.height())),
                egui::Button::new("Set now").frame(false),
            );
            let remove = ui.put(
                egui::Rect::from_min_size(
                    bar.min + egui::vec2(half, 0.0),
                    egui::vec2(half, bar.height()),
                ),
                egui::Button::new(egui::RichText::new("Remove").color(theme::MUTED)).frame(false),
            );
            if set.clicked() {
                self.apply(path);
                button_clicked = true;
            }
            if remove
                .on_hover_text("Takes it off the list. The file is not deleted.")
                .clicked()
            {
                wallpaper::remove_image(path);
                self.reload();
                button_clicked = true;
            }
        }
        let response = response.on_hover_text(path.to_string_lossy());
        if button_clicked {
            return;
        }
        // A double-click's two clicks add and remove it again, so the carousel ends up as it was.
        if response.double_clicked() {
            self.apply(path);
        } else if response.clicked() {
            wallpaper::toggle_playlist(path);
            if !self.playlist.remove(path) {
                self.playlist.insert(path.to_path_buf());
            }
        }
    }

    fn apply(&mut self, path: &Path) {
        if let Err(error) = wallpaper::apply(path) {
            eprintln!("{error}");
        }
        self.current = Some(path.to_path_buf());
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        theme::card()
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let mut rotate = self.rotate;
                    let mut interval = self.interval;
                    theme::toggle(ui, &mut rotate);
                    ui.label("Change by itself every");
                    ui.add_enabled_ui(rotate, |ui| {
                        egui::ComboBox::from_id_salt("interval")
                            .selected_text(interval_label(interval))
                            .show_ui(ui, |ui| {
                                for &minutes in INTERVALS {
                                    ui.selectable_value(
                                        &mut interval,
                                        minutes,
                                        interval_label(minutes),
                                    );
                                }
                            });
                    });
                    if (rotate, interval) != (self.rotate, self.interval) {
                        (self.rotate, self.interval) = (rotate, interval);
                        wallpaper::update(|s| {
                            s.rotate = rotate;
                            s.interval = interval;
                        });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .selectable_label(self.show_shortcuts, "⌨ Shortcuts")
                            .clicked()
                        {
                            self.show_shortcuts = !self.show_shortcuts;
                        }
                        ui.add_space(8.0);
                        legend(ui, theme::GREEN, "Current");
                        legend(ui, theme::ACCENT, "In carousel");
                    });
                });
                if self.show_shortcuts {
                    ui.add_space(8.0);
                    for (id, what) in [
                        ("wallpaper", "Open this tab"),
                        ("wallpaper-random", "Random wallpaper"),
                        ("wallpaper-next", "Next wallpaper"),
                        ("wallpaper-prev", "Previous wallpaper"),
                    ] {
                        self.shortcuts.row(ui, id, what);
                    }
                }
            });
    }
}

fn legend(ui: &mut egui::Ui, color: egui::Color32, text: &str) {
    ui.label(egui::RichText::new(text).size(12.0).color(theme::MUTED));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

fn interval_label(minutes: u64) -> String {
    match minutes {
        1440 => "1 day".into(),
        m if m >= 60 && m % 60 == 0 => format!("{} h", m / 60),
        m => format!("{m} min"),
    }
}

// ---------- thumbnails ----------

fn cache_file(path: &Path) -> Option<PathBuf> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (path, modified).hash(&mut hasher);
    Some(
        crate::config::cache_dir()
            .join("thumbs")
            .join(format!("{:016x}.png", hasher.finish())),
    )
}

fn thumbnail(path: &Path) -> Option<egui::ColorImage> {
    let cached = cache_file(path);
    let image = match cached.as_ref().and_then(|c| image::open(c).ok()) {
        Some(image) => image,
        None => {
            // A quick shrink first: filtering a 4K photo directly is slow.
            let image = image::open(path)
                .ok()?
                .thumbnail(THUMB.0 * 2, THUMB.1 * 2)
                .resize_to_fill(THUMB.0, THUMB.1, image::imageops::FilterType::Triangle);
            if let Some(c) = &cached {
                let _ = std::fs::create_dir_all(c.parent()?);
                let _ = image.save(c);
            }
            image
        }
    };
    let rgba = image.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        &rgba,
    ))
}
