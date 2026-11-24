//! A systemd user service, `~/.config/systemd/user/assist.service`, enabled with
//! `systemctl --user enable --now`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use assist_core::private_fs;

use super::Launch;

const UNIT: &str = "assist.service";

/// Writes and enables the service; returns the unit file.
pub fn install(launch: &Launch) -> Result<PathBuf> {
    let dir = unit_dir()?;
    private_fs::create_dir(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let file = dir.join(UNIT);
    private_fs::write_atomic(&file, unit(launch).as_bytes())
        .with_context(|| format!("cannot write {}", file.display()))?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", UNIT])?;
    Ok(file)
}

/// Disables and deletes the service; `None` when there was none.
pub fn remove() -> Result<Option<PathBuf>> {
    let file = unit_dir()?.join(UNIT);
    if !file.is_file() {
        return Ok(None);
    }
    systemctl(&["disable", "--now", UNIT])?;
    std::fs::remove_file(&file).with_context(|| format!("cannot delete {}", file.display()))?;
    systemctl(&["daemon-reload"])?;
    Ok(Some(file))
}

fn unit_dir() -> Result<PathBuf> {
    let config = directories::BaseDirs::new()
        .map(|d| d.config_dir().to_path_buf())
        .context("no config folder")?;
    Ok(config.join("systemd/user"))
}

fn unit(launch: &Launch) -> String {
    let mut exec = quote(&launch.exe);
    for arg in launch.args() {
        exec.push(' ');
        exec.push_str(&quote(arg));
    }
    format!(
        "[Unit]\nDescription=assist daemon\n\n[Service]\nExecStart={exec} serve\n\
         Restart=on-failure\n\n[Install]\nWantedBy=default.target\n"
    )
}

/// A path as one systemd `ExecStart` word.
fn quote(path: &Path) -> String {
    let text = path.display().to_string();
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("cannot run systemctl")?;
    if !status.success() {
        bail!("systemctl --user {} failed", args.join(" "));
    }
    Ok(())
}
