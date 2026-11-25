//! Windows: the shell's "open", which also finds apps registered under App Paths
//! (e.g. `firefox.exe` that is not on PATH) and `shell:AppsFolder\<id>` for found apps.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

use serde::Deserialize;

pub(super) fn launch(program: &str) -> std::io::Result<()> {
    open::that_detached(program)
}

/// No console window for the helper, also when the daemon runs hidden.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Most output read from the helper; a Start menu list is far smaller.
const MAX_OUTPUT: usize = 4 << 20;

/// The Start menu's apps, desktop and Store alike, as Windows lists them (`Get-StartApps`):
/// name and `shell:AppsFolder\<id>`, which opens them. Empty if PowerShell fails. Blocking.
pub(super) fn installed() -> Vec<(String, String)> {
    let script = "[Console]::OutputEncoding = [Text.Encoding]::UTF8; \
                  ConvertTo-Json -Compress -InputObject @(Get-StartApps | Select-Object Name, AppID)";
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match output {
        Ok(out) if out.status.success() && out.stdout.len() <= MAX_OUTPUT => {
            parse(&String::from_utf8_lossy(&out.stdout))
        }
        _ => Vec::new(),
    }
}

#[derive(Deserialize)]
struct StartApp {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "AppID")]
    id: String,
}

/// The apps worth opening by name from `Get-StartApps` JSON.
fn parse(json: &str) -> Vec<(String, String)> {
    let apps: Vec<StartApp> = serde_json::from_str(json.trim()).unwrap_or_default();
    apps.into_iter()
        .filter(|app| wanted(&app.name, &app.id))
        .map(|app| (app.name, format!("shell:AppsFolder\\{}", app.id)))
        .collect()
}

/// Leaves out uninstallers, help and readme files and web links the Start menu also lists.
fn wanted(name: &str, id: &str) -> bool {
    const NOISE: [&str; 8] = [
        "uninstall",
        "readme",
        "read me",
        "help",
        "documentation",
        "release notes",
        "license",
        "website",
    ];
    let name = name.to_lowercase();
    !id.starts_with("http") && !NOISE.iter().any(|noise| name.contains(noise))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_apps_and_drops_uninstallers_and_links() {
        let json = r#"[{"Name":"Calculator","AppID":"Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"},
            {"Name":"Uninstall Foo","AppID":"{6D80}\\Foo\\uninstall.exe"},
            {"Name":"Foo website","AppID":"https://foo.example"},
            {"Name":"Google Chrome","AppID":"Chrome"}]"#;
        assert_eq!(
            parse(json),
            [
                (
                    "Calculator".to_owned(),
                    "shell:AppsFolder\\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".to_owned()
                ),
                (
                    "Google Chrome".to_owned(),
                    "shell:AppsFolder\\Chrome".to_owned()
                ),
            ]
        );
        assert!(parse("not json").is_empty());
    }
}
