//! Spots "hey bee", "okay bee" and "stop" in 16 kHz wav files, for checking the wake word.
//!
//! cargo run -p assist-voice --release --example spot -- <models dir> <file.wav>...

use std::path::Path;

use anyhow::{bail, Context};
use assist_voice::{Spotter, KWS_MODEL, RATE};
use sherpa_onnx as so;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((models, files)) = args.split_first() else {
        bail!("usage: spot <models dir> <file.wav>...");
    };
    let wake = ["hey bee", "okay bee", "ok bee"].map(str::to_owned);
    for file in files {
        // A fresh spotter per file, with half a second of lead-in as live audio has.
        let mut spotter = Spotter::new(&Path::new(models).join(KWS_MODEL), &wake)?;
        let wave = so::Wave::read(file).context("read wav")?;
        if wave.sample_rate() != RATE as i32 {
            bail!("{file}: {} Hz, need {RATE}", wave.sample_rate());
        }
        let silence = vec![0.0; RATE as usize / 2];
        let mut heard = Vec::new();
        for chunk in [&silence[..], wave.samples(), &silence[..], &silence[..]] {
            for part in chunk.chunks(1_600) {
                heard.extend(spotter.push(part));
            }
        }
        println!("{file}: {heard:?}");
    }
    Ok(())
}
