//! `assist listen`: spoken commands to the daemon, spoken replies back. The daemon gives this
//! connection voice trust (PLAN.md section 3.6). Audio, recognition and speech run on their
//! own thread; this task only talks to the daemon.
//!
//! Speech counts only after the wake word ("hey Bee") or within [`AWAKE`] of the last
//! exchange. The wake word or "stop" cuts a reply short (barge-in). With a voiceprint from
//! `assist enroll`, each utterance is checked against it, and only the owner's "yes"
//! confirms.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use assist_core::replies::{Line, Replies};
use assist_proto::Reply;
use assist_voice::{
    Event, Keyword, Mic, Recognizer, Segmenter, Silero, Speaker, SpeakerModel, Spotter,
    VadSettings, VoiceError, Voiceprint, KWS_MODEL, RATE, SPEAKER_MODEL, STT_MODEL, VAD_MODEL,
};
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc as async_mpsc;

use crate::client::Client;
use crate::display::printable;
use crate::setup::Paths;

/// How long the assistant keeps listening without the wake word after it was woken or last
/// spoke, so answers to its questions need no wake word.
const AWAKE: Duration = Duration::from_secs(8);

/// What the voice thread reports.
enum Heard {
    /// Models loaded and the microphone is open.
    Ready(String),
    /// The wake word alone; the command comes next.
    Woke,
    /// A finished utterance addressed to the assistant, with how close the voice is to the
    /// owner's voiceprint (when there is one).
    Text(String, Option<f32>),
    /// Voice stopped working.
    Failed(String),
}

/// What the voice thread needs.
struct Setup {
    models: PathBuf,
    device: Option<String>,
    wake: Vec<String>,
    owner: Option<Voiceprint>,
}

/// Listens until Ctrl+C; Enter pauses and resumes the microphone.
pub async fn run(paths: &Paths, device: Option<String>) -> Result<()> {
    let config = crate::setup::load_config(paths)?;
    let replies = Replies::new(&config);
    let threshold = config.voice.speaker_threshold;
    let setup = Setup {
        models: crate::models::ensure(paths.data()).await?,
        device,
        wake: config.persona.wake_phrases(),
        owner: crate::enroll::load(paths.data())?,
    };
    let hint = setup.wake.first().cloned().unwrap_or_default();
    if setup.owner.is_none() {
        println!("No voiceprint: confirm calls and messages on screen, or run assist enroll.");
    }
    let mut client = Client::connect(paths.data(), true).await?;
    client.subscribe(&["notices"]).await?;
    println!("Loading voice models...");

    let paused = Arc::new(AtomicBool::new(false));
    let (heard_tx, mut heard) = async_mpsc::channel(4);
    let (say_tx, say_rx) = mpsc::sync_channel::<String>(4);
    let thread_paused = Arc::clone(&paused);
    let voice = std::thread::Builder::new()
        .name("assist-voice".into())
        .spawn(move || {
            let result = voice_loop(&setup, &thread_paused, &heard_tx, &say_rx);
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
            notice = client.notice() => match notice {
                Ok(notice) => {
                    println!("{}", printable(&notice.text));
                    let _ = say_tx.try_send(notice.text);
                }
                Err(err) => break Err(err),
            },
            heard = heard.recv() => match heard {
                Some(Heard::Ready(mic)) => {
                    println!("Listening on {mic}. Say \"{}\" first; Enter pauses, Ctrl+C stops.", printable(&hint));
                    let greeting = replies.line(Line::Greeting, &[]);
                    println!("{}", printable(&greeting));
                    let _ = say_tx.try_send(greeting);
                }
                Some(Heard::Woke) => {
                    let line = replies.line(Line::Wake, &[]);
                    println!("{}", printable(&line));
                    let _ = say_tx.try_send(line);
                }
                Some(Heard::Text(text, score)) => {
                    let verified = score.is_some_and(|score| score >= threshold);
                    match score {
                        Some(score) => println!("> {}  [voice {score:.2}]", printable(&text)),
                        None => println!("> {}", printable(&text)),
                    }
                    let reply = client.say_as(&text, verified).await?;
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

/// Mic, wake word, VAD, recognition, speaker check and speech, until `say` is closed or the
/// receiver is gone.
fn voice_loop(
    setup: &Setup,
    paused: &AtomicBool,
    heard: &async_mpsc::Sender<Heard>,
    say: &Receiver<String>,
) -> Result<(), VoiceError> {
    let models: &Path = &setup.models;
    let settings = VadSettings::default();
    let vad = Silero::new(&models.join(VAD_MODEL), &settings)?;
    let mut segmenter = Segmenter::new(vad, &settings);
    let recognizer = Recognizer::new(&models.join(STT_MODEL))?;
    let mut spotter = Spotter::new(&models.join(KWS_MODEL), &setup.wake)?;
    let check = match &setup.owner {
        Some(owner) => Some((SpeakerModel::new(&models.join(SPEAKER_MODEL))?, owner)),
        None => None,
    };
    let mut speaker = Speaker::new()?;
    let mic = Mic::open(setup.device.as_deref())?;
    if heard
        .blocking_send(Heard::Ready(mic.name().to_owned()))
        .is_err()
    {
        return Ok(());
    }

    let mut transcript = None;
    // The utterance's audio for the speaker check; bounded by the VAD's longest utterance.
    let mut utterance: Vec<f32> = Vec::with_capacity(RATE as usize * 15);
    let mut awake_until: Option<Instant> = None;
    // The wake word was heard in the current utterance.
    let mut woke = false;
    loop {
        match say.try_recv() {
            Ok(text) => speaker.say(&text)?,
            Err(TryRecvError::Disconnected) => return Ok(()),
            Err(TryRecvError::Empty) => {}
        }
        let Some(chunk) = mic.recv(Duration::from_millis(50))? else {
            continue;
        };
        if paused.load(Ordering::Relaxed) {
            transcript = None;
            continue;
        }
        let keyword = spotter.push(&chunk);
        let now = Instant::now();
        let awake = awake_until.is_some_and(|until| until > now);
        // Half duplex: only keywords are heard while the assistant speaks.
        if speaker.is_speaking() {
            if keyword.is_some() {
                speaker.stop()?;
            }
            if awake || keyword == Some(Keyword::Wake) {
                awake_until = Some(now + AWAKE);
            }
            transcript = None;
            continue;
        }
        if keyword == Some(Keyword::Wake) {
            awake_until = Some(now + AWAKE);
            woke = true;
            // Recognition restarts after the wake word, so the text is the command alone.
            // shortcut: audio between the wake word's end and its detection is dropped, a
            // few tens of ms; upgrade to the spotter's timestamps if first words go missing.
            if segmenter.is_speaking() {
                transcript = Some(recognizer.start());
                utterance.clear();
            }
        }
        let mut gone = false;
        segmenter.push(&chunk, |event| match event {
            Event::Start => {
                transcript = Some(recognizer.start());
                utterance.clear();
            }
            Event::Audio(audio) => {
                if let Some(t) = transcript.as_mut() {
                    t.feed(audio);
                    if check.is_some() {
                        utterance.extend_from_slice(audio);
                    }
                }
            }
            Event::End => {
                let text = transcript.take().map(|t| t.finish()).unwrap_or_default();
                let addressed = awake_until.is_some_and(|until| until > Instant::now());
                let message = if !text.is_empty() && addressed {
                    awake_until = Some(Instant::now() + AWAKE);
                    let score = check.as_ref().map(|(model, owner)| {
                        model
                            .voiceprint(&utterance)
                            .map_or(0.0, |print| owner.score(&print))
                    });
                    Some(Heard::Text(text, score))
                } else if woke {
                    Some(Heard::Woke)
                } else {
                    None
                };
                woke = false;
                if message.is_some_and(|m| heard.blocking_send(m).is_err()) {
                    gone = true;
                }
            }
        });
        if gone {
            return Ok(());
        }
    }
}
