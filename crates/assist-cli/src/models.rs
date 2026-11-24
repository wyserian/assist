//! Voice models in `<data dir>/models`, downloaded on first use with the owner's consent.
//! Kroko is never bundled because its licence is unclear (PLAN.md section 12).

use std::fs::File;
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use assist_core::private_fs::create_dir;
use assist_voice::{KWS_MODEL, SPEAKER_MODEL, STT_MODEL, VAD_MODEL};
use sha2::{Digest, Sha256};

const RELEASES: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download";

/// A file from the sherpa-onnx release page, checked against its published sha256.
struct Model {
    /// What exists in the models directory once it is installed.
    installed: &'static str,
    /// Release tag on the sherpa-onnx release page.
    release: &'static str,
    /// File name on the release page.
    file: &'static str,
    sha256: &'static str,
    size_mb: u32,
    licence: &'static str,
}

const MODELS: [Model; 4] = [
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

/// The models directory with every model installed; asks before downloading missing ones.
pub async fn ensure(data: &Path) -> Result<PathBuf> {
    let dir = data.join("models");
    let missing: Vec<&Model> = MODELS
        .iter()
        .filter(|m| !dir.join(m.installed).exists())
        .collect();
    if missing.is_empty() {
        return Ok(dir);
    }
    println!("Voice needs these models in {}:", dir.display());
    for m in &missing {
        println!("  {} ({} MB, licence: {})", m.file, m.size_mb, m.licence);
    }
    println!("from {RELEASES}");
    if !io::stdin().is_terminal() {
        bail!("run assist listen in a terminal once to approve the download");
    }
    let answer = crate::commands::read_line("Download them now? [y/N] ").await?;
    if !answer.trim().eq_ignore_ascii_case("y") {
        bail!("voice models not downloaded");
    }
    create_dir(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    for m in missing {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || install(&dir, m)).await??;
        println!("  installed {}", m.installed);
    }
    Ok(dir)
}

fn install(dir: &Path, model: &Model) -> Result<()> {
    let download = dir.join(format!("{}.part", model.file));
    let url = format!("{RELEASES}/{}/{}", model.release, model.file);
    // Connection resets on large release downloads are common: retry and resume the
    // partial file. The sha256 check below still guards what ends up installed.
    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--progress-bar",
            "--proto",
            "=https",
            "--retry",
            "5",
            "--retry-all-errors",
            "--continue-at",
            "-",
        ])
        .arg("--output")
        .arg(&download)
        .arg(&url)
        .status()
        .context("cannot run curl")?;
    if !status.success() {
        let _ = std::fs::remove_file(&download);
        bail!("download of {url} failed");
    }
    let actual = sha256(&download)?;
    if actual != model.sha256 {
        let _ = std::fs::remove_file(&download);
        bail!(
            "{} has sha256 {actual}, expected {}; not installed",
            model.file,
            model.sha256
        );
    }
    if model.file.ends_with(".tar.bz2") {
        let status = Command::new("tar")
            .arg("-xjf")
            .arg(&download)
            .arg("-C")
            .arg(dir)
            .status()
            .context("cannot run tar")?;
        let _ = std::fs::remove_file(&download);
        if !status.success() {
            bail!("unpacking {} failed", model.file);
        }
    } else {
        std::fs::rename(&download, dir.join(model.installed))?;
    }
    Ok(())
}

/// Hex sha256 of a file, read in blocks.
fn sha256(path: &Path) -> Result<String> {
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

    #[test]
    fn sha256_matches_a_known_value() {
        let path = std::env::temp_dir().join(format!("assist-sha-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        let hash = sha256(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
