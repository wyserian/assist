//! `assist install-autostart [--remove]`: starts the daemon at login with the same config
//! and data directory. Windows: a shortcut in the Startup folder that runs it through
//! `conhost --headless`, so no window appears. Linux: a systemd user service.
//!
//! shortcut: the daemon started this way has no console, so what it prints (trigger
//! messages) is not seen; add a log file when that output matters.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::setup::Paths;

#[cfg(windows)]
#[path = "autostart_windows.rs"]
mod os;
#[cfg(not(windows))]
#[path = "autostart_linux.rs"]
mod os;

/// Installs or removes the login entry.
pub fn run(paths: &Paths, remove: bool) -> Result<()> {
    if remove {
        return match os::remove()? {
            Some(entry) => {
                println!("Removed {}.", entry.display());
                Ok(())
            }
            None => {
                println!("No autostart entry to remove.");
                Ok(())
            }
        };
    }
    let exe = std::env::current_exe().context("cannot find the assist program")?;
    let entry = os::install(&Launch {
        exe,
        config: absolute(paths.config_file())?,
        data: absolute(paths.data())?,
    })?;
    println!(
        "The daemon starts at login: {}. Remove with: assist install-autostart --remove",
        entry.display()
    );
    Ok(())
}

/// What the login entry runs: `<exe> --config <config> --data-dir <data> serve`.
#[derive(Debug)]
pub struct Launch {
    exe: PathBuf,
    config: PathBuf,
    data: PathBuf,
}

impl Launch {
    /// The arguments after the program, each as one value.
    fn args(&self) -> [&Path; 4] {
        [
            Path::new("--config"),
            &self.config,
            Path::new("--data-dir"),
            &self.data,
        ]
    }
}

fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("cannot resolve {}", path.display()))
}
