// Short sounds that go with some notifications. They're made here (a few sine-wave notes), so
// there are no audio files to ship, and played by whatever the system already has: paplay,
// pw-play or aplay on Linux, afplay on Mac, PlaySound on Windows.

use crate::config;
use std::path::PathBuf;

const RATE: u32 = 22_050;

#[derive(Clone, Copy)]
pub enum Sound {
    /// "Ding-dong": a reminder is due.
    Reminder,
    /// Three soft notes going down: time to rest.
    Rest,
    /// Four quick notes going up: back to work.
    Work,
}

impl Sound {
    fn name(self) -> &'static str {
        match self {
            Sound::Reminder => "reminder",
            Sound::Rest => "rest",
            Sound::Work => "work",
        }
    }

    /// (start in seconds, frequency in Hz, how long it rings in seconds), and the volume.
    fn notes(self) -> (&'static [(f32, f32, f32)], f32) {
        match self {
            Sound::Reminder => (&[(0.0, 659.3, 0.9), (0.38, 523.3, 1.1)], 0.5),
            Sound::Rest => (
                &[(0.0, 784.0, 1.0), (0.32, 659.3, 1.0), (0.64, 523.3, 1.6)],
                0.35,
            ),
            Sound::Work => (
                &[
                    (0.0, 523.3, 0.5),
                    (0.11, 659.3, 0.5),
                    (0.22, 784.0, 0.5),
                    (0.33, 1046.5, 0.9),
                ],
                0.45,
            ),
        }
    }
}

/// The settings key for a tool's sound ("sound-reminders"), on unless turned off.
pub fn key(tool: &str) -> String {
    format!("sound-{tool}")
}

/// Plays the sound if that tool's sound is on. Best effort, and it doesn't wait for it to end.
pub fn play_for(tool: &str, sound: Sound) {
    if config::enabled(&key(tool)) {
        play(sound);
    }
}

/// "Play a sound" on/off for a tool's page, with a button to hear each of its sounds.
pub fn setting(ui: &mut eframe::egui::Ui, tool: &str, sounds: &[(&str, Sound)]) {
    let key = key(tool);
    let mut on = config::enabled(&key);
    ui.horizontal(|ui| {
        if ui.checkbox(&mut on, "Play a sound").changed() {
            config::set_enabled(&key, on);
        }
        for &(label, sound) in sounds {
            if ui.small_button(label).clicked() {
                play(sound);
            }
        }
    });
}

pub fn play(sound: Sound) {
    let Some(file) = file(sound) else { return };
    #[cfg(all(unix, not(target_os = "macos")))]
    for player in ["paplay", "pw-play", "aplay"] {
        let mut command = crate::desktop::command(player);
        if player == "aplay" {
            command.arg("-q");
        }
        if let Ok(mut child) = command.arg(&file).spawn() {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            break;
        }
    }
    #[cfg(target_os = "macos")]
    {
        let mut command = crate::desktop::command("afplay");
        command.arg(&file);
        crate::desktop::start(command);
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Media::Audio::{
            PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT,
        };
        let wide: Vec<u16> = file.as_os_str().encode_wide().chain([0]).collect();
        unsafe {
            PlaySoundW(
                wide.as_ptr(),
                std::ptr::null_mut(),
                SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
            );
        }
    }
}

/// The sound as a .wav in the cache, written if missing or different.
fn file(sound: Sound) -> Option<PathBuf> {
    let file = config::cache_dir()
        .join("sounds")
        .join(format!("{}.wav", sound.name()));
    let wav = wav(sound);
    if std::fs::read(&file).ok().as_deref() != Some(wav.as_slice()) {
        std::fs::create_dir_all(file.parent()?).ok()?;
        std::fs::write(&file, &wav).ok()?;
    }
    Some(file)
}

fn wav(sound: Sound) -> Vec<u8> {
    let (notes, volume) = sound.notes();
    let length = notes
        .iter()
        .map(|&(at, _, ring)| at + ring)
        .fold(0.0, f32::max);
    let samples = (length * RATE as f32) as usize;
    let mut out = vec![0f32; samples];
    for &(at, freq, ring) in notes {
        let first = (at * RATE as f32) as usize;
        let count = ((ring * RATE as f32) as usize).min(samples - first);
        for i in 0..count {
            let t = i as f32 / RATE as f32;
            // A quick start so it doesn't click, then it fades like a bell.
            let envelope = (t / 0.008).min(1.0) * (-5.0 * t / ring).exp();
            let phase = std::f32::consts::TAU * freq * t;
            out[first + i] += envelope * (phase.sin() + 0.25 * (2.0 * phase).sin());
        }
    }
    let peak = out.iter().fold(0f32, |m, s| m.max(s.abs())).max(1e-6);

    let data_len = (samples * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + samples * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for s in out {
        let s = (s / peak * volume * i16::MAX as f32) as i16;
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_matches_length() {
        for sound in [Sound::Reminder, Sound::Rest, Sound::Work] {
            let wav = wav(sound);
            assert_eq!(&wav[..4], b"RIFF");
            let data = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
            assert_eq!(wav.len(), 44 + data);
        }
    }

    /// To hear them: `cargo test --release play_all -- --ignored`
    #[test]
    #[ignore]
    fn play_all() {
        for sound in [Sound::Reminder, Sound::Rest, Sound::Work] {
            play(sound);
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    }
}
