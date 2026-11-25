//! Hands-free listening, shared by `assist listen` and the Android app: speech counts only
//! after the wake word ("hey Bee") or within the awake time (`[voice] awake`) of the last
//! exchange, so follow-ups and answers to the assistant's questions need no wake word. While the assistant speaks only keywords are
//! heard (half duplex), and the wake word or "stop" cuts the reply short.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::{
    Event, Keyword, Recognizer, Segmenter, Silero, Spotter, Transcript, VadSettings, VoiceError,
    KWS_MODEL, RATE, STT_MODEL, VAD_MODEL,
};

/// What [`Attention::push`] reports.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Heard<'a> {
    /// The wake word or "stop" while the assistant speaks: stop speaking.
    Interrupt,
    /// The wake word alone; the request comes next.
    Woke,
    /// A finished utterance addressed to the assistant, with its audio when kept (for the
    /// speaker check), else empty.
    Text(&'a str, &'a [f32]),
}

/// Wake word, utterance detection and speech to text over one audio stream.
pub struct Attention {
    spotter: Spotter,
    segmenter: Segmenter<Silero>,
    recognizer: Recognizer,
    transcript: Option<Transcript>,
    /// The utterance's audio, kept only for the speaker check; bounded by the VAD's longest
    /// utterance.
    utterance: Vec<f32>,
    keep_audio: bool,
    awake_until: Option<Instant>,
    /// How long it stays awake after the wake word or a request.
    awake: Duration,
    /// The wake word was heard in the current utterance.
    woke: bool,
}

impl Attention {
    /// Loads the VAD, speech and keyword models from `models` and listens for `wake`
    /// phrases, staying awake for `awake` after each. `keep_audio` hands each utterance's
    /// audio out with its text.
    pub fn new(
        models: &Path,
        wake: &[String],
        awake: Duration,
        keep_audio: bool,
    ) -> Result<Self, VoiceError> {
        let settings = VadSettings::default();
        let vad = Silero::new(&models.join(VAD_MODEL), &settings)?;
        let capacity = if keep_audio { RATE as usize * 15 } else { 0 };
        Ok(Self {
            spotter: Spotter::new(&models.join(KWS_MODEL), wake)?,
            segmenter: Segmenter::new(vad, &settings),
            recognizer: Recognizer::new(&models.join(STT_MODEL))?,
            transcript: None,
            utterance: Vec::with_capacity(capacity),
            keep_audio,
            awake_until: None,
            awake,
            woke: false,
        })
    }

    /// Forgets the utterance in progress, e.g. while the microphone is paused.
    pub fn drop_utterance(&mut self) {
        self.transcript = None;
    }

    /// Feeds 16 kHz audio. `speaking` says the assistant is talking now.
    pub fn push(&mut self, chunk: &[f32], speaking: bool, mut on: impl FnMut(Heard<'_>)) {
        let keyword = self.spotter.push(chunk);
        let now = Instant::now();
        let awake = self.awake_until.is_some_and(|until| until > now);
        if speaking {
            if keyword.is_some() {
                on(Heard::Interrupt);
            }
            if awake || keyword == Some(Keyword::Wake) {
                self.awake_until = Some(now + self.awake);
            }
            self.transcript = None;
            return;
        }
        if keyword == Some(Keyword::Wake) {
            self.awake_until = Some(now + self.awake);
            self.woke = true;
            // Recognition restarts after the wake word, so the text is the request alone.
            // shortcut: audio between the wake word's end and its detection is dropped, a
            // few tens of ms; upgrade to the spotter's timestamps if first words go missing.
            if self.segmenter.is_speaking() {
                self.transcript = Some(self.recognizer.start());
                self.utterance.clear();
            }
        }
        let Self {
            segmenter,
            recognizer,
            transcript,
            utterance,
            keep_audio,
            awake_until,
            awake,
            woke,
            spotter: _,
        } = self;
        segmenter.push(chunk, |event| match event {
            Event::Start => {
                *transcript = Some(recognizer.start());
                utterance.clear();
            }
            Event::Audio(audio) => {
                if let Some(t) = transcript.as_mut() {
                    t.feed(audio);
                    if *keep_audio {
                        utterance.extend_from_slice(audio);
                    }
                }
            }
            Event::End => {
                let text = transcript
                    .take()
                    .map(Transcript::finish)
                    .unwrap_or_default();
                let now = Instant::now();
                if !text.is_empty() && awake_until.is_some_and(|until| until > now) {
                    *awake_until = Some(now + *awake);
                    on(Heard::Text(&text, utterance));
                } else if *woke {
                    on(Heard::Woke);
                }
                *woke = false;
            }
        });
    }
}
