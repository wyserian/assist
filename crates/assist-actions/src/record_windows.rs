//! ffmpeg inputs on Windows: `gdigrab` for the desktop, DirectShow for the microphone.

use crate::record_spec::Kind;

/// The input arguments for `kind`. DirectShow needs the microphone's exact name, so it asks
/// ffmpeg for its devices.
///
/// shortcut: takes the first microphone ffmpeg lists, not the headset `listen` prefers;
/// share the headset choice when recording uses `[voice]` settings.
pub async fn input(kind: Kind) -> Result<Vec<String>, String> {
    match kind {
        Kind::Screen => Ok(["-f", "gdigrab", "-framerate", "15", "-i", "desktop"]
            .iter()
            .map(|a| (*a).to_owned())
            .collect()),
        Kind::Audio => {
            let output = tokio::process::Command::new(crate::process::FFMPEG)
                .args([
                    "-hide_banner",
                    "-list_devices",
                    "true",
                    "-f",
                    "dshow",
                    "-i",
                    "dummy",
                ])
                .stdin(std::process::Stdio::null())
                .output()
                .await
                .map_err(|err| format!("cannot run ffmpeg: {err}"))?;
            let listing = String::from_utf8_lossy(&output.stderr);
            let mic = first_microphone(&listing).ok_or("ffmpeg lists no microphone")?;
            Ok(vec![
                "-f".into(),
                "dshow".into(),
                "-i".into(),
                format!("audio={mic}"),
            ])
        }
    }
}

/// The first `"<name>" (audio)` in ffmpeg's DirectShow device list.
fn first_microphone(listing: &str) -> Option<String> {
    listing
        .lines()
        .filter(|line| line.trim_end().ends_with("(audio)"))
        .find_map(|line| {
            let start = line.find('"')? + 1;
            let end = start + line.get(start..)?.find('"')?;
            line.get(start..end).map(str::to_owned)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_first_microphone() {
        let listing = "[in#0 @ 01] \"Integrated Camera\" (video)\n\
                       [in#0 @ 01]   Alternative name \"@device_pnp_x\"\n\
                       [in#0 @ 01] \"Microphone Array (Realtek(R) Audio)\" (audio)\n\
                       [in#0 @ 01] \"Headset (Airdopes Prime 413)\" (audio)\n";
        assert_eq!(
            first_microphone(listing).as_deref(),
            Some("Microphone Array (Realtek(R) Audio)")
        );
        assert_eq!(first_microphone("nothing here"), None);
    }
}
