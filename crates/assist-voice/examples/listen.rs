//! Prints what is said on a microphone and how long recognition took after speech ended.
//!
//! cargo run -p assist-voice --release --example listen -- <models dir> [device]
//! cargo run -p assist-voice --release --example listen -- devices
//!
//! The models directory holds `silero_vad.onnx` and the Kroko model directory.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context;
use assist_voice::{
    input_devices, Event, Mic, Recognizer, Segmenter, Silero, VadSettings, STT_MODEL, VAD_MODEL,
};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let models = args
        .next()
        .context("usage: listen <models dir> [device] | devices")?;
    if models == "devices" {
        for name in input_devices()? {
            println!("{name}");
        }
        return Ok(());
    }
    let models = Path::new(&models);
    let device = args.next();
    let settings = VadSettings::default();
    let mut segmenter = Segmenter::new(Silero::new(&models.join(VAD_MODEL), &settings)?, &settings);
    let recognizer = Recognizer::new(&models.join(STT_MODEL))?;
    let mic = Mic::open(device.as_deref())?;
    println!("Listening on {}. Speak; Ctrl+C stops.", mic.name());

    let mut transcript = None;
    loop {
        let Some(chunk) = mic.recv(Duration::from_secs(1))? else {
            continue;
        };
        segmenter.push(&chunk, |event| match event {
            Event::Start => transcript = Some(recognizer.start()),
            Event::Audio(audio) => {
                if let Some(t) = transcript.as_mut() {
                    t.feed(audio);
                }
            }
            Event::End => {
                if let Some(t) = transcript.take() {
                    let ended = Instant::now();
                    let text = t.finish();
                    let ms = ended.elapsed().as_millis();
                    println!("{text:?}  ({ms} ms after speech ended)");
                }
            }
        });
    }
}
