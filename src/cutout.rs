// Remove background: ISNet (general use, Apache 2.0) finds the subject and everything else
// becomes transparent. Runs offline with rten; the model (~170 MB) is downloaded the first time.

use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, Tensor};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MODEL: &str = "isnet-general-use.onnx";
const MODEL_URL: &str =
    "https://github.com/danielgatis/rembg/releases/download/v0.0.0/isnet-general-use.onnx";
const SIZE: usize = 1024;

pub const EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp"];

pub fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTS.contains(&e.to_lowercase().as_str()))
}

/// cutswitch already downloaded the same model: use it instead of downloading it again.
fn model_path() -> Result<PathBuf, String> {
    let old = crate::config::cache_dir()
        .with_file_name("cutswitch")
        .join("models")
        .join(MODEL);
    if old.exists() && !crate::models::dir().join(MODEL).exists() {
        return Ok(old);
    }
    crate::models::get(MODEL, MODEL_URL)
}

/// Loaded once; one image at a time, so the memory stays at one run's worth.
fn model() -> Result<&'static Mutex<rten::Model>, String> {
    static MODEL_CELL: OnceLock<Result<Mutex<rten::Model>, String>> = OnceLock::new();
    MODEL_CELL
        .get_or_init(|| {
            let path = model_path()?;
            rten::Model::load_file(&path)
                .map(Mutex::new)
                .map_err(|e| format!("{MODEL}: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The image the way people see it (phone photos are often stored sideways).
fn open(path: &Path) -> Result<image::DynamicImage, String> {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().ok();
    let mut image = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    Ok(image)
}

/// 0..255 per pixel, 255 where the subject is, at the image's size.
fn mask(rgb: &image::RgbImage) -> Result<image::GrayImage, String> {
    let small = image::imageops::resize(
        rgb,
        SIZE as u32,
        SIZE as u32,
        image::imageops::FilterType::Triangle,
    );
    // Same preprocessing as rembg: 0..1 by the brightest value, minus the mean.
    let max = small.as_raw().iter().copied().max().unwrap_or(1).max(1) as f32;
    let mean = [0.485, 0.456, 0.406];
    let mut input = vec![0f32; 3 * SIZE * SIZE];
    for (i, pixel) in small.pixels().enumerate() {
        for c in 0..3 {
            input[c * SIZE * SIZE + i] = pixel[c] as f32 / max - mean[c];
        }
    }
    let input = Tensor::from_data(&[1, 3, SIZE, SIZE], input);

    let model = model()?
        .lock()
        .map_err(|_| "The model crashed".to_string())?;
    let output: NdTensor<f32, 4> = model
        .run_one(input.view().into(), None)
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|e: rten::TryFromValueError| e.to_string())?;
    drop(model);

    let pred: Vec<f32> = output.iter().copied().collect();
    let (lo, hi) = pred
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let range = if hi > lo { hi - lo } else { 1.0 };
    let small_mask = image::GrayImage::from_fn(SIZE as u32, SIZE as u32, |x, y| {
        let v = pred[y as usize * SIZE + x as usize];
        image::Luma([((v - lo) / range * 255.0).round() as u8])
    });
    Ok(image::imageops::resize(
        &small_mask,
        rgb.width(),
        rgb.height(),
        image::imageops::FilterType::Lanczos3,
    ))
}

/// "photo.jpg" -> "photo-nobg.png", without overwriting anything.
fn output_path(file: &Path) -> PathBuf {
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    let dir = file.parent().unwrap_or(Path::new("."));
    let mut out = dir.join(format!("{stem}-nobg.png"));
    let mut n = 2;
    while out.exists() {
        out = dir.join(format!("{stem}-nobg-{n}.png"));
        n += 1;
    }
    out
}

/// Saves a transparent PNG next to the photo and returns its path.
pub fn remove_background(file: &Path) -> Result<PathBuf, String> {
    let rgb = open(file)?.into_rgb8();
    let mask = mask(&rgb)?;
    let rgba = image::RgbaImage::from_fn(rgb.width(), rgb.height(), |x, y| {
        let [r, g, b] = rgb.get_pixel(x, y).0;
        image::Rgba([r, g, b, mask.get_pixel(x, y)[0]])
    });
    let out = output_path(file);
    rgba.save(&out).map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    // CUTOUT_TEST_IMAGE=photo.jpg cargo test --release cutout_image -- --ignored --nocapture
    #[test]
    #[ignore]
    fn cutout_image() {
        let path = std::env::var("CUTOUT_TEST_IMAGE").expect("set CUTOUT_TEST_IMAGE");
        let start = std::time::Instant::now();
        let out = super::remove_background(path.as_ref()).unwrap();
        println!("{} ({:?})", out.display(), start.elapsed());
    }
}
