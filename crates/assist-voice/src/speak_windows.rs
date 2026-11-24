//! Spoken replies through the Windows voice (WinRT `SpeechSynthesizer`).

use windows::core::HSTRING;
use windows::Media::Core::MediaSource;
use windows::Media::Playback::{MediaPlaybackState, MediaPlayer};
use windows::Media::SpeechSynthesis::SpeechSynthesizer;

use crate::VoiceError;

/// The default Windows voice, playing on the default output device.
pub struct Speaker {
    synth: SpeechSynthesizer,
    player: MediaPlayer,
}

fn speech(e: windows::core::Error) -> VoiceError {
    VoiceError::Speech(e.message())
}

impl Speaker {
    /// Opens the default voice.
    pub fn new() -> Result<Self, VoiceError> {
        Ok(Self {
            synth: SpeechSynthesizer::new().map_err(speech)?,
            player: MediaPlayer::new().map_err(speech)?,
        })
    }

    /// Synthesizes `text` (blocks while it does) and starts playing it, replacing anything
    /// still playing.
    pub fn say(&mut self, text: &str) -> Result<(), VoiceError> {
        let stream = self
            .synth
            .SynthesizeTextToStreamAsync(&HSTRING::from(text))
            .and_then(|op| op.join())
            .map_err(speech)?;
        let kind = stream.ContentType().map_err(speech)?;
        let source = MediaSource::CreateFromStream(&stream, &kind).map_err(speech)?;
        self.player.SetSource(&source).map_err(speech)?;
        self.player.Play().map_err(speech)
    }

    /// Whether a reply is playing.
    pub fn is_speaking(&mut self) -> bool {
        self.player
            .PlaybackSession()
            .and_then(|s| s.PlaybackState())
            .is_ok_and(|state| {
                state == MediaPlaybackState::Playing || state == MediaPlaybackState::Opening
            })
    }

    /// Silences the current reply at once.
    pub fn stop(&mut self) -> Result<(), VoiceError> {
        self.player.Pause().map_err(speech)
    }
}
