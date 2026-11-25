//! Voice models in `<data dir>/models`, downloaded on first use with the owner's consent.
//! The list, pinned digests and unpacking are shared with Android in `assist_voice::MODELS`.
//! Kroko is never bundled because its licence is unclear (PLAN.md section 12).

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use assist_core::private_fs::create_dir;
use assist_voice::{Model, MODELS};

/// The models directory with every model installed; asks before downloading missing ones.
pub async fn ensure(data: &Path) -> Result<PathBuf> {
    let dir = data.join("models");
    let wanted: Vec<&str> = MODELS.iter().map(|m| m.installed).collect();
    let missing = assist_voice::missing(&dir, &wanted);
    if missing.is_empty() {
        return Ok(dir);
    }
    println!("Voice needs these models in {}:", dir.display());
    for m in &missing {
        println!("  {} ({} MB, licence: {})", m.file, m.size_mb, m.licence);
    }
    println!("from the sherpa-onnx release page on GitHub");
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
    let url = model.url();
    // Connection resets on large release downloads are common: retry and resume the
    // partial file. The sha256 check in `install` still guards what ends up installed.
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
    Ok(model.install(dir, &download)?)
}
