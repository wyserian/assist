//! Voice (PLAN.md Phase 2): microphone capture, voice activity detection, utterance
//! segmentation, wake word, streaming speech to text, speaker verification and spoken replies
//! through the OS voice.
//!
//! Audio never reaches the async runtime: [`Mic`] reads on its own thread and hands out
//! 16 kHz mono chunks; [`Segmenter`] turns them into [`Event`]s on the caller's thread.

#[cfg(not(target_os = "android"))]
mod capture;
mod file;
mod models;
mod segment;
#[cfg(all(unix, not(target_os = "android")))]
#[path = "speak_unix.rs"]
mod speak;
#[cfg(windows)]
#[path = "speak_windows.rs"]
mod speak;
mod speaker;
mod stt;
mod vad;
mod wake;

use std::path::PathBuf;

#[cfg(not(target_os = "android"))]
pub use capture::{input_devices, Mic};
pub use file::{read_wav, transcribe, MAX_FILE_SECONDS};
pub use models::{missing, Model, MODELS};
pub use segment::{Event, Segmenter, PRE_ROLL};
#[cfg(not(target_os = "android"))]
pub use speak::Speaker;
pub use speaker::{SpeakerModel, Voiceprint};
pub use stt::{Recognizer, Transcript};
pub use vad::{Detector, Silero, VadSettings};
pub use wake::{Keyword, Spotter};

/// Sample rate of everything this crate hands out, Hz.
pub const RATE: u32 = 16_000;

/// The Silero VAD model file inside the models directory.
pub const VAD_MODEL: &str = "silero_vad.onnx";

/// The English speech model directory inside the models directory: Kroko streaming, the most
/// accurate in the Phase 2 spike. Its licence is unclear, so it is downloaded by the user,
/// never bundled (PLAN.md section 12).
pub const STT_MODEL: &str = "sherpa-onnx-streaming-zipformer-en-kroko-2025-08-06";

/// The speaker verification model file inside the models directory: 3D-Speaker CAM++
/// (Apache-2.0), the clearer of two in the Phase 2 spike.
pub const SPEAKER_MODEL: &str = "3dspeaker_speech_campplus_sv_en_voxceleb_16k.onnx";

/// The keyword spotting model directory inside the models directory (Apache-2.0).
pub const KWS_MODEL: &str = "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01";

/// Why voice input could not start or stopped.
#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    /// The system has no input device.
    #[error("no microphone found")]
    NoDevice,
    /// No input device name contains the configured text.
    #[error("no microphone named like {0:?}")]
    DeviceNotFound(String),
    /// The audio system reported an error, e.g. the device was unplugged.
    #[error("microphone: {0}")]
    Device(String),
    /// The device delivers samples in a format this crate does not convert.
    #[error("microphone sample format {0} is not supported")]
    Format(String),
    /// An audio file cannot be used: not a WAV, too long, no sample rate.
    #[error("{0}")]
    File(String),
    /// A model file is missing.
    #[error("model file {} not found", .0.display())]
    Model(PathBuf),
    /// The OS voice failed or is missing.
    #[error("speech output: {0}")]
    Speech(String),
    /// None of the wake phrases can be spelled with the keyword model's pieces.
    #[error("cannot listen for the wake word {0:?}; use plain letters")]
    Wake(String),
    /// The speech library could not create a component (bad model file, out of memory).
    #[error("could not create the {0}")]
    Engine(&'static str),
}
