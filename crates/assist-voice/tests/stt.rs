//! Transcribes the Kroko model's own sample recording. Needs the models from the Phase 2
//! spike in `spikes/voice/models`: cargo test -p assist-voice -- --ignored

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use assist_voice::{Recognizer, RATE, STT_MODEL};
use sherpa_onnx as so;

#[test]
#[ignore = "needs the Kroko model in spikes/voice/models"]
fn transcribes_the_model_sample() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../spikes/voice/models")
        .join(STT_MODEL);
    let wave = so::Wave::read(&dir.join("test_wavs/0.wav").to_string_lossy()).unwrap();
    let samples = so::LinearResampler::create(wave.sample_rate(), RATE as i32)
        .unwrap()
        .resample(wave.samples(), true);

    let recognizer = Recognizer::new(&dir).unwrap();
    let mut transcript = recognizer.start();
    // Pre-roll silence, then the audio in 100 ms chunks, as the segmenter delivers it.
    transcript.feed(&[0.0; 16_000]);
    for chunk in samples.chunks(1_600) {
        transcript.feed(chunk);
    }
    let text = transcript.finish().to_lowercase();
    assert!(
        text.contains("ask not what your country can do for you"),
        "{text}"
    );
}
