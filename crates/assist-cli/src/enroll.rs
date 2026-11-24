//! `assist enroll`: records the owner's voiceprint, so a spoken "yes" can confirm risky
//! commands (PLAN.md section 3.4). The voiceprint is biometric data: it stays in the data
//! directory, readable by this user only.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use assist_core::private_fs;
use assist_voice::{
    Event, Mic, Segmenter, Silero, SpeakerModel, VadSettings, Voiceprint, RATE, SPEAKER_MODEL,
    VAD_MODEL,
};

use crate::setup::Paths;

/// Sentences to read; `{name}` is the persona name. Short answers are in the mix because
/// confirmations are short.
const SENTENCES: [&str; 5] = [
    "Hey {name}, open firefox.",
    "Yes, call mom.",
    "Search the web for the weather tomorrow.",
    "Okay {name}, send a text to dad.",
    "Yes, do it.",
];

/// Shorter utterances make an unreliable voiceprint.
const MIN_SPEECH: usize = RATE as usize;

/// The voiceprint file in the data directory.
pub fn voiceprint_path(data: &Path) -> PathBuf {
    data.join("voiceprint.txt")
}

/// The saved voiceprint, if the owner enrolled.
pub fn load(data: &Path) -> Result<Option<Voiceprint>> {
    let path = voiceprint_path(data);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("cannot read {}", path.display())),
    };
    match Voiceprint::from_text(&text) {
        Some(print) => Ok(Some(print)),
        None => bail!("{} is damaged; run assist enroll again", path.display()),
    }
}

/// Records a new voiceprint, or deletes it with `forget`.
pub async fn run(paths: &Paths, device: Option<String>, forget: bool) -> Result<()> {
    let path = voiceprint_path(paths.data());
    if forget {
        match std::fs::remove_file(&path) {
            Ok(()) => println!("Voiceprint deleted."),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                println!("No voiceprint to delete.");
            }
            Err(err) => {
                return Err(err).with_context(|| format!("cannot delete {}", path.display()))
            }
        }
        return Ok(());
    }
    let name = crate::setup::load_config(paths)?.persona.name;
    let models = crate::models::ensure(paths.data()).await?;
    let print =
        tokio::task::spawn_blocking(move || record(&models, device.as_deref(), &name)).await??;
    private_fs::write_atomic(&path, print.to_text().as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))?;
    println!("Voiceprint saved. A spoken \"yes\" from you now confirms; assist enroll --forget deletes it.");
    Ok(())
}

/// Asks for each sentence and averages the voiceprints of the answers.
fn record(models: &Path, device: Option<&str>, name: &str) -> Result<Voiceprint> {
    let settings = VadSettings::default();
    let mut segmenter = Segmenter::new(Silero::new(&models.join(VAD_MODEL), &settings)?, &settings);
    let speaker = SpeakerModel::new(&models.join(SPEAKER_MODEL))?;
    let mic = Mic::open(device)?;
    println!(
        "Recording on {}. Read each sentence in your normal voice.",
        mic.name()
    );
    let mut prints = Vec::with_capacity(SENTENCES.len());
    let mut audio: Vec<f32> = Vec::with_capacity(RATE as usize * 15);
    for (i, sentence) in SENTENCES.iter().enumerate() {
        println!(
            "{}/{}: {}",
            i + 1,
            SENTENCES.len(),
            sentence.replace("{name}", name)
        );
        loop {
            let Some(chunk) = mic.recv(Duration::from_millis(100))? else {
                continue;
            };
            let mut done = false;
            segmenter.push(&chunk, |event| match event {
                Event::Start => audio.clear(),
                Event::Audio(samples) => audio.extend_from_slice(samples),
                Event::End => done = true,
            });
            if !done {
                continue;
            }
            match (audio.len() >= MIN_SPEECH)
                .then(|| speaker.voiceprint(&audio))
                .flatten()
            {
                Some(print) => {
                    prints.push(print);
                    break;
                }
                None => println!("That was too short; please read it again."),
            }
        }
    }
    Voiceprint::average(&prints).context("no voiceprint recorded")
}
