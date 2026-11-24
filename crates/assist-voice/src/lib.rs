//! Voice input (PLAN.md Phase 2): microphone capture, voice activity detection and utterance
//! segmentation. Speech recognition, spoken replies and the wake word build on these.
//!
//! Audio never reaches the async runtime: [`Mic`] reads on its own thread and hands out
//! 16 kHz mono chunks; [`Segmenter`] turns them into [`Event`]s on the caller's thread.

mod capture;
mod segment;
mod vad;

use std::path::PathBuf;

pub use capture::{input_devices, Mic};
pub use segment::{Event, Segmenter, PRE_ROLL};
pub use vad::{Detector, Silero, VadSettings};

/// Sample rate of everything this crate hands out, Hz.
pub const RATE: u32 = 16_000;

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
    /// The speech library could not create a component (bad model file, out of memory).
    #[error("could not create the {0}")]
    Engine(&'static str),
}
