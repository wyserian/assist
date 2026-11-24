//! Streaming speech to text: audio is decoded while it arrives, so little work is left when
//! the utterance ends.

use std::path::Path;

use sherpa_onnx as so;

use crate::{VoiceError, RATE};

/// Threads for decoding, the same as the daemon's worker count (PLAN.md section 8).
const THREADS: i32 = 2;

/// Silence fed after an utterance so the model emits its last word (2 s, Phase 2 spike).
/// Costs compute only; nothing waits for it.
static FLUSH: [f32; 32_000] = [0.0; 32_000];

/// A streaming transducer model, loaded once and kept warm.
pub struct Recognizer {
    inner: so::OnlineRecognizer,
}

impl Recognizer {
    /// Loads a model directory with `encoder.onnx`, `decoder.onnx`, `joiner.onnx` and
    /// `tokens.txt` (the layout of [`crate::STT_MODEL`]).
    pub fn new(dir: &Path) -> Result<Self, VoiceError> {
        let file = |name: &str| {
            let path = dir.join(name);
            if path.is_file() {
                Ok(Some(path.to_string_lossy().into_owned()))
            } else {
                Err(VoiceError::Model(path))
            }
        };
        let mut config = so::OnlineRecognizerConfig::default();
        config.model_config.transducer.encoder = file("encoder.onnx")?;
        config.model_config.transducer.decoder = file("decoder.onnx")?;
        config.model_config.transducer.joiner = file("joiner.onnx")?;
        config.model_config.tokens = file("tokens.txt")?;
        config.model_config.num_threads = THREADS;
        config.decoding_method = Some("greedy_search".into());
        let inner =
            so::OnlineRecognizer::create(&config).ok_or(VoiceError::Engine("speech recognizer"))?;
        Ok(Self { inner })
    }

    /// Starts transcribing one utterance.
    pub fn start(&self) -> Transcript<'_> {
        Transcript {
            recognizer: &self.inner,
            stream: self.inner.create_stream(),
        }
    }
}

/// One utterance being transcribed. Feed it [`crate::Event::Audio`] samples, then call
/// [`Transcript::finish`] on [`crate::Event::End`]. Runs on the caller's thread, never on the
/// async runtime.
pub struct Transcript<'a> {
    recognizer: &'a so::OnlineRecognizer,
    stream: so::OnlineStream,
}

impl Transcript<'_> {
    /// Decodes 16 kHz mono samples as far as they allow.
    pub fn feed(&mut self, samples: &[f32]) {
        self.stream.accept_waveform(RATE as i32, samples);
        self.decode();
    }

    /// Flushes the model and returns the text, trimmed; empty if nothing was understood.
    pub fn finish(self) -> String {
        self.stream.accept_waveform(RATE as i32, &FLUSH);
        self.stream.input_finished();
        self.decode();
        self.recognizer
            .get_result(&self.stream)
            .map(|r| r.text.trim().to_owned())
            .unwrap_or_default()
    }

    fn decode(&self) {
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
    }
}
