//! A Startup-folder shortcut running `conhost.exe --headless "<assist.exe>" ... serve`. The
//! shortcut is written by PowerShell's shell COM object; every path reaches the script as
//! an environment variable, never as script text.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::Launch;

const SHORTCUT: &str = "assist.lnk";

/// Writes the shortcut; returns its path.
pub fn install(launch: &Launch) -> Result<PathBuf> {
    let lnk = startup_dir()?.join(SHORTCUT);
    write_shortcut(&lnk, launch)?;
    Ok(lnk)
}

/// Deletes the shortcut; `None` when there was none.
pub fn remove() -> Result<Option<PathBuf>> {
    let lnk = startup_dir()?.join(SHORTCUT);
    match std::fs::remove_file(&lnk) {
        Ok(()) => Ok(Some(lnk)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("cannot delete {}", lnk.display())),
    }
}

fn startup_dir() -> Result<PathBuf> {
    let roaming = directories::BaseDirs::new()
        .map(|d| d.config_dir().to_path_buf())
        .context("no AppData folder")?;
    Ok(roaming.join(r"Microsoft\Windows\Start Menu\Programs\Startup"))
}

/// `conhost --headless` runs the console program with no window.
pub fn write_shortcut(lnk: &Path, launch: &Launch) -> Result<()> {
    let windir =
        std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let mut arguments = format!("--headless {}", quote(&launch.exe));
    for arg in launch.args() {
        arguments.push(' ');
        arguments.push_str(&quote(arg));
    }
    arguments.push_str(" serve");
    let script = "$s = (New-Object -ComObject WScript.Shell).CreateShortcut($env:ASSIST_LNK); \
                  $s.TargetPath = $env:ASSIST_TARGET; $s.Arguments = $env:ASSIST_ARGS; \
                  $s.WorkingDirectory = $env:USERPROFILE; $s.WindowStyle = 7; $s.Save()";
    let status = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("ASSIST_LNK", lnk)
        .env("ASSIST_TARGET", windir.join(r"System32\conhost.exe"))
        .env("ASSIST_ARGS", &arguments)
        .status()
        .context("cannot run PowerShell to write the shortcut")?;
    if !status.success() || !lnk.is_file() {
        bail!("could not write {}", lnk.display());
    }
    Ok(())
}

/// A path as one Windows command-line argument.
fn quote(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens a shortcut like Explorer does at login and checks the daemon comes up headless.
    #[test]
    #[ignore = "starts a daemon from a shortcut; run on Windows with the binary built"]
    fn the_shortcut_starts_a_daemon() {
        let dir = std::env::temp_dir().join(format!("assist-autostart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[server]\nbind = \"127.0.0.1:7796\"\n",
        )
        .unwrap();
        let exe = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/assist.exe");
        let launch = Launch {
            exe: std::path::absolute(exe).unwrap(),
            config: dir.join("config.toml"),
            data: dir.join("data"),
        };
        let lnk = dir.join(SHORTCUT);
        write_shortcut(&lnk, &launch).unwrap();
        Command::new("explorer.exe").arg(&lnk).status().unwrap();
        let addr = dir.join("data").join("daemon.addr");
        let started = (0..40).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(250));
            addr.is_file()
        });
        // Stop the test daemon before asserting, so a failure leaves nothing running.
        let _ = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", "Get-CimInstance Win32_Process -Filter \"Name='assist.exe'\" | Where-Object { $_.CommandLine -like '*assist-autostart-*' } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force }"])
            .status();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(started, "no daemon.addr after 10 s");
    }
}
