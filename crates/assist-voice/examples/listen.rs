//! Prints when speech starts and ends on a microphone.
//!
//! cargo run -p assist-voice --release --example listen -- <silero_vad.onnx> [device]
//! cargo run -p assist-voice --release --example listen -- devices

use std::time::Duration;

use anyhow::Context;
use assist_voice::{input_devices, Event, Mic, Segmenter, Silero, VadSettings, RATE};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let model = args
        .next()
        .context("usage: listen <silero_vad.onnx> [device] | devices")?;
    if model == "devices" {
        for name in input_devices()? {
            println!("{name}");
        }
        return Ok(());
    }
    let device = args.next();
    let settings = VadSettings::default();
    let mut segmenter = Segmenter::new(Silero::new(model.as_ref(), &settings)?, &settings);
    let mic = Mic::open(device.as_deref())?;
    println!("Listening on {}. Speak; Ctrl+C stops.", mic.name());

    let mut samples = 0usize;
    loop {
        let Some(chunk) = mic.recv(Duration::from_secs(1))? else {
            continue;
        };
        segmenter.push(&chunk, |event| match event {
            Event::Start => {
                samples = 0;
                print!("speech ... ");
            }
            Event::Audio(audio) => samples += audio.len(),
            Event::End => println!("end, {:.1} s with pre-roll", samples as f32 / RATE as f32),
        });
        std::io::Write::flush(&mut std::io::stdout())?;
    }
}
