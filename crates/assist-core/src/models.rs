//! Downloadable model files (speech, keywords, OCR) and how they are installed in
//! `<data dir>/models`: checked against a pinned sha256, then unpacked or moved into place.
//! Downloading is the caller's job (curl on the desktop, the app on Android); the lists live
//! with the code that uses them (`assist_voice::MODELS`, OCR in assist-actions).

use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

/// A model file to download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    /// What exists in the models directory once it is installed.
    pub installed: &'static str,
    /// Where to download it. A `.tar.bz2` is unpacked into the models directory.
    pub url: &'static str,
    /// Hex sha256 of the download.
    pub sha256: &'static str,
    /// Download size, rounded.
    pub size_mb: u32,
    /// Its licence, for the download prompt.
    pub licence: &'static str,
}

/// Why a download was not installed.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ModelError(String);

impl Model {
    /// The downloaded file's name, the last part of [`Model::url`].
    pub fn file(&self) -> &'static str {
        self.url.rsplit('/').next().unwrap_or(self.url)
    }

    /// Checks the downloaded `file` and installs it in `dir`: an archive is unpacked, a plain
    /// file copied. The download is removed either way.
    pub fn install(&self, dir: &Path, file: &Path) -> Result<(), ModelError> {
        let result = self.check_and_place(dir, file);
        let _ = std::fs::remove_file(file);
        result
    }

    fn check_and_place(&self, dir: &Path, file: &Path) -> Result<(), ModelError> {
        let name = self.file();
        let failed = |what: &str, err: std::io::Error| ModelError(format!("{name} {what}: {err}"));
        let actual = sha256(file).map_err(|e| failed("cannot be read", e))?;
        if actual != self.sha256 {
            return Err(ModelError(format!(
                "{name} has sha256 {actual}, expected {}; not installed",
                self.sha256
            )));
        }
        if name.ends_with(".tar.bz2") {
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

/// The models in `list` that `dir` does not have yet.
pub fn missing<'a>(dir: &Path, list: &'a [Model]) -> Vec<&'a Model> {
    list.iter()
        .filter(|m| !dir.join(m.installed).exists())
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

    const ONE: Model = Model {
        installed: "one.onnx",
        url: "https://example.org/models/one.onnx",
        sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        size_mb: 1,
        licence: "MIT",
    };

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("assist-models-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn installs_a_file_with_the_pinned_digest() {
        let dir = scratch("good");
        let download = dir.join("one.part");
        std::fs::write(&download, b"abc").unwrap();
        assert_eq!(ONE.file(), "one.onnx");
        assert_eq!(missing(&dir, &[ONE]).len(), 1);
        ONE.install(&dir, &download).unwrap();
        assert!(!download.exists() && missing(&dir, &[ONE]).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_a_file_with_the_wrong_digest_and_removes_it() {
        let dir = scratch("bad");
        let download = dir.join("one.part");
        std::fs::write(&download, b"not the model").unwrap();
        let err = ONE.install(&dir, &download).unwrap_err();
        assert!(err.to_string().contains("not installed"), "{err}");
        assert!(!download.exists() && !dir.join(ONE.installed).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
