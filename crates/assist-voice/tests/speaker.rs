//! Speaker check on the models' own sample recordings: two halves of one recording match
//! better than two different speakers. Needs the models from the Phase 2 spike in
//! `spikes/voice/models`: cargo test -p assist-voice -- --ignored

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use assist_voice::{SpeakerModel, KWS_MODEL, RATE, SPEAKER_MODEL, STT_MODEL};
use sherpa_onnx as so;

fn models() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/voice/models")
}

fn read(path: &Path) -> Vec<f32> {
    let wave = so::Wave::read(&path.to_string_lossy()).unwrap();
    so::LinearResampler::create(wave.sample_rate(), RATE as i32)
        .unwrap()
        .resample(wave.samples(), true)
}

#[test]
#[ignore = "needs the speaker, Kroko and keyword models in spikes/voice/models"]
fn same_speaker_scores_above_another() {
    let model = SpeakerModel::new(&models().join(SPEAKER_MODEL)).unwrap();
    let one = read(&models().join(STT_MODEL).join("test_wavs/0.wav"));
    let other = read(&models().join(KWS_MODEL).join("test_wavs/0.wav"));
    let (first, second) = one.split_at(one.len() / 2);

    let first = model.voiceprint(first).unwrap();
    let same = first.score(&model.voiceprint(second).unwrap());
    let different = first.score(&model.voiceprint(&other).unwrap());
    assert!(
        same > 0.5 && same > different + 0.2,
        "same {same}, different {different}"
    );
}
