//! ffmpeg inputs on Linux: `x11grab` for the desktop, PulseAudio (or PipeWire's Pulse
//! server) for the microphone.
//!
//! shortcut: X11 only; a Wayland session needs the screencast portal, added with Linux
//! screenshots.

use super::Kind;

/// The input arguments for `kind`.
pub async fn input(kind: Kind) -> Result<Vec<String>, String> {
    let args: Vec<String> = match kind {
        Kind::Screen => {
            let display = std::env::var("DISPLAY").map_err(|_| "no X11 display ($DISPLAY)")?;
            vec![
                "-f".into(),
                "x11grab".into(),
                "-framerate".into(),
                "15".into(),
                "-i".into(),
                display,
            ]
        }
        Kind::Audio => ["-f", "pulse", "-i", "default"]
            .iter()
            .map(|a| (*a).to_owned())
            .collect(),
    };
    Ok(args)
}
