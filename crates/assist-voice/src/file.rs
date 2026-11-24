//! Speech in audio files: reading a WAV at 16 kHz, and transcribing it sentence by sentence.

use std::path::Path;

use sherpa_onnx as so;

use crate::{Event, Recognizer, Segmenter, Silero, VadSettings, VoiceError, RATE};

/// Longest file read: a 16 kHz sample is 4 bytes, so this bounds memory near 60 MB.
pub const MAX_FILE_SECONDS: u32 = 15 * 60;

/// A WAV file's samples as 16 kHz mono.
pub fn read_wav(path: &Path) -> Result<Vec<f32>, VoiceError> {
    let wave = so::Wave::read(&path.to_string_lossy()).ok_or_else(|| {
        VoiceError::File(format!("{} is not a readable WAV file", path.display()))
    })?;
    let rate = u32::try_from(wave.sample_rate()).unwrap_or(0);
    if rate == 0 {
        return Err(VoiceError::File(
            "WAV file without a sample rate".to_owned(),
        ));
    }
    let seconds = u64::try_from(wave.num_samples()).unwrap_or(0) / u64::from(rate);
    if seconds > u64::from(MAX_FILE_SECONDS) {
        return Err(VoiceError::File(format!(
            "{} is longer than {} minutes",
            path.display(),
            MAX_FILE_SECONDS / 60
        )));
    }
    if rate == RATE {
        return Ok(wave.samples().to_vec());
    }
    let resampler = so::LinearResampler::create(wave.sample_rate(), RATE as i32)
        .ok_or(VoiceError::Engine("resampler"))?;
    Ok(resampler.resample(wave.samples(), true))
}

/// The speech in 16 kHz `samples`, one line per sentence the VAD finds.
pub fn transcribe(
    samples: &[f32],
    vad_model: &Path,
    recognizer: &Recognizer,
) -> Result<Vec<String>, VoiceError> {
    let settings = VadSettings::default();
    let mut segmenter = Segmenter::new(Silero::new(vad_model, &settings)?, &settings);
    let mut transcript = None;
    let mut lines = Vec::new();
    // A second of silence after the file ends its last sentence.
    let tail = vec![0.0; RATE as usize];
    for chunk in samples.chunks(1_600).chain(tail.chunks(1_600)) {
        segmenter.push(chunk, |event| match event {
            Event::Start => transcript = Some(recognizer.start()),
            Event::Audio(audio) => {
                if let Some(t) = transcript.as_mut() {
                    t.feed(audio);
                }
            }
            Event::End => {
                let text = transcript.take().map(|t| t.finish()).unwrap_or_default();
                if !text.trim().is_empty() {
                    lines.push(text.trim().to_owned());
                }
            }
        });
    }
    Ok(lines)
}
