//! Voice (PLAN.md Phase 2): microphone capture, voice activity detection, utterance
//! segmentation, streaming speech to text and spoken replies through the OS voice.
//!
//! Audio never reaches the async runtime: [`Mic`] reads on its own thread and hands out
//! 16 kHz mono chunks; [`Segmenter`] turns them into [`Event`]s on the caller's thread.

mod capture;
mod segment;
#[cfg(not(windows))]
#[path = "speak_unix.rs"]
mod speak;
#[cfg(windows)]
#[path = "speak_windows.rs"]
mod speak;
mod stt;
mod vad;

use std::path::PathBuf;

pub use capture::{input_devices, Mic};
pub use segment::{Event, Segmenter, PRE_ROLL};
pub use speak::Speaker;
pub use stt::{Recognizer, Transcript};
pub use vad::{Detector, Silero, VadSettings};

/// Sample rate of everything this crate hands out, Hz.
pub const RATE: u32 = 16_000;

/// The Silero VAD model file inside the models directory.
pub const VAD_MODEL: &str = "silero_vad.onnx";

/// The English speech model directory inside the models directory: Kroko streaming, the most
/// accurate in the Phase 2 spike. Its licence is unclear, so it is downloaded by the user,
/// never bundled (PLAN.md section 12).
pub const STT_MODEL: &str = "sherpa-onnx-streaming-zipformer-en-kroko-2025-08-06";

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
    /// A model file is missing.
    #[error("model file {} not found", .0.display())]
    Model(PathBuf),
    /// The OS voice failed or is missing.
    #[error("speech output: {0}")]
    Speech(String),
    /// The speech library could not create a component (bad model file, out of memory).
    #[error("could not create the {0}")]
    Engine(&'static str),
}
