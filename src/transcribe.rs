// Audio to text: OpenAI's Whisper (MIT) turns speech into text in ~100 languages, with
// accents and ñ. Runs offline with rten; the model is downloaded the first time.
// The steps, as in the original Python: the sound in 30-second pieces -> log-mel spectrogram
// -> encoder -> the decoder writes the text, with timestamps that say where the next piece
// starts.

use crate::audio;
use rten::{Dimension, FloatOperators};
use rten_generate::filter::{LogitsFilter, token_id_filter};
use rten_generate::{Generator, GeneratorConfig, GeneratorUtils, Logits};
use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, NdTensorView, Tensor};
use rten_text::Tokenizer;
use rustfft::FftPlanner;
use rustfft::num_complex::Complex32;
use std::path::{Path, PathBuf};

const HF: &str = "https://huggingface.co/onnx-community";

/// Fast: Whisper base (~77 MB). Best: Whisper small (~250 MB), ~3x slower but fewer mistakes.
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, Default)]
pub enum Quality {
    #[default]
    Fast,
    Best,
}

impl Quality {
    fn size(self) -> &'static str {
        match self {
            Quality::Fast => "base",
            Quality::Best => "small",
        }
    }
}

const TOKENIZER: &str = "whisper-tokenizer.json";

pub use audio::{EXTS, supported};

const CHUNK_SECONDS: usize = 30;
const CHUNK: usize = audio::RATE as usize * CHUNK_SECONDS;
const N_FFT: usize = 400;
const HOP: usize = 160;
/// Most tokens the decoder may write per 30-second piece (from Hugging Face Transformers).
const MAX_TOKENS: usize = 448;

/// The model's file, downloaded the first time. "encoder" or "decoder".
fn model(quality: Quality, part: &str) -> Result<PathBuf, String> {
    let size = quality.size();
    let remote = if part == "encoder" { "encoder_model" } else { "decoder_model_merged" };
    crate::models::get(
        &format!("whisper-{size}-{part}.onnx"),
        &format!("{HF}/whisper-{size}/resolve/main/onnx/{remote}_quantized.onnx"),
    )
}

/// Hugging Face's tokenizer.json for Whisper predates the "ignore_merges" field rten-text
/// asks for; it's added as false, what older files meant.
fn tokenizer() -> Result<Tokenizer, String> {
    let json = std::fs::read_to_string(crate::models::get(
        TOKENIZER,
        &format!("{HF}/whisper-base/resolve/main/tokenizer.json"),
    )?).map_err(|e| e.to_string())?;
    let mut value: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    if let Some(model) = value.get_mut("model").and_then(|m| m.as_object_mut()) {
        model.entry("ignore_merges").or_insert(false.into());
    }
    Tokenizer::from_json(&value.to_string()).map_err(|e| e.to_string())
}

/// Mel filters as librosa makes them (Slaney scale and normalization), which is what Whisper
/// was trained with. `[n_mels, N_FFT / 2 + 1]`.
fn mel_filters(n_mels: usize) -> NdTensor<f32, 2> {
    let sr = audio::RATE as f64;
    let n_freqs = N_FFT / 2 + 1;
    let (f_sp, min_log_hz, min_log_mel) = (200.0 / 3.0, 1000.0, 15.0);
    let log_step = 6.4f64.ln() / 27.0;
    let hz_to_mel = |f: f64| {
        if f >= min_log_hz {
            min_log_mel + (f / min_log_hz).ln() / log_step
        } else {
            f / f_sp
        }
    };
    let mel_to_hz = |m: f64| {
        if m >= min_log_mel {
            min_log_hz * (log_step * (m - min_log_mel)).exp()
        } else {
            m * f_sp
        }
    };
    let top = hz_to_mel(sr / 2.0);
    let points: Vec<f64> = (0..n_mels + 2)
        .map(|i| mel_to_hz(top * i as f64 / (n_mels + 1) as f64))
        .collect();
    let mut filters = NdTensor::zeros([n_mels, n_freqs]);
    for m in 0..n_mels {
        let (lo, mid, hi) = (points[m], points[m + 1], points[m + 2]);
        let norm = 2.0 / (hi - lo);
        for k in 0..n_freqs {
            let f = k as f64 * sr / N_FFT as f64;
            let w = ((f - lo) / (mid - lo)).min((hi - f) / (hi - mid)).max(0.0);
            filters[[m, k]] = (w * norm) as f32;
        }
    }
    filters
}

/// Whisper's input for 30 s of sound: `[n_mels, 3000]`, like `log_mel_spectrogram` in
/// openai/whisper (torch.stft with center=True and the last frame dropped).
fn log_mel(samples: &[f32], filters: &NdTensor<f32, 2>) -> NdTensor<f32, 2> {
    let frames = CHUNK / HOP;
    let mut padded = vec![0f32; CHUNK];
    padded[..samples.len().min(CHUNK)].copy_from_slice(&samples[..samples.len().min(CHUNK)]);
    // Reflect padding of half a window on each side.
    let half = N_FFT / 2;
    let reflect = |i: isize| -> f32 {
        let n = CHUNK as isize;
        let j = if i < 0 { -i } else if i >= n { 2 * (n - 1) - i } else { i };
        padded[j as usize]
    };
    let window: Vec<f32> = (0..N_FFT)
        .map(|i| (std::f32::consts::PI * i as f32 / N_FFT as f32).sin().powi(2))
        .collect();
    let fft = FftPlanner::new().plan_fft_forward(N_FFT);
    let n_freqs = N_FFT / 2 + 1;
    let mut power = NdTensor::zeros([n_freqs, frames]);
    let mut buf = vec![Complex32::new(0.0, 0.0); N_FFT];
    for t in 0..frames {
        for (k, b) in buf.iter_mut().enumerate() {
            let i = (t * HOP + k) as isize - half as isize;
            *b = Complex32::new(reflect(i) * window[k], 0.0);
        }
        fft.process(&mut buf);
        for f in 0..n_freqs {
            power[[f, t]] = buf[f].norm_sqr();
        }
    }
    let mels: NdTensor<f32, 2> = filters
        .matmul(power.as_dyn())
        .expect("mel matmul")
        .try_into()
        .expect("2D");
    let mut mels = mels;
    mels.apply(|x| x.max(1e-10).log10());
    let max = mels.iter().copied().fold(f32::MIN, f32::max);
    mels.apply(|x| (x.max(max - 8.0) + 4.0) / 4.0);
    mels
}

/// Whisper's timestamp rules (ApplyTimestampRules in openai/whisper), simplified: no
/// <|notimestamps|>, timestamps don't go back, and a timestamp wins when timestamps are more
/// likely together than any single word.
struct TimestampFilter {
    first: u32,
    last: u32,
    no_timestamps: u32,
    prompt_len: usize,
}

impl TimestampFilter {
    fn is_timestamp(&self, token: u32) -> bool {
        (self.first..=self.last).contains(&token)
    }
}

impl LogitsFilter for TimestampFilter {
    fn filter(&self, logits: Logits, prev_tokens: &[u32]) -> Logits {
        let (mut logits, indices) = logits.into_logits_indices();
        let probs = NdTensorView::from(logits.as_slice()).softmax(-1).unwrap();
        let (mut timestamps, mut best_word) = (0.0, 0.0f32);
        for (token, p) in indices.iter().zip(probs.iter()) {
            if self.is_timestamp(*token) {
                timestamps += p;
            } else {
                best_word = best_word.max(*p);
            }
        }
        let only_timestamps = timestamps > best_word;
        let previous = prev_tokens[self.prompt_len.min(prev_tokens.len())..]
            .iter()
            .rev()
            .find(|t| self.is_timestamp(**t))
            .copied()
            .unwrap_or(self.first);
        for (&token, logit) in indices.iter().zip(logits.iter_mut()) {
            let stamp = self.is_timestamp(token);
            if (only_timestamps && !stamp)
                || token == self.no_timestamps
                || (stamp && token < previous)
            {
                *logit = f32::NEG_INFINITY;
            }
        }
        Logits::sparse(logits, indices)
    }
}

/// The decoder's "use the cache" switch, false on the first run. rten-generate passes it
/// as a single number but these models want a list of one.
fn cache_flag<'a>(_batch: usize, positions: std::ops::Range<usize>) -> rten::ValueOrView<'a> {
    Tensor::from_data(&[1], vec![i32::from(positions.start != 0)]).into()
}

fn generator<'a>(
    decoder: &'a rten::Model,
    kv_cache_capacity: Option<usize>,
) -> Result<Generator<'a>, String> {
    let mut config = GeneratorConfig {
        kv_cache_capacity,
        ..Default::default()
    };
    // Not a real input name, so rten-generate leaves the switch to cache_flag.
    config.model_inputs.use_cache_flag = "-";
    let flag = decoder.node_id("use_cache_branch").map_err(|e| e.to_string())?;
    Ok(Generator::from_model_config(decoder, config)
        .map_err(|e| e.to_string())?
        .with_varying_input(flag, &cache_flag::<'a>))
}

/// The text of a sound file. `progress` gets 0..1 as it goes.
pub fn text_of(file: &Path, quality: Quality, progress: &dyn Fn(f32)) -> Result<String, String> {
    let mut samples = audio::read(file)?;
    // Quiet recordings are brought up to full volume.
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    if peak > 0.0 && peak < 0.9 {
        let gain = 0.9 / peak;
        samples.iter_mut().for_each(|s| *s *= gain);
    }

    let tokenizer = tokenizer()?;
    let encoder_path = model(quality, "encoder")?;
    let decoder_path = model(quality, "decoder")?;
    // Loaded for each file and freed after: ~150 MB that only matter while it works.
    let encoder = rten::Model::load_file(&encoder_path).map_err(|e| format!("Whisper: {e}"))?;
    let decoder = rten::Model::load_file(&decoder_path).map_err(|e| format!("Whisper: {e}"))?;
    let token = |s: &str| tokenizer.get_token_id(s).map_err(|e| e.to_string());
    let start_of_transcript = token("<|startoftranscript|>")?;
    let end = token("<|endoftext|>")?;
    let start_of_prev = token("<|startofprev|>")?;
    let transcribe = token("<|transcribe|>")?;
    let first_stamp = token("<|0.00|>")?;
    let last_stamp = token("<|30.00|>")?;
    let no_timestamps = token("<|notimestamps|>")?;
    let (first_lang, last_lang) = (token("<|en|>")?, token("<|su|>")?);

    let n_mels = match encoder.input_shape(0).as_deref() {
        Some([_, Dimension::Fixed(mels), _]) => *mels,
        _ => 80,
    };
    let filters = mel_filters(n_mels);
    let input_id = encoder.node_id("input_features").map_err(|e| e.to_string())?;
    let output_id = encoder.node_id("last_hidden_state").map_err(|e| e.to_string())?;
    let hidden_id = decoder
        .node_id("encoder_hidden_states")
        .map_err(|e| e.to_string())?;

    let mut text = String::new();
    let mut language = None;
    let mut previous: Vec<u32> = vec![];
    let mut offset = 0;
    while offset < samples.len() {
        progress(offset as f32 / samples.len() as f32);
        let piece = &samples[offset..(offset + CHUNK).min(samples.len())];
        // Silence makes Whisper invent text ("Thanks for watching"): skip it.
        let loud = piece.iter().map(|s| s * s).sum::<f32>() / piece.len() as f32;
        if loud.sqrt() < 0.003 {
            offset += CHUNK;
            previous.clear();
            continue;
        }
        let mel = log_mel(piece, &filters).with_new_axis(0);
        let [encoded] = encoder
            .run_n([(input_id, mel.view().into())].into(), [output_id], None)
            .map_err(|e| e.to_string())?;
        let encoded: NdTensor<f32, 3> = encoded.try_into().map_err(|e: rten::TryFromValueError| e.to_string())?;

        // The language, from the first piece with speech.
        let lang = match language {
            Some(lang) => lang,
            None => {
                let mut guess = generator(&decoder, None)?
                    .with_prompt(&[start_of_transcript])
                    .with_constant_input(hidden_id, encoded.view().into())
                    .with_logits_filter(token_id_filter(move |t| {
                        (first_lang..=last_lang).contains(&t)
                    }))
                    .take(1);
                let lang = guess
                    .next()
                    .ok_or("Whisper gave no language")?
                    .map_err(|e| e.to_string())?;
                language = Some(lang);
                lang
            }
        };

        // What came before helps it keep names and style; then this piece's instructions.
        let mut prompt = vec![];
        if !previous.is_empty() {
            prompt.push(start_of_prev);
            prompt.extend(previous.iter().rev().take(MAX_TOKENS / 2 - 1).rev().copied());
        }
        prompt.extend([start_of_transcript, lang, transcribe]);
        previous.clear();
        let generator = generator(&decoder, Some(MAX_TOKENS + prompt.len()))?
            .with_prompt(&prompt)
            .with_constant_input(hidden_id, encoded.view().into())
            .with_logits_filter(TimestampFilter {
                first: first_stamp,
                last: last_stamp,
                no_timestamps,
                prompt_len: prompt.len(),
            })
            .take(MAX_TOKENS)
            .stop_on_tokens([end]);

        // Segments are words between two timestamps; the last one may be cut off by the end
        // of the piece, so the next piece starts at the last closed segment.
        let mut open: Option<u32> = None;
        let mut words = vec![];
        let mut closed_at = None;
        let mut segments = vec![];
        for t in generator {
            let t = t.map_err(|e| e.to_string())?;
            if t == end {
                break;
            }
            previous.push(t);
            if (first_stamp..=last_stamp).contains(&t) {
                match open.take() {
                    Some(_) => {
                        segments.push(tokenizer.decode(&words).map_err(|e| e.to_string())?);
                        words.clear();
                        closed_at = Some(t - first_stamp);
                    }
                    None => open = Some(t),
                }
            } else {
                words.push(t);
            }
        }
        for segment in segments {
            let segment = segment.trim();
            if !segment.is_empty() {
                if !text.is_empty() {
                    text.push(if text.ends_with(['.', '?', '!']) { '\n' } else { ' ' });
                }
                text.push_str(segment);
            }
        }
        // Timestamps come in 20 ms steps. The last piece ends with the file.
        let advance = match closed_at {
            Some(steps) if steps > 0 && piece.len() == CHUNK => steps as usize * 20 * 16,
            _ => CHUNK,
        };
        offset += advance;
    }
    progress(1.0);
    if text.is_empty() {
        return Err("No speech found".into());
    }
    Ok(text)
}

/// "voice.ogg" -> "voice.txt" next to it, without overwriting anything.
pub fn output_path(file: &Path) -> PathBuf {
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    let dir = file.parent().unwrap_or(Path::new("."));
    let mut out = dir.join(format!("{stem}.txt"));
    let mut n = 2;
    while out.exists() {
        out = dir.join(format!("{stem}-{n}.txt"));
        n += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn mel_filters_match_librosa() {
        // librosa.filters.mel(sr=16000, n_fft=400, n_mels=80): a few values and row sums.
        let f = super::mel_filters(80);
        assert!((f[[0, 1]] - 0.024_862_4).abs() < 1e-4, "{}", f[[0, 1]]);
        assert!(f[[0, 2]].abs() < 1e-6);
        let sum: f32 = (0..201).map(|k| f[[79, k]]).sum();
        assert!((sum - 0.024_925_3).abs() < 1e-4, "{sum}");
        let first = (0..201).map(|k| f[[40, k]]).find(|v| *v > 0.0).unwrap();
        assert!((first - 0.005_41).abs() < 1e-4, "{first}");
    }

    // TRANSCRIBE_TEST_FILE=voice.ogg [TRANSCRIBE_QUALITY=best] cargo test --release transcribe_file -- --ignored --nocapture
    #[test]
    #[ignore]
    fn transcribe_file() {
        let path = std::env::var("TRANSCRIBE_TEST_FILE").expect("set TRANSCRIBE_TEST_FILE");
        let start = std::time::Instant::now();
        let quality = match std::env::var("TRANSCRIBE_QUALITY").as_deref() {
            Ok("best") => super::Quality::Best,
            _ => super::Quality::Fast,
        };
        let text = super::text_of(path.as_ref(), quality, &|p| eprint!("\r{:.0}%", p * 100.0)).unwrap();
        println!("\n{text}\n({:?})", start.elapsed());
    }
}
