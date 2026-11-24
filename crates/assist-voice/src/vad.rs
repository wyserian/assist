//! Voice activity detection: is this window of audio speech?

use std::path::Path;
use std::time::Duration;

use sherpa_onnx as so;

use crate::{VoiceError, RATE};

/// Decides per window of [`crate::segment::WINDOW`] samples (16 kHz) whether speech is going
/// on. Behind a trait so segmentation is tested without a model.
pub trait Detector {
    /// Takes the next window; true while speech is going on. Implementations may lag: speech
    /// is reported after `min_speech` and its end after `min_silence`.
    fn is_speech(&mut self, window: &[f32]) -> bool;
    /// Forgets all state, e.g. after a forced cut.
    fn reset(&mut self);
}

/// Tuning for [`Silero`] and [`crate::Segmenter`].
#[derive(Debug, Clone, PartialEq)]
pub struct VadSettings {
    /// Speech probability above which a window counts as speech, 0 to 1.
    pub threshold: f32,
    /// Silence after speech before the utterance ends; every reply waits this long.
    pub min_silence: Duration,
    /// Speech shorter than this is ignored (clicks, coughs).
    pub min_speech: Duration,
    /// Utterances are cut at this length.
    pub max_utterance: Duration,
}

impl Default for VadSettings {
    /// Values measured in the Phase 2 spike (PLAN.md section 13).
    fn default() -> Self {
        Self {
            threshold: 0.5,
            min_silence: Duration::from_millis(300),
            min_speech: Duration::from_millis(250),
            max_utterance: Duration::from_secs(15),
        }
    }
}

/// Silero VAD through sherpa-onnx.
pub struct Silero {
    vad: so::VoiceActivityDetector,
}

impl Silero {
    /// Loads the Silero model (`silero_vad.onnx`).
    pub fn new(model: &Path, settings: &VadSettings) -> Result<Self, VoiceError> {
        if !model.is_file() {
            return Err(VoiceError::Model(model.to_path_buf()));
        }
        let mut config = so::VadModelConfig::default();
        config.silero_vad.model = Some(model.to_string_lossy().into_owned());
        config.silero_vad.threshold = settings.threshold;
        config.silero_vad.min_silence_duration = settings.min_silence.as_secs_f32();
        config.silero_vad.min_speech_duration = settings.min_speech.as_secs_f32();
        config.silero_vad.max_speech_duration = settings.max_utterance.as_secs_f32();
        config.silero_vad.window_size = crate::segment::WINDOW as i32;
        config.sample_rate = RATE as i32;
        config.num_threads = 1;
        // The detector's own segment buffer; segments are discarded as they appear.
        let buffer_s = settings.max_utterance.as_secs_f32() + 1.0;
        let vad = so::VoiceActivityDetector::create(&config, buffer_s)
            .ok_or(VoiceError::Engine("voice activity detector"))?;
        Ok(Self { vad })
    }
}

impl Detector for Silero {
    fn is_speech(&mut self, window: &[f32]) -> bool {
        self.vad.accept_waveform(window);
        // Segmenter keeps its own audio; dropping finished segments bounds memory here.
        while !self.vad.is_empty() {
            self.vad.pop();
        }
        self.vad.detected()
    }

    fn reset(&mut self) {
        self.vad.reset();
    }
}
