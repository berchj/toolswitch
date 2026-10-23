// Reading audio files (and the sound of videos) as 16 kHz mono, what speech models want.
// symphonia reads MP3, M4A/AAC, OGG, FLAC, WAV and the sound of MP4/MKV videos; Opus (WhatsApp
// voice notes, Telegram, WebM) goes through opus-decoder. Both are pure Rust.

use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, CODEC_TYPE_OPUS, DecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const RATE: u32 = 16_000;

pub const EXTS: &[&str] = &[
    "mp3", "m4a", "aac", "ogg", "oga", "opus", "flac", "wav", "mp4", "m4v", "mov", "mkv", "webm",
];

pub fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTS.contains(&e.to_lowercase().as_str()))
}

/// The file's sound as samples in -1..1, one channel, 16 000 per second.
pub fn read(path: &Path) -> Result<Vec<f32>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|_| "Not an audio file this can read".to_string())?
        .format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("The file has no sound")?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    let channels = params.channels.map_or(1, |c| c.count()).max(1);

    let mut mono = vec![];
    let mut rate = params.sample_rate.unwrap_or(RATE);
    if params.codec == CODEC_TYPE_OPUS {
        // Opus decodes straight to 16 kHz.
        rate = RATE;
        let mut decoder = opus_decoder::OpusDecoder::new(RATE, channels.min(2))
            .map_err(|e| format!("Opus: {e:?}"))?;
        // 120 ms, the longest an Opus packet can be.
        let mut pcm = vec![0f32; 1920 * channels.min(2)];
        while let Some(packet) = next_packet(&mut *format, track_id)? {
            let data = packet.buf();
            if data.starts_with(b"OpusHead") || data.starts_with(b"OpusTags") {
                continue;
            }
            // A damaged packet is skipped, like a crackle.
            if let Ok(n) = decoder.decode_float(data, &mut pcm, false) {
                mix(&pcm[..n * channels.min(2)], channels.min(2), &mut mono);
            }
        }
    } else {
        let mut decoder = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .map_err(|_| "This kind of sound isn't supported".to_string())?;
        let mut buffer: Option<SampleBuffer<f32>> = None;
        while let Some(packet) = next_packet(&mut *format, track_id)? {
            let decoded = match decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(Error::DecodeError(_)) => continue,
                Err(e) => return Err(e.to_string()),
            };
            let spec = *decoded.spec();
            rate = spec.rate;
            let buffer = match &mut buffer {
                Some(b) if b.capacity() >= decoded.capacity() * spec.channels.count() => b,
                _ => buffer.insert(SampleBuffer::new(decoded.capacity() as u64, spec)),
            };
            buffer.copy_interleaved_ref(decoded);
            mix(buffer.samples(), spec.channels.count().max(1), &mut mono);
        }
    }
    if mono.is_empty() {
        return Err("The file has no sound".into());
    }
    Ok(resample(&mono, rate, RATE))
}

/// The next packet of the track, None at the end of the file.
fn next_packet(
    format: &mut dyn symphonia::core::formats::FormatReader,
    track_id: u32,
) -> Result<Option<symphonia::core::formats::Packet>, String> {
    loop {
        match format.next_packet() {
            Ok(packet) if packet.track_id() == track_id => return Ok(Some(packet)),
            Ok(_) => {}
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(None);
            }
            // A file cut short still has what came before.
            Err(Error::ResetRequired | Error::DecodeError(_)) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Interleaved samples -> their average per moment.
fn mix(samples: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.extend(
        samples
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32),
    );
}

/// Changes the samples per second with a windowed sinc, which also filters out the sounds
/// too high for the new rate.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    const TAPS: i64 = 16;
    let step = from as f64 / to as f64;
    let cutoff = (to as f64 / from as f64).min(1.0) * 0.95;
    let len = (input.len() as f64 / step) as usize;
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let center = i as f64 * step;
        let base = center.floor() as i64;
        let (mut sum, mut weights) = (0.0, 0.0);
        for k in (base - TAPS + 1)..=(base + TAPS) {
            if k < 0 || k as usize >= input.len() {
                continue;
            }
            let x = k as f64 - center;
            let sinc = if x.abs() < 1e-9 {
                1.0
            } else {
                let a = std::f64::consts::PI * x * cutoff;
                a.sin() / a
            };
            // Hann window over the taps.
            let window = 0.5 + 0.5 * (std::f64::consts::PI * x / TAPS as f64).cos();
            let w = sinc * window;
            sum += input[k as usize] as f64 * w;
            weights += w;
        }
        out.push(if weights.abs() > 1e-9 { (sum / weights) as f32 } else { 0.0 });
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn resample_keeps_a_tone() {
        // A 440 Hz tone at 48 kHz stays a 440 Hz tone at 16 kHz, with the same strength.
        let tone: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin())
            .collect();
        let out = super::resample(&tone, 48_000, 16_000);
        assert_eq!(out.len(), 16_000);
        for (i, v) in out.iter().enumerate().skip(100).take(15_800) {
            let expected = (i as f32 / 16_000.0 * 440.0 * std::f32::consts::TAU).sin();
            assert!((v - expected).abs() < 0.02, "sample {i}: {v} vs {expected}");
        }
    }
}
