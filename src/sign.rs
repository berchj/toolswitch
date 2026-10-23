// Sign PDF: draw your signature once (or load a picture of it), then drop PDFs and a signed
// copy is saved next to each one as name-signed.pdf, with the signature where you chose.

use crate::jobs::{self, FileJobs, Work};
use crate::{config, theme};
use eframe::egui;
use lopdf::{Dictionary, Document, Object, Stream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

const SIGNATURE: &str = "signature.png";
/// The drawing area, in points. The signature is saved at twice this size.
const PAD: egui::Vec2 = egui::vec2(440.0, 150.0);
const SCALE: f32 = 2.0;
/// Space between the signature and the page edge, in PDF points (1/72 inch).
const MARGIN: f32 = 40.0;

#[derive(Clone, Copy, PartialEq)]
pub enum Pages {
    First,
    Last,
    All,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Corner {
    BottomRight,
    BottomCenter,
    BottomLeft,
    TopRight,
}

#[derive(Clone, Copy)]
pub struct Placement {
    pub pages: Pages,
    pub corner: Corner,
    /// Signature width as a share of the page width.
    pub width: f32,
}

fn signature_file() -> PathBuf {
    config::dir().join(SIGNATURE)
}

/// Just the inked part: the transparent border around the signature is cut off.
fn trim(image: &image::RgbaImage) -> image::RgbaImage {
    let (w, h) = image.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (x, y, p) in image.enumerate_pixels() {
        if p[3] > 10 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x1 < x0 {
        return image.clone();
    }
    image::imageops::crop_imm(image, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image()
}

/// A photo or scan of a signature: the paper (light pixels) becomes transparent.
fn from_picture(path: &Path) -> Result<image::RgbaImage, String> {
    let mut image = image::open(path).map_err(|e| e.to_string())?.into_rgba8();
    for p in image.pixels_mut() {
        let light = (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3;
        // Light gray and white fade out; ink stays.
        let alpha = (255u32.saturating_sub(light) * 255 / 120).min(255);
        p[3] = (p[3] as u32).min(alpha) as u8;
    }
    Ok(trim(&image))
}

/// The strokes drawn with the mouse, as an image with a transparent background.
fn render(strokes: &[Vec<egui::Pos2>], ink: [u8; 3]) -> image::RgbaImage {
    let (w, h) = ((PAD.x * SCALE) as u32, (PAD.y * SCALE) as u32);
    let mut image = image::RgbaImage::from_pixel(w, h, image::Rgba([ink[0], ink[1], ink[2], 0]));
    let radius = 1.6 * SCALE;
    let mut stamp = |cx: f32, cy: f32| {
        let (xa, xb) = (
            (cx - radius - 1.0).max(0.0) as u32,
            ((cx + radius + 1.0) as u32).min(w - 1),
        );
        let (ya, yb) = (
            (cy - radius - 1.0).max(0.0) as u32,
            ((cy + radius + 1.0) as u32).min(h - 1),
        );
        for y in ya..=yb {
            for x in xa..=xb {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                // Soft edge one pixel wide, so the line isn't jagged.
                let cover = ((radius + 0.5 - d).clamp(0.0, 1.0) * 255.0) as u8;
                let p = image.get_pixel_mut(x, y);
                p[3] = p[3].max(cover);
            }
        }
    };
    for stroke in strokes {
        for pair in stroke.windows(2) {
            let (a, b) = (pair[0] * SCALE, pair[1] * SCALE);
            let steps = (a.distance(b) / (radius * 0.5)).ceil().max(1.0) as usize;
            for i in 0..=steps {
                let p = a.lerp(b, i as f32 / steps as f32);
                stamp(p.x, p.y);
            }
        }
        if stroke.len() == 1 {
            stamp(stroke[0].x * SCALE, stroke[0].y * SCALE);
        }
    }
    trim(&image)
}

/// A page setting, looked up on the page and then on its parents (pages inherit them).
fn inherited(doc: &Document, page_id: lopdf::ObjectId, key: &[u8]) -> Option<Object> {
    let mut id = Some(page_id);
    while let Some(current) = id {
        let dict = doc.get_dictionary(current).ok()?;
        if let Ok(value) = dict.get(key) {
            return Some(match value {
                Object::Reference(r) => doc.get_object(*r).cloned().unwrap_or(Object::Null),
                other => other.clone(),
            });
        }
        id = dict.get(b"Parent").and_then(Object::as_reference).ok();
    }
    None
}

/// The visible part of the page: its CropBox, or else its MediaBox.
fn page_box(doc: &Document, page_id: lopdf::ObjectId) -> [f32; 4] {
    for key in [b"CropBox".as_slice(), b"MediaBox"] {
        if let Some(Object::Array(b)) = inherited(doc, page_id, key)
            && b.len() == 4
        {
            let n: Vec<f32> = b.iter().map(|v| v.as_float().unwrap_or(0.0)).collect();
            return [n[0].min(n[2]), n[1].min(n[3]), n[0].max(n[2]), n[1].max(n[3])];
        }
    }
    // A4
    [0.0, 0.0, 595.0, 842.0]
}

/// How the page is turned when shown: 0, 90, 180 or 270 degrees clockwise.
fn page_rotation(doc: &Document, page_id: lopdf::ObjectId) -> i64 {
    inherited(doc, page_id, b"Rotate")
        .and_then(|r| r.as_i64().ok())
        .map_or(0, |r| r.rem_euclid(360) / 90 * 90)
}

/// The matrix that draws the signature upright at (vx, vy), size (sw, sh), measured on the
/// page as it's shown, whatever its rotation.
fn placement_matrix(bx: [f32; 4], rotation: i64, (vx, vy, sw, sh): (f32, f32, f32, f32)) -> [f32; 6] {
    let [x0, y0, x1, y1] = bx;
    match rotation {
        90 => [0.0, sw, -sh, 0.0, x1 - vy, y0 + vx],
        180 => [-sw, 0.0, 0.0, -sh, x1 - vx, y1 - vy],
        270 => [0.0, -sw, sh, 0.0, x0 + vy, y1 - vx],
        _ => [sw, 0.0, 0.0, sh, x0 + vx, y0 + vy],
    }
}

/// Puts the page's existing drawing between q and Q, and the signature after it, so nothing
/// the page leaves changed (a scale, a move) applies to the signature.
fn add_on_top(doc: &mut Document, page_id: lopdf::ObjectId, content: Vec<u8>) -> Result<(), String> {
    let existing: Vec<Object> = match doc.get_dictionary(page_id).map_err(|e| e.to_string())?.get(b"Contents") {
        Ok(Object::Reference(id)) => vec![Object::Reference(*id)],
        Ok(Object::Array(items)) => items.clone(),
        _ => vec![],
    };
    let mut stream = |bytes: &[u8]| {
        Object::Reference(doc.add_object(Stream::new(Dictionary::new(), bytes.to_vec())))
    };
    let mut contents = vec![stream(b"q\n")];
    contents.extend(existing);
    contents.push(stream(b"\nQ\n"));
    contents.push(stream(&content));
    let page = doc
        .get_object_mut(page_id)
        .and_then(Object::as_dict_mut)
        .map_err(|e| e.to_string())?;
    page.set("Contents", contents);
    Ok(())
}

/// Saves a signed copy next to the PDF and returns its path.
pub fn sign_pdf(
    file: &Path,
    signature: &image::RgbaImage,
    placement: Placement,
) -> Result<PathBuf, String> {
    let mut doc = Document::load(file).map_err(|e| format!("Couldn't read the PDF: {e}"))?;
    if doc.is_encrypted() {
        return Err("The PDF is password protected".into());
    }
    let (w, h) = signature.dimensions();
    let rgb: Vec<u8> = signature
        .pixels()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    let alpha: Vec<u8> = signature.pixels().map(|p| p[3]).collect();

    let image_dict = |space: &str| {
        let mut d = Dictionary::new();
        d.set("Type", "XObject");
        d.set("Subtype", "Image");
        d.set("Width", w as i64);
        d.set("Height", h as i64);
        d.set("ColorSpace", Object::Name(space.as_bytes().to_vec()));
        d.set("BitsPerComponent", 8);
        d
    };
    let mut mask = Stream::new(image_dict("DeviceGray"), alpha);
    let _ = mask.compress();
    let mask_id = doc.add_object(mask);
    let mut dict = image_dict("DeviceRGB");
    dict.set("SMask", Object::Reference(mask_id));
    let mut picture = Stream::new(dict, rgb);
    let _ = picture.compress();
    let picture_id = doc.add_object(picture);

    let pages = doc.get_pages();
    let ids: Vec<lopdf::ObjectId> = match placement.pages {
        Pages::First => pages.values().next().copied().into_iter().collect(),
        Pages::Last => pages.values().last().copied().into_iter().collect(),
        Pages::All => pages.values().copied().collect(),
    };
    if ids.is_empty() {
        return Err("The PDF has no pages".into());
    }
    for page_id in ids {
        let bx = page_box(&doc, page_id);
        let rotation = page_rotation(&doc, page_id);
        // The page as it's shown: turned sideways, width and height swap.
        let (pw, ph) = if rotation % 180 == 0 {
            (bx[2] - bx[0], bx[3] - bx[1])
        } else {
            (bx[3] - bx[1], bx[2] - bx[0])
        };
        let sw = pw * placement.width;
        let sh = sw * h as f32 / w as f32;
        let vx = match placement.corner {
            Corner::BottomLeft => MARGIN,
            Corner::BottomCenter => (pw - sw) / 2.0,
            Corner::BottomRight | Corner::TopRight => pw - MARGIN - sw,
        };
        let vy = match placement.corner {
            Corner::TopRight => ph - MARGIN - sh,
            _ => MARGIN,
        };
        let [a, b, c, d, e, f] = placement_matrix(bx, rotation, (vx, vy, sw, sh));

        // A page that inherits its resources (fonts, pictures) gets its own copy first, or
        // adding the signature would leave it with only the signature.
        let has_own = doc
            .get_dictionary(page_id)
            .is_ok_and(|p| p.has(b"Resources"));
        if !has_own
            && let Some(resources) = inherited(&doc, page_id, b"Resources")
            && let Ok(page) = doc.get_object_mut(page_id).and_then(Object::as_dict_mut)
        {
            page.set("Resources", resources);
        }
        doc.add_xobject(page_id, "TsSignature", picture_id)
            .map_err(|e| e.to_string())?;
        let content =
            format!("q {a:.3} {b:.3} {c:.3} {d:.3} {e:.3} {f:.3} cm /TsSignature Do Q\n");
        add_on_top(&mut doc, page_id, content.into_bytes())?;
    }

    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    let dir = file.parent().unwrap_or(Path::new("."));
    let out = (1..)
        .map(|i| {
            let n = if i > 1 {
                format!("-{i}")
            } else {
                String::new()
            };
            dir.join(format!("{stem}-signed{n}.pdf"))
        })
        .find(|p| !p.exists())
        .unwrap();
    doc.save(&out).map_err(|e| e.to_string())?;
    Ok(out)
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

pub struct SignPage {
    signature: Option<image::RgbaImage>,
    texture: Option<egui::TextureHandle>,
    strokes: Vec<Vec<egui::Pos2>>,
    drawing: bool,
    blue_ink: bool,
    placement: Placement,
    jobs: Option<FileJobs>,
    pdfs: (Sender<Vec<PathBuf>>, Receiver<Vec<PathBuf>>),
    picture: (Sender<Vec<PathBuf>>, Receiver<Vec<PathBuf>>),
    error: Option<String>,
}

impl Default for SignPage {
    fn default() -> Self {
        SignPage {
            signature: image::open(signature_file()).ok().map(|i| i.into_rgba8()),
            texture: None,
            strokes: vec![],
            drawing: false,
            blue_ink: true,
            placement: Placement {
                pages: Pages::Last,
                corner: Corner::BottomRight,
                width: 0.25,
            },
            jobs: None,
            pdfs: channel(),
            picture: channel(),
            error: None,
        }
    }
}

impl SignPage {
    fn set_signature(&mut self, image: image::RgbaImage) {
        if let Err(error) = std::fs::create_dir_all(config::dir())
            .and_then(|_| image.save(signature_file()).map_err(std::io::Error::other))
        {
            self.error = Some(format!("Couldn't save the signature: {error}"));
        }
        self.signature = Some(image);
        self.texture = None;
        self.drawing = false;
        self.strokes.clear();
    }

    fn add(&mut self, files: &[PathBuf], ctx: &egui::Context) {
        let Some(signature) = self.signature.clone() else {
            self.error = Some("Draw or load your signature first.".into());
            return;
        };
        let signature = Arc::new(signature);
        let placement = self.placement;
        let work: Work = Arc::new(move |file| {
            let out = sign_pdf(file, &signature, placement)?;
            Ok((out, String::new()))
        });
        let jobs = self.jobs.get_or_insert_with(|| FileJobs::new(ctx));
        for file in jobs::collect(files, is_pdf) {
            jobs.add(file, work.clone());
        }
    }

    fn pad(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(PAD, egui::Sense::drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, theme::RADIUS, egui::Color32::WHITE);
        painter.line_segment(
            [
                egui::pos2(rect.left() + 30.0, rect.bottom() - 34.0),
                egui::pos2(rect.right() - 30.0, rect.bottom() - 34.0),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_gray(200)),
        );
        if self.strokes.is_empty() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Sign here with the mouse",
                egui::FontId::proportional(16.0),
                egui::Color32::from_gray(170),
            );
        }
        if response.drag_started() {
            self.strokes.push(vec![]);
        }
        if response.dragged()
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(stroke) = self.strokes.last_mut()
        {
            let local = (pos - rect.min).to_pos2();
            if stroke.last() != Some(&local) {
                stroke.push(local);
            }
        }
        let ink = ink_color(self.blue_ink);
        for stroke in &self.strokes {
            let points: Vec<egui::Pos2> = stroke.iter().map(|p| rect.min + p.to_vec2()).collect();
            painter.add(egui::Shape::line(points, egui::Stroke::new(3.2, ink)));
        }
        response.on_hover_cursor(egui::CursorIcon::Crosshair);
    }
}

fn ink_color(blue: bool) -> egui::Color32 {
    if blue {
        egui::Color32::from_rgb(0x1a, 0x3a, 0x9c)
    } else {
        egui::Color32::from_rgb(0x15, 0x15, 0x18)
    }
}

impl crate::Page for SignPage {
    fn add_files(&mut self, files: &[PathBuf]) {
        // Files from the right-click menu arrive before the screen draws: they wait for it.
        let _ = self.pdfs.0.send(files.to_vec());
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Some(jobs) = &mut self.jobs {
            jobs.poll();
        }
        let (dropped, hovering) = theme::dropped_files(&ctx);
        if !dropped.is_empty() {
            self.add(&dropped, &ctx);
        }
        while let Ok(files) = self.pdfs.1.try_recv() {
            self.add(&files, &ctx);
        }
        if let Ok(files) = self.picture.1.try_recv()
            && let Some(file) = files.first()
        {
            match from_picture(file) {
                Ok(image) => self.set_signature(image),
                Err(error) => self.error = Some(error),
            }
        }

        theme::page_header(
            ui,
            "Sign PDF",
            "Your signature on PDFs, without printing. A signed copy is saved next to each one as name-signed.",
        );
        ui.add_space(14.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                theme::card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new("Your signature").strong());
                    ui.add_space(6.0);
                    let drawing = self.drawing || self.signature.is_none();
                    if drawing {
                        self.pad(ui);
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut self.blue_ink, true, "Blue");
                            ui.radio_value(&mut self.blue_ink, false, "Black");
                            ui.add_space(12.0);
                            if ui.button("Clear").clicked() {
                                self.strokes.clear();
                            }
                            if ui
                                .add_enabled(
                                    !self.strokes.is_empty(),
                                    egui::Button::new(
                                        egui::RichText::new("Use this signature")
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(theme::ACCENT),
                                )
                                .clicked()
                            {
                                let c = ink_color(self.blue_ink);
                                let image = render(&self.strokes, [c.r(), c.g(), c.b()]);
                                self.set_signature(image);
                            }
                            if ui.button("Load a picture…").clicked() {
                                jobs::pick_files(
                                    &ctx,
                                    self.picture.0.clone(),
                                    "A picture of your signature",
                                    ("Images", &["png", "jpg", "jpeg", "webp"]),
                                );
                            }
                            if self.signature.is_some() && ui.button("Cancel").clicked() {
                                self.drawing = false;
                                self.strokes.clear();
                            }
                        });
                    } else if let Some(signature) = &self.signature {
                        let texture = self.texture.get_or_insert_with(|| {
                            let size = [signature.width() as usize, signature.height() as usize];
                            ctx.load_texture(
                                "signature",
                                egui::ColorImage::from_rgba_unmultiplied(size, signature.as_raw()),
                                egui::TextureOptions::LINEAR,
                            )
                        });
                        ui.horizontal(|ui| {
                            egui::Frame::new()
                                .fill(egui::Color32::WHITE)
                                .corner_radius(8.0)
                                .inner_margin(egui::Margin::same(10))
                                .show(ui, |ui| {
                                    let size = texture.size_vec2();
                                    let scale = (260.0 / size.x).min(80.0 / size.y);
                                    ui.image((texture.id(), size * scale));
                                });
                            if ui.button("Draw again").clicked() {
                                self.drawing = true;
                            }
                        });
                    }
                });
                ui.add_space(10.0);

                theme::card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new("Where it goes").strong());
                            ui.add_space(4.0);
                            let p = &mut self.placement;
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Page").color(theme::MUTED));
                                ui.radio_value(&mut p.pages, Pages::First, "First");
                                ui.radio_value(&mut p.pages, Pages::Last, "Last");
                                ui.radio_value(&mut p.pages, Pages::All, "All");
                            });
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Corner").color(theme::MUTED));
                                egui::ComboBox::from_id_salt("corner")
                                    .selected_text(corner_label(p.corner))
                                    .show_ui(ui, |ui| {
                                        for c in [
                                            Corner::BottomRight,
                                            Corner::BottomCenter,
                                            Corner::BottomLeft,
                                            Corner::TopRight,
                                        ] {
                                            ui.selectable_value(&mut p.corner, c, corner_label(c));
                                        }
                                    });
                            });
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Size").color(theme::MUTED));
                                ui.add(
                                    egui::Slider::new(&mut p.width, 0.12..=0.5).show_value(false),
                                );
                            });
                        });
                        ui.add_space(20.0);
                        preview(ui, &self.placement, self.texture.as_ref());
                    });
                });
                ui.add_space(10.0);
                if theme::drop_zone(
                    ui,
                    hovering,
                    "Drop PDFs here to sign them",
                    "or click to choose · also from the right-click menu in Files",
                )
                .clicked()
                {
                    jobs::pick_files(&ctx, self.pdfs.0.clone(), "Sign PDF", ("PDF", &["pdf"]));
                }
                if let Some(error) = &self.error {
                    ui.label(egui::RichText::new(error).color(theme::WARNING));
                }
                ui.add_space(10.0);
                if let Some(jobs) = &mut self.jobs {
                    jobs.ui(ui);
                }
            });
    }
}

fn corner_label(c: Corner) -> &'static str {
    match c {
        Corner::BottomRight => "Bottom right",
        Corner::BottomCenter => "Bottom center",
        Corner::BottomLeft => "Bottom left",
        Corner::TopRight => "Top right",
    }
}

/// A small sheet showing where the signature will be.
fn preview(ui: &mut egui::Ui, p: &Placement, texture: Option<&egui::TextureHandle>) {
    let page = egui::vec2(90.0, 127.0);
    let (rect, _) = ui.allocate_exact_size(page, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, egui::Color32::WHITE);
    for i in 0..7 {
        let y = rect.top() + 14.0 + i as f32 * 9.0;
        painter.line_segment(
            [
                egui::pos2(rect.left() + 10.0, y),
                egui::pos2(rect.right() - 10.0, y),
            ],
            egui::Stroke::new(2.0, egui::Color32::from_gray(225)),
        );
    }
    let margin = MARGIN / 595.0 * page.x;
    let sw = page.x * p.width;
    let aspect = texture.map_or(0.35, |t| t.size_vec2().y / t.size_vec2().x);
    let sh = sw * aspect;
    let x = match p.corner {
        Corner::BottomLeft => rect.left() + margin,
        Corner::BottomCenter => rect.center().x - sw / 2.0,
        Corner::BottomRight | Corner::TopRight => rect.right() - margin - sw,
    };
    let y = match p.corner {
        Corner::TopRight => rect.top() + margin,
        _ => rect.bottom() - margin - sh,
    };
    let sig = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(sw, sh));
    match texture {
        Some(t) => {
            painter.image(
                t.id(),
                sig,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        None => {
            painter.rect_filled(sig, 2.0, theme::ACCENT.gamma_multiply(0.5));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Signs $SIGN_TEST_PDF with a scribble and prints where the copy went.
    #[test]
    #[ignore]
    fn sign_a_pdf() {
        let pdf = std::env::var("SIGN_TEST_PDF").expect("set SIGN_TEST_PDF");
        let strokes = vec![
            (0..60)
                .map(|i| egui::pos2(40.0 + i as f32 * 5.0, 80.0 + (i as f32 * 0.4).sin() * 30.0))
                .collect::<Vec<_>>(),
        ];
        let signature = render(&strokes, [0x1a, 0x3a, 0x9c]);
        let out = sign_pdf(
            pdf.as_ref(),
            &signature,
            Placement {
                pages: Pages::Last,
                corner: Corner::BottomRight,
                width: 0.3,
            },
        )
        .unwrap();
        println!("{}", out.display());
    }

    /// Like sign_a_pdf, on a copy of $SIGN_TEST_PDF turned 90 degrees.
    #[test]
    #[ignore]
    fn sign_a_turned_pdf() {
        let pdf = std::env::var("SIGN_TEST_PDF").expect("set SIGN_TEST_PDF");
        let mut doc = Document::load(&pdf).unwrap();
        let first = *doc.get_pages().values().next().unwrap();
        doc.get_object_mut(first)
            .and_then(Object::as_dict_mut)
            .unwrap()
            .set("Rotate", 90);
        let turned = std::path::Path::new(&pdf).with_file_name("turned.pdf");
        doc.save(&turned).unwrap();
        let strokes = vec![(0..60)
            .map(|i| egui::pos2(40.0 + i as f32 * 5.0, 80.0 + (i as f32 * 0.4).sin() * 30.0))
            .collect::<Vec<_>>()];
        let signature = render(&strokes, [0x1a, 0x3a, 0x9c]);
        let placement = Placement {
            pages: Pages::First,
            corner: Corner::BottomRight,
            width: 0.3,
        };
        println!("{}", sign_pdf(&turned, &signature, placement).unwrap().display());
    }

    #[test]
    fn upright_on_turned_pages() {
        let bx = [0.0, 0.0, 600.0, 800.0];
        // The signature's bottom-left corner, as shown, lands where it should on the page.
        for (rotation, expected) in [(0, (10.0, 20.0)), (90, (580.0, 10.0)), (180, (590.0, 780.0)), (270, (20.0, 790.0))] {
            let [_, _, _, _, e, f] = placement_matrix(bx, rotation, (10.0, 20.0, 50.0, 30.0));
            assert_eq!((e, f), expected, "rotation {rotation}");
        }
    }
}
