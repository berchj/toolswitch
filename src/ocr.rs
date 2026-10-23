// Text from screen: reads the text in part of a screenshot, offline. ocrs finds the lines of
// text and PaddleOCR's Latin model (PP-OCRv5, Apache 2.0) reads them, accents and ñ included.
// The models (~10 MB) are downloaded the first time it's used.

use ocrs::{ImageSource, OcrEngine, OcrEngineParams};
use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, Tensor};
use std::sync::OnceLock;

const OCRS_URL: &str = "https://ocrs-models.s3-accelerate.amazonaws.com";
const DETECTION: &str = "text-detection.onnx";
const PADDLE_URL: &str = "https://huggingface.co/monkt/paddleocr-onnx/resolve/main/languages/latin";
const RECOGNITION: &str = "latin-PP-OCRv5-rec.onnx";
const DICT: &str = "latin-PP-OCRv5-dict.txt";
/// ocrs' own reader, used before: it only knew letters without accents.
const OLD_RECOGNITION: &str = "text-recognition.onnx";
/// The height PaddleOCR reads lines at.
const LINE_HEIGHT: u32 = 48;

/// Finds the lines; loading takes a moment, so it happens once.
fn finder() -> Result<&'static OcrEngine, String> {
    static ENGINE: OnceLock<Result<OcrEngine, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let detection = crate::models::load(DETECTION, &format!("{OCRS_URL}/{DETECTION}"))?;
            OcrEngine::new(OcrEngineParams {
                detection_model: Some(detection),
                ..Default::default()
            })
            .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

struct Reader {
    model: rten::Model,
    /// Index 0 is "nothing here"; then the dictionary's characters, then a space.
    chars: Vec<char>,
}

fn reader() -> Result<&'static Reader, String> {
    static READER: OnceLock<Result<Reader, String>> = OnceLock::new();
    READER
        .get_or_init(|| {
            let model = crate::models::load(RECOGNITION, &format!("{PADDLE_URL}/rec.onnx"))?;
            let dict = crate::models::get(DICT, &format!("{PADDLE_URL}/dict.txt"))?;
            let dict = std::fs::read_to_string(dict).map_err(|e| format!("{DICT}: {e}"))?;
            let chars = std::iter::once('\0')
                .chain(dict.lines().filter_map(|l| l.chars().next()))
                .chain(std::iter::once(' '))
                .collect();
            let _ = std::fs::remove_file(crate::models::dir().join(OLD_RECOGNITION));
            Ok(Reader { model, chars })
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The text in the image, one line per line of text.
pub fn read(image: &image::RgbImage) -> Result<String, String> {
    let finder = finder()?;
    let reader = reader()?;
    let source =
        ImageSource::from_bytes(image.as_raw(), image.dimensions()).map_err(|e| e.to_string())?;
    let input = finder.prepare_input(source).map_err(|e| e.to_string())?;
    let words = finder.detect_words(&input).map_err(|e| e.to_string())?;
    let lines = finder.find_text_lines(&input, &words);

    let mut text = vec![];
    for line in lines {
        // The box around the line's words, with a little room: letters touch its edges.
        let corners = line.iter().flat_map(|w| w.corners());
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in corners {
            (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
        }
        let pad = (y1 - y0) * 0.15;
        let x0 = (x0 - pad).max(0.0) as u32;
        let y0 = (y0 - pad).max(0.0) as u32;
        let x1 = ((x1 + pad) as u32).min(image.width());
        let y1 = ((y1 + pad) as u32).min(image.height());
        if x1 <= x0 + 2 || y1 <= y0 + 2 {
            continue;
        }
        let crop = image::imageops::crop_imm(image, x0, y0, x1 - x0, y1 - y0).to_image();
        let read = read_line(reader, &crop)?;
        if !read.trim().is_empty() {
            text.push(read.trim().to_string());
        }
    }
    Ok(text.join("\n"))
}

/// One line of text, read by PaddleOCR.
fn read_line(reader: &Reader, line: &image::RgbImage) -> Result<String, String> {
    let width = (line.width() as f32 * LINE_HEIGHT as f32 / line.height() as f32)
        .round()
        .clamp(16.0, 4000.0) as u32;
    let small = image::imageops::resize(
        line,
        width,
        LINE_HEIGHT,
        image::imageops::FilterType::Triangle,
    );
    // -1..1 per channel.
    let (w, h) = (width as usize, LINE_HEIGHT as usize);
    let mut input = vec![0f32; 3 * w * h];
    for (i, pixel) in small.pixels().enumerate() {
        for c in 0..3 {
            input[c * w * h + i] = pixel[c] as f32 / 127.5 - 1.0;
        }
    }
    let input = Tensor::from_data(&[1, 3, h, w], input);
    // [1, steps, characters]: how likely each character is at each step.
    let output: NdTensor<f32, 3> = reader
        .model
        .run_one(input.view().into(), None)
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|e: rten::TryFromValueError| e.to_string())?;
    let [_, steps, classes] = output.shape();
    let mut text = String::new();
    let mut last = 0;
    for step in 0..steps {
        let best = (0..classes)
            .max_by(|&a, &b| output[[0, step, a]].total_cmp(&output[[0, step, b]]))
            .unwrap_or(0);
        // A repeat of the previous step is the same letter, unless "nothing" came between.
        if best != 0
            && best != last
            && let Some(&c) = reader.chars.get(best)
        {
            text.push(c);
        }
        last = best;
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    // OCR_TEST_IMAGE=photo.png cargo test --release ocr_image -- --ignored --nocapture
    #[test]
    #[ignore]
    fn ocr_image() {
        let path = std::env::var("OCR_TEST_IMAGE").expect("set OCR_TEST_IMAGE");
        let image = image::open(path).unwrap().into_rgb8();
        let start = std::time::Instant::now();
        println!("{}", super::read(&image).unwrap());
        println!("({:?})", start.elapsed());
    }
}
