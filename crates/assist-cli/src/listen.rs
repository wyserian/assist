//! `assist listen`: spoken commands to the daemon, spoken replies back. The daemon gives this
//! connection voice trust (PLAN.md section 3.6). Audio, recognition and speech run on their
//! own thread; this task only talks to the daemon.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use assist_core::replies::{Line, Replies};
use assist_proto::Reply;
use assist_voice::{
    Event, Mic, Recognizer, Segmenter, Silero, Speaker, VadSettings, VoiceError, STT_MODEL,
    VAD_MODEL,
};
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc as async_mpsc;

use crate::client::Client;
use crate::display::printable;
use crate::setup::Paths;

/// What the voice thread reports.
enum Heard {
    /// Models loaded and the microphone is open.
    Ready(String),
    /// A finished utterance.
    Text(String),
    /// Voice stopped working.
    Failed(String),
}

/// Listens until Ctrl+C; Enter pauses and resumes the microphone.
pub async fn run(paths: &Paths, device: Option<String>) -> Result<()> {
    let replies = Replies::new(&crate::setup::load_config(paths)?);
    let models = crate::models::ensure(paths.data()).await?;
    let mut client = Client::connect(paths.data(), true).await?;
    println!("Loading voice models...");

    let paused = Arc::new(AtomicBool::new(false));
    let (heard_tx, mut heard) = async_mpsc::channel(4);
    let (say_tx, say_rx) = mpsc::sync_channel::<String>(4);
    let thread_paused = Arc::clone(&paused);
    let voice = std::thread::Builder::new()
        .name("assist-voice".into())
        .spawn(move || {
            let result = voice_loop(
                &models,
                device.as_deref(),
                &thread_paused,
                &heard_tx,
                &say_rx,
            );
            if let Err(e) = result {
                let _ = heard_tx.blocking_send(Heard::Failed(e.to_string()));
            }
        })?;

    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut stdin_open = true;
    let result = loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break Ok(()),
            line = stdin.next_line(), if stdin_open => match line {
                Ok(Some(_)) => {
                    let pause = !paused.load(Ordering::Relaxed);
                    paused.store(pause, Ordering::Relaxed);
                    println!("{}", if pause { "Paused. Enter resumes." } else { "Listening." });
                }
                _ => stdin_open = false,
            },
            heard = heard.recv() => match heard {
                Some(Heard::Ready(mic)) => {
                    println!("Listening on {mic}. Speak a command; Enter pauses, Ctrl+C stops.");
                    let greeting = replies.line(Line::Greeting, &[]);
                    println!("{}", printable(&greeting));
                    let _ = say_tx.try_send(greeting);
                }
                Some(Heard::Text(text)) => {
                    println!("> {}", printable(&text));
                    let reply = client.say(&text).await?;
                    show(&reply);
                    // A full queue means replies pile up faster than they are spoken; skip.
                    if !reply.say.is_empty() {
                        let _ = say_tx.try_send(reply.say.join(" "));
                    }
                }
                Some(Heard::Failed(error)) => break Err(anyhow::anyhow!("voice: {error}")),
                None => break Ok(()),
            },
        }
    };
    drop(say_tx);
    if voice.join().is_err() {
        bail!("the voice thread panicked");
    }
    result
}

fn show(reply: &Reply) {
    for line in &reply.say {
        println!("{}", printable(line));
    }
    for job in &reply.jobs {
        println!("[#{}] {}", job.id, printable(&job.title));
    }
}

/// Mic, VAD, recognition and speech, until `say` is closed or the receiver is gone.
fn voice_loop(
    models: &Path,
    device: Option<&str>,
    paused: &AtomicBool,
    heard: &async_mpsc::Sender<Heard>,
    say: &Receiver<String>,
) -> Result<(), VoiceError> {
    let settings = VadSettings::default();
    let vad = Silero::new(&models.join(VAD_MODEL), &settings)?;
    let mut segmenter = Segmenter::new(vad, &settings);
    let recognizer = Recognizer::new(&models.join(STT_MODEL))?;
    let mut speaker = Speaker::new()?;
    let mic = Mic::open(device)?;
    if heard
        .blocking_send(Heard::Ready(mic.name().to_owned()))
        .is_err()
    {
        return Ok(());
    }

    let mut transcript = None;
    loop {
        match say.try_recv() {
            Ok(text) => speaker.say(&text)?,
            Err(TryRecvError::Disconnected) => return Ok(()),
            Err(TryRecvError::Empty) => {}
        }
        let Some(chunk) = mic.recv(Duration::from_millis(50))? else {
            continue;
        };
        // Half duplex: neither paused audio nor the assistant's own voice is heard.
        if paused.load(Ordering::Relaxed) || speaker.is_speaking() {
            transcript = None;
            continue;
        }
        let mut gone = false;
        segmenter.push(&chunk, |event| match event {
            Event::Start => transcript = Some(recognizer.start()),
            Event::Audio(audio) => {
                if let Some(t) = transcript.as_mut() {
                    t.feed(audio);
                }
            }
            Event::End => {
                let text = transcript.take().map(|t| t.finish()).unwrap_or_default();
                if !text.is_empty() && heard.blocking_send(Heard::Text(text)).is_err() {
                    gone = true;
                }
            }
        });
        if gone {
            return Ok(());
        }
    }
}
