//! The voice models: files on the sherpa-onnx release page, pinned by sha256. Installing is
//! `assist_core::models`.

use assist_core::models::Model;

use crate::{KWS_MODEL, SPEAKER_MODEL, STT_MODEL, VAD_MODEL};

/// A file on the sherpa-onnx release page.
macro_rules! release {
    ($path:literal) => {
        concat!(
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/",
            $path
        )
    };
}

/// Silero voice activity detection.
pub const VAD: Model = Model {
    installed: VAD_MODEL,
    url: release!("asr-models/silero_vad.onnx"),
    sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
    size_mb: 1,
    licence: "MIT",
};

/// Kroko streaming speech to text.
pub const STT: Model = Model {
    installed: STT_MODEL,
    url: release!("asr-models/sherpa-onnx-streaming-zipformer-en-kroko-2025-08-06.tar.bz2"),
    sha256: "c8676e5ff9ac2a85296e53ee0fd4d5fb1db6770e7a7647166eeafe349ade6834",
    size_mb: 55,
    licence: "unclear, see https://huggingface.co/Banafo/Kroko-ASR (personal use)",
};

/// Keyword spotting for the wake word.
pub const KWS: Model = Model {
    installed: KWS_MODEL,
    url: release!("kws-models/sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01.tar.bz2"),
    // The release page publishes no digest; this is the file as first downloaded.
    sha256: "f170013b4716e41b62b9bfd809687c207cef798ef9bc6534d524e17af9b6561a",
    size_mb: 18,
    licence: "Apache-2.0",
};

/// Speaker verification.
pub const SPEAKER: Model = Model {
    installed: SPEAKER_MODEL,
    url: release!("speaker-recongition-models/3dspeaker_speech_campplus_sv_en_voxceleb_16k.onnx"),
    // The release page publishes no digest; this is the file as first downloaded.
    sha256: "98daf9bcf1c8abf3db374fec4c27a5eda7c58705bb3f180d057b085a72a88bb5",
    size_mb: 30,
    licence: "Apache-2.0",
};

/// Every voice model, in download order.
pub const MODELS: [Model; 4] = [VAD, STT, KWS, SPEAKER];
