//! Models the app downloads with the owner's consent: speech, the wake word and OCR. The app
//! fetches the file; the core checks it against its pinned sha256 and installs it
//! (`assist_core::models`).

use std::path::{Path, PathBuf};

use assist_core::models::{self, Model};
use assist_core::private_fs::create_dir;
use assist_voice::models::{KWS, STT, VAD};

use crate::AssistError;

/// What the models are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Feature {
    /// Push-to-talk.
    Talk,
    /// The wake word.
    HandsFree,
    /// Reading text on the screen.
    Ocr,
}

const TALK: [Model; 2] = [VAD, STT];
const HANDS_FREE: [Model; 3] = [VAD, STT, KWS];
const ALL: [&[Model]; 3] = [&HANDS_FREE, &assist_actions::ocr::MODELS, &TALK];

impl Feature {
    fn models(self) -> &'static [Model] {
        match self {
            Self::Talk => &TALK,
            Self::HandsFree => &HANDS_FREE,
            Self::Ocr => &assist_actions::ocr::MODELS,
        }
    }
}

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

/// `<data dir>/models`.
pub(crate) fn models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models")
}

/// Whether `feature` has every model it needs in `dir`.
pub(crate) fn have(dir: &Path, feature: Feature) -> bool {
    models::missing(dir, feature.models()).is_empty()
}

/// The models `feature` still needs in `data_dir`.
#[uniffi::export]
pub fn missing_models(data_dir: String, feature: Feature) -> Vec<ModelFile> {
    models::missing(&models_dir(Path::new(&data_dir)), feature.models())
        .into_iter()
        .map(|m| ModelFile {
            name: m.installed.to_owned(),
            url: m.url.to_owned(),
            size_mb: m.size_mb,
            licence: m.licence.to_owned(),
        })
        .collect()
}

/// Checks the downloaded file `download` against the pinned digest of model `name` and
/// installs it in `data_dir`. The download is removed either way.
#[uniffi::export]
pub fn install_model(data_dir: String, name: String, download: String) -> Result<(), AssistError> {
    let model = ALL
        .iter()
        .flat_map(|list| list.iter())
        .find(|m| m.installed == name)
        .ok_or_else(|| AssistError::Voice(format!("{name:?} is not a model")))?;
    let dir = models_dir(Path::new(&data_dir));
    create_dir(&dir).map_err(|err| AssistError::Voice(format!("{}: {err}", dir.display())))?;
    model
        .install(&dir, Path::new(&download))
        .map_err(|err| AssistError::Voice(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use assist_voice::{KWS_MODEL, STT_MODEL, VAD_MODEL};

    #[test]
    fn lists_models_per_feature_and_refuses_others() {
        let dir = std::env::temp_dir().join(format!("assist-ffi-models-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = dir.display().to_string();
        let names = |feature| -> Vec<String> {
            missing_models(data.clone(), feature)
                .into_iter()
                .map(|m| m.name)
                .collect()
        };
        assert_eq!(names(Feature::Talk), [VAD_MODEL, STT_MODEL]);
        assert_eq!(names(Feature::HandsFree), [VAD_MODEL, STT_MODEL, KWS_MODEL]);
        assert_eq!(names(Feature::Ocr).len(), 2);
        let err = install_model(data, "../evil".to_owned(), "x".to_owned()).unwrap_err();
        assert!(err.to_string().contains("not a model"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
