//! Voice for the app (PLAN.md Phase 4 step 3): the app records the microphone and pushes
//! 16 kHz audio. [`Listener`] is push-to-talk: it finds one utterance (Silero VAD) and
//! transcribes it (the desktop's streaming model). [`HandsFree`] adds the wake word through
//! `assist_voice::Attention`, as `assist listen` does. Models are downloaded by the app and
//! checked and unpacked here against the pinned digests in `assist_voice::MODELS`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use assist_core::private_fs::create_dir;
use assist_voice::{
    Attention, Event, Heard, Recognizer, Segmenter, Silero, Transcript, VadSettings, KWS_MODEL,
    MODELS, STT_MODEL, VAD_MODEL,
};

use crate::AssistError;

/// The models push-to-talk needs, then the wake word's.
const PUSH_TO_TALK: [&str; 2] = [VAD_MODEL, STT_MODEL];
const HANDS_FREE: [&str; 3] = [VAD_MODEL, STT_MODEL, KWS_MODEL];

/// A model the app still has to download.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ModelFile {
    /// Its name, for [`install_model`].
    pub name: String,
    /// Where to download it.
    pub url: String,
    /// Download size, rounded.
    pub size_mb: u32,
    /// Its licence, for the download prompt.
    pub licence: String,
}

fn models_dir(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("models")
}

/// The voice models missing from `data_dir` for push-to-talk, or with `hands_free` also for
/// the wake word.
#[uniffi::export]
pub fn missing_models(data_dir: String, hands_free: bool) -> Vec<ModelFile> {
    let wanted: &[&str] = if hands_free {
        &HANDS_FREE
    } else {
        &PUSH_TO_TALK
    };
    assist_voice::missing(&models_dir(&data_dir), wanted)
        .into_iter()
        .map(|m| ModelFile {
            name: m.installed.to_owned(),
            url: m.url(),
            size_mb: m.size_mb,
            licence: m.licence.to_owned(),
        })
        .collect()
}

/// Checks the downloaded file `download` against the pinned digest of model `name` and
/// installs it in `data_dir`. The download is removed either way.
#[uniffi::export]
pub fn install_model(data_dir: String, name: String, download: String) -> Result<(), AssistError> {
    let model = MODELS
        .iter()
        .find(|m| m.installed == name && HANDS_FREE.contains(&m.installed))
        .ok_or_else(|| AssistError::Voice(format!("{name:?} is not a voice model")))?;
    let dir = models_dir(&data_dir);
    create_dir(&dir).map_err(|err| AssistError::Voice(format!("{}: {err}", dir.display())))?;
    model
        .install(&dir, Path::new(&download))
        .map_err(|err| AssistError::Voice(err.to_string()))
}

/// Turns pushed audio into the text of one utterance at a time.
#[derive(uniffi::Object)]
pub struct Listener {
    inner: Mutex<Inner>,
}

struct Inner {
    models: PathBuf,
    segmenter: Segmenter<Silero>,
    recognizer: Recognizer,
    transcript: Option<Transcript>,
}

#[uniffi::export]
impl Listener {
    /// Loads the models from `data_dir`; slow (about a second), so not on the UI thread.
    #[uniffi::constructor]
    pub fn new(data_dir: String) -> Result<Arc<Self>, AssistError> {
        let dir = models_dir(&data_dir);
        let failed = |err: assist_voice::VoiceError| AssistError::Voice(err.to_string());
        let recognizer = Recognizer::new(&dir.join(STT_MODEL)).map_err(failed)?;
        Ok(Arc::new(Self {
            inner: Mutex::new(Inner {
                segmenter: segmenter(&dir).map_err(failed)?,
                models: dir,
                recognizer,
                transcript: None,
            }),
        }))
    }

    /// Forgets any utterance in progress, for the next tap on the mic.
    pub fn reset(&self) -> Result<(), AssistError> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.segmenter =
            segmenter(&inner.models).map_err(|err| AssistError::Voice(err.to_string()))?;
        inner.transcript = None;
        Ok(())
    }

    /// Feeds 16 kHz mono samples. Returns the text when an utterance ends, empty if nothing
    /// was understood; `None` while it goes on or nobody speaks yet.
    pub fn push(&self, samples: Vec<f32>) -> Option<String> {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let Inner {
            segmenter,
            models: _,
            recognizer,
            transcript,
        } = &mut *guard;
        let mut heard = None;
        segmenter.push(&samples, |event| match event {
            Event::Start => *transcript = Some(recognizer.start()),
            Event::Audio(audio) => {
                if let Some(t) = transcript.as_mut() {
                    t.feed(audio);
                }
            }
            Event::End => {
                let text = transcript
                    .take()
                    .map(Transcript::finish)
                    .unwrap_or_default();
                heard.get_or_insert(text);
            }
        });
        heard
    }

    /// Whether someone is speaking now, for the app to show it.
    pub fn is_speaking(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .segmenter
            .is_speaking()
    }
}

/// What [`HandsFree::push`] heard.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum Hearing {
    /// The wake word or "stop" while the app speaks: stop speaking.
    Interrupt,
    /// The wake word alone; answer with `Assistant::woke_reply`.
    Woke,
    /// A request after the wake word, for `Assistant::hear`.
    Text {
        /// What was said.
        text: String,
    },
}

/// Listens all the time for the wake word, then for requests while awake.
#[derive(uniffi::Object)]
pub struct HandsFree {
    attention: Mutex<Attention>,
}

#[uniffi::export]
impl HandsFree {
    /// Loads the models from `data_dir` and listens for `wake` (see
    /// `Assistant::wake_phrases`); slow, so not on the UI thread.
    #[uniffi::constructor]
    pub fn new(data_dir: String, wake: Vec<String>) -> Result<Arc<Self>, AssistError> {
        let attention = Attention::new(&models_dir(&data_dir), &wake, false)
            .map_err(|err| AssistError::Voice(err.to_string()))?;
        Ok(Arc::new(Self {
            attention: Mutex::new(attention),
        }))
    }

    /// Feeds 16 kHz mono samples; `speaking` says the app is speaking a reply now.
    pub fn push(&self, samples: Vec<f32>, speaking: bool) -> Vec<Hearing> {
        let mut heard = Vec::new();
        self.attention
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(&samples, speaking, |event| {
                heard.push(match event {
                    Heard::Interrupt => Hearing::Interrupt,
                    Heard::Woke => Hearing::Woke,
                    Heard::Text(text, _) => Hearing::Text {
                        text: text.to_owned(),
                    },
                });
            });
        heard
    }
}

/// A fresh utterance detector; loading the small VAD model takes milliseconds.
fn segmenter(models: &Path) -> Result<Segmenter<Silero>, assist_voice::VoiceError> {
    let settings = VadSettings::default();
    let vad = Silero::new(&models.join(VAD_MODEL), &settings)?;
    Ok(Segmenter::new(vad, &settings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("assist-ffi-voice-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn lists_the_push_to_talk_models_and_refuses_others() {
        let dir = scratch("models");
        let data = dir.display().to_string();
        let hands_free: Vec<_> = missing_models(data.clone(), true)
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert_eq!(hands_free, [VAD_MODEL, STT_MODEL, KWS_MODEL]);
        let names: Vec<_> = missing_models(data.clone(), false)
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert_eq!(names, [VAD_MODEL, STT_MODEL]);
        let err = install_model(data, "../evil".to_owned(), "x".to_owned()).unwrap_err();
        assert!(err.to_string().contains("not a voice model"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "needs the models from spikes/voice/models"]
    fn hears_the_model_sample() {
        use assist_voice::RATE;
        use sherpa_onnx as so;
        let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/voice");
        let listener = Listener::new(models.display().to_string()).unwrap();
        let wave = so::Wave::read(
            &models
                .join("models")
                .join(STT_MODEL)
                .join("test_wavs/0.wav")
                .to_string_lossy(),
        )
        .unwrap();
        let samples = so::LinearResampler::create(wave.sample_rate(), RATE as i32)
            .unwrap()
            .resample(wave.samples(), true);
        let silence = vec![0.0; RATE as usize];
        let heard = samples
            .chunks(1_600)
            .chain(silence.chunks(1_600))
            .find_map(|chunk| listener.push(chunk.to_vec()))
            .unwrap();
        assert!(!heard.is_empty());
    }
}
