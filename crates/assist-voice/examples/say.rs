//! Speaks a sentence with the OS voice and prints how long synthesis took before playback.
//!
//! cargo run -p assist-voice --release --example say -- "Opening Firefox"

use std::time::{Duration, Instant};

use assist_voice::Speaker;

fn main() -> anyhow::Result<()> {
    let text: Vec<String> = std::env::args().skip(1).collect();
    let text = if text.is_empty() {
        "Opening Firefox".to_owned()
    } else {
        text.join(" ")
    };
    let mut speaker = Speaker::new()?;

    let started = Instant::now();
    speaker.say(&text)?;
    println!("playing after {} ms", started.elapsed().as_millis());

    // Wait for playback to start, then to end (bounded, in case the device never plays).
    let deadline = Instant::now() + Duration::from_secs(30);
    while !speaker.is_speaking() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    while speaker.is_speaking() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}
