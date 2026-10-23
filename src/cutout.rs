// Remove background: BiRefNet (general, lite; MIT) finds the subject and everything else
// becomes transparent. Runs offline with rten; the model (~220 MB) is downloaded the first time.

use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, Tensor};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MODEL: &str = "BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx";
const MODEL_URL: &str = "https://github.com/danielgatis/rembg/releases/download/v0.0.0/BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx";
const MODEL_SHA256: &str = "5600024376f572a557870a5eb0afb1e5961636bef4e1e22132025467d0f03333";
/// The model used before: much worse with light clothes; deleted to free its 170 MB.
const OLD_MODEL: &str = "isnet-general-use.onnx";
const SIZE: usize = 1024;

pub const EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp"];

pub fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTS.contains(&e.to_lowercase().as_str()))
}

fn model_path() -> Result<PathBuf, String> {
    let path = crate::models::get(MODEL, MODEL_URL, MODEL_SHA256)?;
    let _ = std::fs::remove_file(crate::models::dir().join(OLD_MODEL));
    Ok(path)
}

/// The model while it's in use, and when it last ran. It takes a few hundred MB, so it's let
/// go after IDLE without use (see `release_if_idle`) and loaded again when needed.
static LOADED: Mutex<Option<(rten::Model, Instant)>> = Mutex::new(None);
const IDLE: Duration = Duration::from_secs(5 * 60);

/// Called from the background thread every few seconds.
pub fn release_if_idle() {
    // Busy with an image: it's in use.
    let Ok(mut loaded) = LOADED.try_lock() else {
        return;
    };
    if loaded.as_ref().is_some_and(|(_, last)| last.elapsed() > IDLE) {
        *loaded = None;
        crate::models::give_back_memory();
    }
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
    // Same preprocessing as rembg: 0..1 by the brightest value, then ImageNet's mean and spread.
    let max = small.as_raw().iter().copied().max().unwrap_or(1).max(1) as f32;
    let mean = [0.485, 0.456, 0.406];
    let std = [0.229, 0.224, 0.225];
    let mut input = vec![0f32; 3 * SIZE * SIZE];
    for (i, pixel) in small.pixels().enumerate() {
        for c in 0..3 {
            input[c * SIZE * SIZE + i] = (pixel[c] as f32 / max - mean[c]) / std[c];
        }
    }
    let input = Tensor::from_data(&[1, 3, SIZE, SIZE], input);

    // One image at a time, so the memory stays at one run's worth.
    let mut loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    if loaded.is_none() {
        let model = rten::Model::load_file(model_path()?).map_err(|e| format!("{MODEL}: {e}"))?;
        *loaded = Some((model, Instant::now()));
    }
    let (model, last) = loaded.as_mut().ok_or("The model isn't loaded")?;
    *last = Instant::now();
    let output: NdTensor<f32, 4> = model
        .run_one(input.view().into(), None)
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|e: rten::TryFromValueError| e.to_string())?;
    drop(loaded);

    // The model gives scores; the sigmoid turns them into 0..1.
    let pred: Vec<f32> = output.iter().map(|v| 1.0 / (1.0 + (-v).exp())).collect();
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
    // Written aside and renamed, so nobody opens it half written.
    let part = out.with_extension("part.png");
    rgba.save(&part).map_err(|e| e.to_string())?;
    std::fs::rename(&part, &out).map_err(|e| e.to_string())?;
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
        println!("memory with the model: {}", rss());
        // Pretend it's been idle long enough, then let it go.
        if let Some((_, last)) = super::LOADED.lock().unwrap().as_mut() {
            *last -= super::IDLE * 2;
        }
        super::release_if_idle();
        println!("memory after letting it go: {}", rss());
    }

    fn rss() -> String {
        std::fs::read_to_string("/proc/self/status")
            .unwrap_or_default()
            .lines()
            .find(|l| l.starts_with("VmRSS"))
            .unwrap_or_default()
            .to_string()
    }
}
