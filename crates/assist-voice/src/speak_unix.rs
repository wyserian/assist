//! Spoken replies through speech-dispatcher's `spd-say`, run as a separate process so no
//! GPL code is linked (CLAUDE.md).

use std::process::{Child, Command, Stdio};

use crate::VoiceError;

/// The speech-dispatcher default voice.
pub struct Speaker {
    child: Option<Child>,
}

impl Speaker {
    /// Checks that `spd-say` can run.
    pub fn new() -> Result<Self, VoiceError> {
        Command::new("spd-say")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| VoiceError::Speech(format!("spd-say: {e} (install speech-dispatcher)")))?;
        Ok(Self { child: None })
    }

    /// Starts speaking `text`, replacing anything still playing.
    pub fn say(&mut self, text: &str) -> Result<(), VoiceError> {
        self.stop()?;
        // `-w` keeps the process alive until speech ends, so `is_speaking` can watch it.
        let child = Command::new("spd-say")
            .args(["-w", "--", text])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| VoiceError::Speech(format!("spd-say: {e}")))?;
        self.child = Some(child);
        Ok(())
    }

    /// Whether a reply is playing.
    pub fn is_speaking(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }

    /// Silences the current reply at once.
    pub fn stop(&mut self) -> Result<(), VoiceError> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            // Killing the client leaves the message queued in the daemon; cancel it too.
            let _ = Command::new("spd-say").arg("-C").status();
        }
        Ok(())
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
