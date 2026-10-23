// Compress files, like tinyswitch: JPG and PNG images, Word/PowerPoint/Excel/LibreOffice
// documents (the images inside) and PDFs (their JPEG photos). The result is saved next to the
// original as "name-min.ext" and never replaces it. Settings live in ~/.config/toolswitch/compress.json.

use crate::config;
use image::{DynamicImage, ImageDecoder, ImageReader};
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

pub const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png"];
pub const DOC_EXTS: &[&str] = &["docx", "pptx", "xlsx", "odt", "odp", "ods", "pdf"];
// Under 3% smaller isn't worth a second file.
const WORTH_IT: f64 = 0.97;

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Same,
    Jpeg,
    Png,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub quality: u8,
    /// 0 keeps the original size.
    pub max_width: u32,
    pub format: Format,
    pub suffix: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            quality: 80,
            max_width: 0,
            format: Format::Same,
            suffix: "-min".into(),
        }
    }
}

fn file() -> PathBuf {
    config::dir().join("compress.json")
}

/// Brought over from tinyswitch the first time.
pub fn load() -> Settings {
    let read = |p: PathBuf| {
        std::fs::read(p)
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
    };
    read(file())
        .or_else(|| read(config::config_dir().join("tinyswitch").join("config.json")))
        .unwrap_or_default()
}

pub fn save(settings: &Settings) {
    let _ = config::write_json(&file(), settings);
}

pub struct Outcome {
    /// None when the result wasn't meaningfully smaller (nothing is written then).
    pub out: Option<PathBuf>,
    pub before: u64,
    pub after: u64,
}

fn ext(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn supported(path: &Path) -> bool {
    let e = ext(path);
    IMAGE_EXTS.contains(&e.as_str()) || DOC_EXTS.contains(&e.as_str())
}

/// Files and folders (searched recursively) -> the files that can be compressed.
pub fn collect(paths: &[PathBuf]) -> Vec<PathBuf> {
    crate::jobs::collect(paths, supported)
}

/// photo.jpg -> photo-min.jpg, or photo-min-2.jpg if that exists. Never the original.
fn output_path(file: &Path, ext: &str, suffix: &str) -> PathBuf {
    let dir = file.parent().unwrap_or(Path::new("."));
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    let suffix = if suffix.is_empty() { "-min" } else { suffix };
    (1..)
        .map(|i| {
            let n = if i > 1 {
                format!("-{i}")
            } else {
                String::new()
            };
            dir.join(format!("{stem}{suffix}{n}.{ext}"))
        })
        .find(|p| !p.exists())
        .unwrap()
}

pub fn compress(file: &Path, settings: &Settings) -> Result<Outcome, String> {
    match ext(file).as_str() {
        "pdf" => compress_pdf(file, settings),
        e if DOC_EXTS.contains(&e) => compress_office(file, settings),
        _ => compress_image(file, settings),
    }
}

fn skipped(before: u64) -> Outcome {
    Outcome {
        out: None,
        before,
        after: before,
    }
}

fn write(
    file: &Path,
    ext: &str,
    settings: &Settings,
    data: &[u8],
    before: u64,
) -> Result<Outcome, String> {
    let out = output_path(file, ext, &settings.suffix);
    std::fs::write(&out, data).map_err(|e| e.to_string())?;
    Ok(Outcome {
        out: Some(out),
        before,
        after: data.len() as u64,
    })
}

// ---------- images ----------

/// Decodes and turns the photo upright (phones store it sideways plus an EXIF note).
fn open(data: &[u8]) -> Result<DynamicImage, String> {
    let mut decoder = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().ok();
    let mut image = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    if let Some(o) = orientation {
        image.apply_orientation(o);
    }
    Ok(image)
}

fn shrink(image: DynamicImage, max_width: u32) -> DynamicImage {
    if max_width == 0 || image.width() <= max_width {
        return image;
    }
    let height = (image.height() as f64 * max_width as f64 / image.width() as f64).round() as u32;
    image.resize_exact(
        max_width,
        height.max(1),
        image::imageops::FilterType::Lanczos3,
    )
}

fn encode(image: &DynamicImage, format: Format, quality: u8) -> Result<Vec<u8>, String> {
    match format {
        Format::Png => encode_png(image),
        _ => encode_jpeg(image, quality),
    }
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = vec![];
    let mut encoder = jpeg_encoder::Encoder::new(&mut out, quality);
    encoder.set_progressive(true);
    encoder.set_optimized_huffman_tables(true);
    let (w, h) = (image.width() as u16, image.height() as u16);
    if image.color().has_color() {
        encoder.encode(&image.to_rgb8(), w, h, jpeg_encoder::ColorType::Rgb)
    } else {
        encoder.encode(&image.to_luma8(), w, h, jpeg_encoder::ColorType::Luma)
    }
    .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Lossy palette PNG (like TinyPNG): up to 256 colors, far smaller, visually the same.
fn encode_png(image: &DynamicImage) -> Result<Vec<u8>, String> {
    let rgba = image.to_rgba8();
    // Learning from every 10th pixel is plenty, and keeps big screenshots fast.
    let quant = color_quant::NeuQuant::new(10, 256, rgba.as_raw());
    // Looking up each pixel is the slow part: similar colors (same top 5 bits per channel,
    // 4 for alpha) share one lookup.
    let mut cache = vec![u16::MAX; 1 << 19];
    let indices: Vec<u8> = rgba
        .pixels()
        .map(|p| {
            let [r, g, b, a] = p.0;
            let key = (r as usize >> 3) << 14
                | (g as usize >> 3) << 9
                | (b as usize >> 3) << 4
                | a as usize >> 4;
            if cache[key] == u16::MAX {
                cache[key] = quant.index_of(&p.0) as u16;
            }
            cache[key] as u8
        })
        .collect();
    let map = quant.color_map_rgba();
    let palette: Vec<u8> = map.chunks(4).flat_map(|c| [c[0], c[1], c[2]]).collect();
    let alpha: Vec<u8> = map.chunks(4).map(|c| c[3]).collect();

    let mut out = vec![];
    {
        let mut encoder = png::Encoder::new(&mut out, image.width(), image.height());
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_palette(palette);
        if alpha.iter().any(|&a| a < 255) {
            encoder.set_trns(alpha);
        }
        encoder.set_compression(png::Compression::High);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(&indices)
            .map_err(|e| e.to_string())?;
    }
    Ok(out)
}

fn compress_image(file: &Path, settings: &Settings) -> Result<Outcome, String> {
    let data = std::fs::read(file).map_err(|e| e.to_string())?;
    let before = data.len() as u64;
    let source = if ext(file) == "png" {
        Format::Png
    } else {
        Format::Jpeg
    };
    let target = if settings.format == Format::Same {
        source
    } else {
        settings.format
    };
    let image = open(&data)?;
    let resized = settings.max_width > 0 && image.width() > settings.max_width;
    let out = encode(&shrink(image, settings.max_width), target, settings.quality)?;
    if out.len() as f64 > before as f64 * WORTH_IT && target == source && !resized {
        return Ok(skipped(before));
    }
    let ext = match target {
        Format::Png => "png",
        _ if source == Format::Jpeg => &self::ext(file),
        _ => "jpg",
    };
    write(file, ext, settings, &out, before)
}

// ---------- Word, PowerPoint, Excel and LibreOffice ----------

/// Images inside keep their name and format (the document points at them by name);
/// only smaller results replace the originals. Everything else is copied as is.
fn compress_office(file: &Path, settings: &Settings) -> Result<Outcome, String> {
    let data = std::fs::read(file).map_err(|e| e.to_string())?;
    let before = data.len() as u64;
    let mut zip = zip::ZipArchive::new(Cursor::new(&data)).map_err(|e| e.to_string())?;

    let mut entries: Vec<(String, Vec<u8>, bool)> = vec![];
    let mut changed = false;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let mut bytes = vec![];
        entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        let is_media = ["word/media/", "ppt/media/", "xl/media/", "Pictures/"]
            .iter()
            .any(|m| name.starts_with(m));
        let format = match ext(Path::new(&name)).as_str() {
            "jpg" | "jpeg" => Some(Format::Jpeg),
            "png" => Some(Format::Png),
            _ => None,
        };
        if let (true, Some(format)) = (is_media, format)
            // An image that can't be read stays as it was.
            && let Ok(image) = open(&bytes)
            && let Ok(out) = encode(&shrink(image, settings.max_width), format, settings.quality)
            && (out.len() as f64) < bytes.len() as f64 * WORTH_IT
        {
            bytes = out;
            changed = true;
        }
        entries.push((name, bytes, is_media));
    }
    if !changed {
        return Ok(skipped(before));
    }

    // ODF needs "mimetype" first and uncompressed; images are already compressed.
    entries.sort_by_key(|(name, ..)| name != "mimetype");
    let mut out = Cursor::new(vec![]);
    {
        let mut writer = zip::ZipWriter::new(&mut out);
        for (name, bytes, is_media) in &entries {
            let method = if name == "mimetype" || *is_media {
                zip::CompressionMethod::Stored
            } else {
                zip::CompressionMethod::Deflated
            };
            let options = zip::write::SimpleFileOptions::default().compression_method(method);
            writer
                .start_file(name.as_str(), options)
                .map_err(|e| e.to_string())?;
            writer.write_all(bytes).map_err(|e| e.to_string())?;
        }
        writer.finish().map_err(|e| e.to_string())?;
    }
    let out = out.into_inner();
    if out.len() as f64 > before as f64 * WORTH_IT {
        return Ok(skipped(before));
    }
    write(file, &ext(file), settings, &out, before)
}

// ---------- PDF ----------

/// Only plain JPEG photos in RGB or gray are touched: those are what make scanned and photo
/// PDFs heavy. CMYK, masked or otherwise special images are left as they are.
fn compress_pdf(file: &Path, settings: &Settings) -> Result<Outcome, String> {
    use lopdf::Object;
    let data = std::fs::read(file).map_err(|e| e.to_string())?;
    let before = data.len() as u64;
    let mut doc = lopdf::Document::load_mem(&data).map_err(|e| e.to_string())?;

    let mut replaced = vec![];
    for (id, object) in &doc.objects {
        let Object::Stream(stream) = object else {
            continue;
        };
        let dict = &stream.dict;
        let name = |key: &[u8]| dict.get(key).ok().and_then(|o| o.as_name().ok());
        if name(b"Subtype") != Some(b"Image")
            || dict.has(b"Mask")
            || dict.has(b"Decode")
            || dict.has(b"SMask")
        {
            continue;
        }
        let is_jpeg = match dict.get(b"Filter") {
            Ok(Object::Name(n)) => n == b"DCTDecode",
            Ok(Object::Array(a)) => a.len() == 1 && a[0].as_name().ok() == Some(b"DCTDecode"),
            _ => false,
        };
        let gray = match name(b"ColorSpace") {
            Some(b"DeviceRGB") => false,
            Some(b"DeviceGray") => true,
            _ => continue,
        };
        if !is_jpeg {
            continue;
        }
        let Ok(image) = open(&stream.content) else {
            continue;
        };
        if image.color().has_color() == gray {
            continue;
        }
        let image = shrink(image, settings.max_width);
        let image = if gray {
            DynamicImage::ImageLuma8(image.to_luma8())
        } else {
            image
        };
        let Ok(out) = encode_jpeg(&image, settings.quality) else {
            continue;
        };
        if (out.len() as f64) < stream.content.len() as f64 * WORTH_IT {
            replaced.push((*id, out, image.width(), image.height()));
        }
    }
    if replaced.is_empty() {
        return Ok(skipped(before));
    }
    for (id, out, width, height) in replaced {
        if let Some(Object::Stream(stream)) = doc.objects.get_mut(&id) {
            // The page decides the size it's drawn at, so a smaller image fits the same box.
            stream.dict.set("Width", width as i64);
            stream.dict.set("Height", height as i64);
            stream.dict.set("BitsPerComponent", 8);
            stream
                .dict
                .set("Filter", Object::Name(b"DCTDecode".to_vec()));
            stream.dict.remove(b"DecodeParms");
            stream.set_content(out);
        }
    }
    let mut out = vec![];
    doc.save_to(&mut out).map_err(|e| e.to_string())?;
    if out.len() as f64 > before as f64 * WORTH_IT {
        return Ok(skipped(before));
    }
    write(file, "pdf", settings, &out, before)
}

pub fn format_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.0} KB", bytes as f64 / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
        _ => format!("{:.1} GB", bytes as f64 / 1_073_741_824.0),
    }
}

#[cfg(test)]
mod tests {
    /// Compresses every file in $COMPRESS_TEST_DIR and prints the sizes:
    /// COMPRESS_TEST_DIR=/some/dir cargo test --release compress_folder -- --ignored --nocapture
    #[test]
    #[ignore]
    fn compress_folder() {
        let dir = std::env::var("COMPRESS_TEST_DIR").expect("COMPRESS_TEST_DIR");
        let settings = super::Settings::default();
        for file in super::collect(&[dir.into()]) {
            let start = std::time::Instant::now();
            match super::compress(&file, &settings) {
                Ok(o) => println!(
                    "{:?}: {} -> {} {:?} in {:?}",
                    file.file_name().unwrap(),
                    o.before,
                    o.after,
                    o.out,
                    start.elapsed()
                ),
                Err(e) => println!("{:?}: error {e}", file.file_name().unwrap()),
            }
        }
    }
}
