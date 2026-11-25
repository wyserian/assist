//! The voice models and where they come from: files on the sherpa-onnx release page, checked
//! against a pinned sha256 before they are installed. Downloading is the caller's job (curl on
//! the desktop, the app on Android); this crate checks and unpacks.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::{VoiceError, KWS_MODEL, SPEAKER_MODEL, STT_MODEL, VAD_MODEL};

const RELEASES: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download";

/// A file from the sherpa-onnx release page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    /// What exists in the models directory once it is installed.
    pub installed: &'static str,
    /// Release tag on the sherpa-onnx release page.
    pub release: &'static str,
    /// File name on the release page.
    pub file: &'static str,
    /// Hex sha256 of that file.
    pub sha256: &'static str,
    /// Download size, rounded.
    pub size_mb: u32,
    /// Its licence, for the download prompt.
    pub licence: &'static str,
}

impl Model {
    /// Where to download it.
    pub fn url(&self) -> String {
        format!("{RELEASES}/{}/{}", self.release, self.file)
    }

    /// Checks the downloaded `file` and installs it in `dir`: an archive is unpacked, a plain
    /// file moved. The download is removed either way.
    pub fn install(&self, dir: &Path, file: &Path) -> Result<(), VoiceError> {
        let result = self.check_and_place(dir, file);
        let _ = std::fs::remove_file(file);
        result
    }

    fn check_and_place(&self, dir: &Path, file: &Path) -> Result<(), VoiceError> {
        let failed = |what: &str, err: std::io::Error| {
            VoiceError::File(format!("{} {what}: {err}", self.file))
        };
        let actual = sha256(file).map_err(|e| failed("cannot be read", e))?;
        if actual != self.sha256 {
            return Err(VoiceError::File(format!(
                "{} has sha256 {actual}, expected {}; not installed",
                self.file, self.sha256
            )));
        }
        if self.file.ends_with(".tar.bz2") {
            let archive = File::open(file).map_err(|e| failed("cannot be read", e))?;
            // `unpack` refuses entries that would land outside `dir`.
            tar::Archive::new(bzip2::read::BzDecoder::new(archive))
                .unpack(dir)
                .map_err(|e| failed("cannot be unpacked", e))
        } else {
            std::fs::copy(file, dir.join(self.installed))
                .map(drop)
                .map_err(|e| failed("cannot be installed", e))
        }
    }
}

/// Every voice model, in download order.
pub const MODELS: [Model; 4] = [
    Model {
        installed: VAD_MODEL,
        release: "asr-models",
        file: "silero_vad.onnx",
        sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
        size_mb: 1,
        licence: "MIT",
    },
    Model {
        installed: STT_MODEL,
        release: "asr-models",
        file: "sherpa-onnx-streaming-zipformer-en-kroko-2025-08-06.tar.bz2",
        sha256: "c8676e5ff9ac2a85296e53ee0fd4d5fb1db6770e7a7647166eeafe349ade6834",
        size_mb: 55,
        licence: "unclear, see https://huggingface.co/Banafo/Kroko-ASR (personal use)",
    },
    Model {
        installed: KWS_MODEL,
        release: "kws-models",
        file: "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01.tar.bz2",
        // The release page publishes no digest; this is the file as first downloaded.
        sha256: "f170013b4716e41b62b9bfd809687c207cef798ef9bc6534d524e17af9b6561a",
        size_mb: 18,
        licence: "Apache-2.0",
    },
    Model {
        installed: SPEAKER_MODEL,
        release: "speaker-recongition-models",
        file: SPEAKER_MODEL,
        // The release page publishes no digest; this is the file as first downloaded.
        sha256: "98daf9bcf1c8abf3db374fec4c27a5eda7c58705bb3f180d057b085a72a88bb5",
        size_mb: 30,
        licence: "Apache-2.0",
    },
];

/// The models named in `wanted` (by [`Model::installed`]) that `dir` does not have yet.
pub fn missing(dir: &Path, wanted: &[&str]) -> Vec<&'static Model> {
    MODELS
        .iter()
        .filter(|m| wanted.contains(&m.installed) && !dir.join(m.installed).exists())
        .collect()
}

/// Hex sha256 of a file, read in blocks.
fn sha256(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut block = vec![0; 64 * 1024];
    loop {
        let n = file.read(&mut block)?;
        if n == 0 {
            break;
        }
        hasher.update(&block[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("assist-models-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sha256_matches_a_known_value() {
        let dir = scratch("sha");
        let path = dir.join("abc");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_a_file_with_the_wrong_digest_and_removes_it() {
        let dir = scratch("digest");
        let download = dir.join("silero_vad.onnx.part");
        std::fs::write(&download, b"not the model").unwrap();
        let err = MODELS[0].install(&dir, &download).unwrap_err();
        assert!(err.to_string().contains("not installed"), "{err}");
        assert!(!download.exists() && !dir.join(VAD_MODEL).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lists_only_wanted_models_that_are_missing() {
        let dir = scratch("missing");
        std::fs::write(dir.join(VAD_MODEL), b"").unwrap();
        let names: Vec<_> = missing(&dir, &[VAD_MODEL, STT_MODEL])
            .iter()
            .map(|m| m.installed)
            .collect();
        assert_eq!(names, [STT_MODEL]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
