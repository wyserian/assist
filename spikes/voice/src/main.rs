//! Voice engine spike (PLAN.md Phase 2): which speech engine turns a spoken command into the
//! right intent fastest on this machine.
//!
//! `record` asks for every voice case in `config/cases.toml` and saves `clips/NN.wav`.
//! `convert <prefix>` turns clips from another recorder (`clips/<prefix> N.mp3`) into the same.
//! `eval` runs each engine on the clips and checks every transcript with the real matcher;
//! with `clips/said.txt` (`NN text` per line) it also scores word error rate against that.
//! `wav <engine> <file>` transcribes one file. Models go in `models/` (see PLAN.md section 13).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use assist_core::config::Config;
use assist_core::matcher::{Matcher, RuleMatcher};
use assist_core::normalize::normalize;
use assist_core::phrases::PhraseSet;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::Deserialize;
use sherpa_onnx as so;

/// Sample rate every engine gets.
const RATE: i32 = 16_000;
/// Silence after speech before an utterance counts as finished. Every engine waits for it,
/// so it is added to every latency.
const ENDPOINT_S: f32 = 0.3;
/// Silence fed to streaming models after the endpoint to flush their last words (2 s).
const FLUSH: usize = 32_000;
/// Audio kept before the speech starts (1 s).
const PRE_ROLL: usize = 16_000;
/// Samples fed per step, like a 100 ms capture callback.
const CHUNK: usize = 1_600;
/// Longest clip kept, seconds.
const MAX_CLIP_S: usize = 15;
/// Threads per engine, the same as the daemon's worker count.
const THREADS: i32 = 2;
/// Cases a voice cannot say.
const TYPED_ONLY: &[&str] = &["open firfox"];
/// SentencePiece word-start marker in `tokens.txt`.
const WORD_START: char = '\u{2581}';

#[derive(Deserialize)]
struct Cases {
    case: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    text: String,
    command: Option<String>,
    #[serde(default)]
    slots: BTreeMap<String, String>,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("record") => record(args.get(1).map(|n| n.parse()).transpose()?),
        Some("eval") => eval(args.get(1).map(String::as_str)),
        Some("convert") => convert(args.get(1).context("missing file name prefix")?),
        Some("kws") => kws(&args[1..]),
        Some("wav") => {
            let (engine, path) = (args.get(1).context("engine")?, args.get(2).context("path")?);
            let wave = so::Wave::read(path).context("read wav")?;
            let words = BTreeSet::new();
            let (text, _, total) = load(engine, &words)?.run(wave.samples(), &[0.0; 4_800]);
            println!("{} Hz, {:.1} s, {} ms: {text}", wave.sample_rate(), wave.num_samples() as f32 / wave.sample_rate() as f32, total.as_millis());
            Ok(())
        }
        _ => bail!(
            "usage: spike.ps1 record [case number] | eval [engine name filter] | convert <prefix>"
        ),
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn model(path: &str) -> Option<String> {
    Some(root().join("models").join(path).to_string_lossy().into_owned())
}

fn clip_path(number: usize) -> PathBuf {
    root().join("clips").join(format!("{number:02}.wav"))
}

fn cases() -> Result<Vec<Case>> {
    let cases: Cases = toml::from_str(include_str!("../../../config/cases.toml"))?;
    Ok(cases
        .case
        .into_iter()
        .filter(|c| !TYPED_ONLY.contains(&c.text.as_str()))
        .collect())
}

fn read_line() -> Result<String> {
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

// ---- record ----

fn record(only: Option<usize>) -> Result<()> {
    let device = cpal::default_host()
        .default_input_device()
        .context("no microphone found")?;
    let config = device.default_input_config()?;
    let rate = config.sample_rate();
    let channels = usize::from(config.channels());
    println!("Microphone: {rate} Hz, {channels} channel(s), {:?}", config.sample_format());

    let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
    let on = Arc::new(AtomicBool::new(false));
    let cap = MAX_CLIP_S * rate as usize;
    let push = {
        let buffer = Arc::clone(&buffer);
        let on = Arc::clone(&on);
        move |frames: &mut dyn Iterator<Item = f32>| {
            if !on.load(Ordering::Relaxed) {
                return;
            }
            if let Ok(mut buf) = buffer.lock() {
                let room = cap.saturating_sub(buf.len());
                buf.extend(frames.take(room));
            }
        }
    };
    let err = |e| eprintln!("microphone error: {e}");
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            let push = push.clone();
            device.build_input_stream(
                config.config(),
                move |data: &[f32], _: &_| {
                    push(&mut data.chunks(channels).map(|f| f.iter().sum::<f32>() / f.len() as f32))
                },
                err,
                None,
            )?
        }
        cpal::SampleFormat::I16 => device.build_input_stream(
            config.config(),
            move |data: &[i16], _: &_| {
                push(&mut data.chunks(channels).map(|f| {
                    f.iter().map(|&s| f32::from(s) / 32768.0).sum::<f32>() / f.len() as f32
                }))
            },
            err,
            None,
        )?,
        other => bail!("unsupported sample format {other:?}"),
    };
    stream.play()?;

    std::fs::create_dir_all(root().join("clips"))?;
    let cases = cases()?;
    println!("Press Enter, say the sentence, then press Enter again. Speak normally.\n");
    for (number, case) in cases.iter().enumerate().map(|(i, c)| (i + 1, c)) {
        if only.is_some_and(|n| n != number) {
            continue;
        }
        println!("[{number}/{}] say: {}", cases.len(), case.text);
        print!("  Enter to start ");
        std::io::Write::flush(&mut std::io::stdout())?;
        read_line()?;
        if let Ok(mut buf) = buffer.lock() {
            buf.clear();
        }
        on.store(true, Ordering::Relaxed);
        print!("  recording, Enter to stop ");
        std::io::Write::flush(&mut std::io::stdout())?;
        read_line()?;
        on.store(false, Ordering::Relaxed);
        let samples = buffer.lock().map(|b| b.clone()).unwrap_or_default();
        let resampler =
            so::LinearResampler::create(rate as i32, RATE).context("resampler")?;
        let clip = resampler.resample(&samples, true);
        let peak = clip.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        if !so::write(&clip_path(number).to_string_lossy(), &clip, RATE) {
            bail!("could not write clip {number}");
        }
        let quiet = if peak < 0.05 { "  (very quiet, re-record with: spike.ps1 record N)" } else { "" };
        println!("  saved {:.1} s, peak {peak:.2}{quiet}\n", clip.len() as f32 / RATE as f32);
    }
    Ok(())
}

// ---- convert ----

/// Turns clips made with another recorder, `clips/<prefix> N.mp3` for case N, into `NN.wav`.
fn convert(prefix: &str) -> Result<()> {
    for number in 1..=cases()?.len() {
        let mp3 = root().join("clips").join(format!("{prefix} {number}.mp3"));
        if !mp3.exists() {
            println!("missing {}", mp3.display());
            continue;
        }
        let (samples, rate) = decode_mp3(&mp3).with_context(|| mp3.display().to_string())?;
        let resampler = so::LinearResampler::create(rate, RATE).context("resampler")?;
        let clip = resampler.resample(&samples, true);
        if !so::write(&clip_path(number).to_string_lossy(), &clip, RATE) {
            bail!("could not write clip {number}");
        }
        let peak = clip.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        println!("{number:02}: {:.1} s, peak {peak:.2}", clip.len() as f32 / RATE as f32);
    }
    Ok(())
}

/// Decodes an MP3 file to mono samples and its sample rate.
fn decode_mp3(path: &Path) -> Result<(Vec<f32>, i32)> {
    use symphonia::core::{audio::SampleBuffer, codecs::DecoderOptions, errors::Error,
        formats::FormatOptions, io::MediaSourceStream, meta::MetadataOptions, probe::Hint};
    let file = std::fs::File::open(path)?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let mut format = symphonia::default::get_probe()
        .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())?
        .format;
    let track = format.default_track().context("no audio track")?;
    let track_id = track.id;
    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let mut out = Vec::new();
    let mut rate = 0;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        rate = spec.rate as i32;
        let channels = spec.channels.count().max(1);
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        out.extend(buf.samples().chunks(channels).map(|f| f.iter().sum::<f32>() / f.len() as f32));
    }
    Ok((out, rate))
}

// ---- engines ----

/// A speech engine under test.
trait Engine {
    /// Transcribes `speech` followed by `tail` (the endpoint silence). Returns the text, the
    /// compute time after the speech ended and the total compute time.
    fn run(&mut self, speech: &[f32], tail: &[f32]) -> (String, Duration, Duration);
}

struct Online {
    recognizer: so::OnlineRecognizer,
    hotwords: Option<String>,
}

impl Engine for Online {
    fn run(&mut self, speech: &[f32], tail: &[f32]) -> (String, Duration, Duration) {
        let r = &self.recognizer;
        let stream = match &self.hotwords {
            Some(words) => r.create_stream_with_hotwords(words),
            None => r.create_stream(),
        };
        let start = Instant::now();
        for chunk in speech.chunks(CHUNK) {
            stream.accept_waveform(RATE, chunk);
            while r.is_ready(&stream) {
                r.decode(&stream);
            }
        }
        let ended = Instant::now();
        stream.accept_waveform(RATE, tail);
        // Zeros that flush the model's right context, as in sherpa-onnx's own examples; this
        // costs compute only, no waiting.
        stream.accept_waveform(RATE, &[0.0; FLUSH]);
        stream.input_finished();
        while r.is_ready(&stream) {
            r.decode(&stream);
        }
        let text = r.get_result(&stream).map(|x| x.text).unwrap_or_default();
        (text, ended.elapsed(), start.elapsed())
    }
}

/// Non-streaming model: VAD cuts the utterance, then the whole segment is decoded.
struct Offline(so::OfflineRecognizer);

impl Engine for Offline {
    fn run(&mut self, speech: &[f32], _tail: &[f32]) -> (String, Duration, Duration) {
        let start = Instant::now();
        let stream = self.0.create_stream();
        stream.accept_waveform(RATE, speech);
        self.0.decode(&stream);
        let text = stream.get_result().map(|x| x.text).unwrap_or_default();
        (text, start.elapsed(), start.elapsed())
    }
}

struct Vosk(vosk::Recognizer);

fn pcm(samples: &[f32]) -> Vec<i16> {
    samples.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect()
}

impl Engine for Vosk {
    fn run(&mut self, speech: &[f32], tail: &[f32]) -> (String, Duration, Duration) {
        let (speech, tail) = (pcm(speech), pcm(tail));
        let start = Instant::now();
        for chunk in speech.chunks(CHUNK) {
            let _ = self.0.accept_waveform(chunk);
        }
        let ended = Instant::now();
        let _ = self.0.accept_waveform(&tail);
        let text = self
            .0
            .final_result()
            .single()
            .map(|r| r.text.to_owned())
            .unwrap_or_default();
        self.0.reset();
        (text, ended.elapsed(), start.elapsed())
    }
}

/// Words the commands can contain: literal phrase words, list values and number words.
fn vocabulary(phrases: &PhraseSet, config: &Config) -> BTreeSet<String> {
    let mut words = BTreeSet::new();
    for (_, spec) in phrases.commands() {
        for phrase in &spec.phrases {
            // Drop `.slot.` names, keep literal words.
            let literal: String = phrase
                .split('.')
                .enumerate()
                .filter(|(i, _)| i % 2 == 0)
                .map(|(_, part)| part)
                .collect::<Vec<_>>()
                .join(" ");
            words.extend(
                literal
                    .split(|c: char| !c.is_ascii_alphabetic() && c != '\'')
                    .filter(|w| !w.is_empty())
                    .map(str::to_owned),
            );
        }
    }
    for values in config.lists().into_values() {
        for value in values {
            words.extend(value.split_whitespace().map(str::to_owned));
        }
    }
    words.extend(
        "zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen \
         fifteen sixteen seventeen eighteen nineteen twenty thirty forty fifty sixty seventy \
         eighty ninety hundred"
            .split_whitespace()
            .map(str::to_owned),
    );
    words
}

/// Greedy longest-match split of `word` into model tokens; `None` if a piece is missing.
fn tokenize(word: &str, tokens: &HashSet<String>, upper: bool) -> Option<Vec<String>> {
    let word = if upper { word.to_uppercase() } else { word.to_lowercase() };
    let mut out = Vec::new();
    let mut rest = word.as_str();
    while !rest.is_empty() {
        let first = out.is_empty();
        let (token, len) = (1..=rest.len()).rev().find_map(|end| {
            let piece = rest.get(..end)?;
            let token = if first { format!("{WORD_START}{piece}") } else { piece.to_owned() };
            tokens.contains(&token).then_some((token, end))
        })?;
        out.push(token);
        rest = &rest[len..];
    }
    Some(out)
}

/// Hotwords in the model's own tokens, one per line.
fn hotwords(dir: &str, words: &BTreeSet<String>) -> Result<String> {
    let src = std::fs::read_to_string(root().join("models").join(dir).join("tokens.txt"))?;
    let tokens: HashSet<String> =
        src.lines().filter_map(|l| l.split_whitespace().next()).map(str::to_owned).collect();
    let upper = tokens.contains(&format!("{WORD_START}THE"));
    let lines: Vec<String> = words
        .iter()
        .filter_map(|w| tokenize(w, &tokens, upper))
        .map(|t| t.join(" "))
        .collect();
    Ok(lines.join("\n"))
}

/// sentencepiece vocab for sherpa's own hotword tokenizer: tokens.txt is in sentencepiece id
/// order, and a BPE piece's score is minus its id.
fn bpe_vocab(dir: &str) -> Result<String> {
    let src = std::fs::read_to_string(root().join("models").join(dir).join("tokens.txt"))?;
    let path = root().join("models").join(format!("{dir}.bpe.vocab"));
    let lines: Vec<String> = src
        .lines()
        .filter_map(|l| l.rsplit_once(' '))
        .map(|(t, id)| format!("{t}	-{id}"))
        .collect();
    std::fs::write(&path, lines.join("
"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Hotwords as plain words, lower-case and capitalised, one per line.
fn plain_hotwords(words: &BTreeSet<String>) -> String {
    let mut out = Vec::new();
    for w in words.iter().filter(|w| w.len() > 2) {
        out.push(w.to_lowercase());
        let mut c = w.chars();
        if let Some(f) = c.next() {
            out.push(f.to_uppercase().chain(c).collect());
        }
    }
    out.join("
")
}

fn kroko_bpe(words: &BTreeSet<String>, score: f32) -> Result<Box<dyn Engine>> {
    let mut c = so::OnlineRecognizerConfig::default();
    c.model_config.transducer.encoder = model(&format!("{KROKO}/encoder.onnx"));
    c.model_config.transducer.decoder = model(&format!("{KROKO}/decoder.onnx"));
    c.model_config.transducer.joiner = model(&format!("{KROKO}/joiner.onnx"));
    c.model_config.tokens = model(&format!("{KROKO}/tokens.txt"));
    c.model_config.num_threads = THREADS;
    c.model_config.modeling_unit = Some("bpe".into());
    c.model_config.bpe_vocab = Some(bpe_vocab(KROKO)?);
    c.decoding_method = Some("modified_beam_search".into());
    c.max_active_paths = 4;
    c.hotwords_score = score;
    let recognizer = so::OnlineRecognizer::create(&c).context("create online recognizer")?;
    Ok(Box::new(Online { recognizer, hotwords: Some(plain_hotwords(words)) }))
}

fn online(dir: &str, encoder: &str, decoder: &str, joiner: &str, hot: Option<String>) -> Result<Box<dyn Engine>> {
    let mut c = so::OnlineRecognizerConfig::default();
    c.model_config.transducer.encoder = model(&format!("{dir}/{encoder}"));
    c.model_config.transducer.decoder = model(&format!("{dir}/{decoder}"));
    c.model_config.transducer.joiner = model(&format!("{dir}/{joiner}"));
    c.model_config.tokens = model(&format!("{dir}/tokens.txt"));
    c.model_config.num_threads = THREADS;
    if hot.is_some() {
        c.decoding_method = Some("modified_beam_search".into());
        c.max_active_paths = 4;
        c.hotwords_score = 2.0;
    } else {
        c.decoding_method = Some("greedy_search".into());
    }
    let recognizer = so::OnlineRecognizer::create(&c).context("create online recognizer")?;
    Ok(Box::new(Online { recognizer, hotwords: hot }))
}

const KROKO: &str = "sherpa-onnx-streaming-zipformer-en-kroko-2025-08-06";
const SMALL: &str = "sherpa-onnx-streaming-zipformer-en-20M-2023-02-17-mobile";
const MOONSHINE: &str = "sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27";
const VOSK: &str = "vosk-model-small-en-us-0.15";

fn load(name: &str, words: &BTreeSet<String>) -> Result<Box<dyn Engine>> {
    match name {
        "A kroko" => online(KROKO, "encoder.onnx", "decoder.onnx", "joiner.onnx", None),
        "A kroko +hot" => {
            online(KROKO, "encoder.onnx", "decoder.onnx", "joiner.onnx", Some(hotwords(KROKO, words)?))
        }
        "A kroko beam" => kroko_bpe(words, 0.0),
        "K kws as asr" => online(
            KWS,
            "encoder-epoch-12-avg-2-chunk-16-left-64.onnx",
            "decoder-epoch-12-avg-2-chunk-16-left-64.onnx",
            "joiner-epoch-12-avg-2-chunk-16-left-64.onnx",
            None,
        ),
        "A kroko bpe1.5" => kroko_bpe(words, 1.5),
        "A kroko bpe3" => kroko_bpe(words, 3.0),
        "B 20M" => online(SMALL, "encoder-epoch-99-avg-1.int8.onnx", "decoder-epoch-99-avg-1.onnx", "joiner-epoch-99-avg-1.int8.onnx", None),
        "B 20M +hot" => online(
            SMALL,
            "encoder-epoch-99-avg-1.int8.onnx",
            "decoder-epoch-99-avg-1.onnx",
            "joiner-epoch-99-avg-1.int8.onnx",
            Some(hotwords(SMALL, words)?),
        ),
        "C moonshine" => {
            let mut c = so::OfflineRecognizerConfig::default();
            c.model_config.moonshine.encoder = model(&format!("{MOONSHINE}/encoder_model.ort"));
            c.model_config.moonshine.merged_decoder =
                model(&format!("{MOONSHINE}/decoder_model_merged.ort"));
            c.model_config.tokens = model(&format!("{MOONSHINE}/tokens.txt"));
            c.model_config.num_threads = THREADS;
            Ok(Box::new(Offline(
                so::OfflineRecognizer::create(&c).context("create offline recognizer")?,
            )))
        }
        "D vosk" | "D vosk grammar" => {
            vosk::set_log_level(vosk::LogLevel::Error);
            let path = root().join("models").join(VOSK);
            let m = vosk::Model::new(path.to_string_lossy()).context("load vosk model")?;
            let r = if name == "D vosk" {
                vosk::Recognizer::new(&m, RATE as f32)
            } else {
                let mut grammar: Vec<&str> = words.iter().map(String::as_str).collect();
                grammar.push("[unk]");
                vosk::Recognizer::new_with_grammar(&m, RATE as f32, &grammar)
            };
            Ok(Box::new(Vosk(r.context("create vosk recognizer")?)))
        }
        _ => bail!("unknown engine {name}"),
    }
}

const ENGINES: &[&str] = &[
    "A kroko",
    "A kroko +hot",
    "A kroko beam",
    "A kroko bpe1.5",
    "A kroko bpe3",
    "B 20M",
    "B 20M +hot",
    "C moonshine",
    "D vosk",
    "D vosk grammar",
];

// ---- keyword spotting ----

const KWS: &str = "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01";

/// Keywords: phrase, boost, threshold, label.
const KEYWORDS: [(&str, &str); 4] = [
    ("HEY BEE", ":1.5 #0.25 @hey_bee"),
    ("OKAY BEE", ":1.5 #0.25 @okay_bee"),
    ("OK BEE", ":1.5 #0.25 @okay_bee"),
    ("STOP", ":1.0 #0.35 @stop"),
];

/// sentencepiece BPE: start from characters, keep merging the adjacent pair whose joined
/// piece ranks best (lowest id in tokens.txt).
fn bpe(phrase: &str, rank: &HashMap<String, usize>) -> Vec<String> {
    let mut out = Vec::new();
    for word in phrase.split_whitespace() {
        let mut pieces: Vec<String> = word.chars().map(String::from).collect();
        if let Some(first) = pieces.first_mut() {
            first.insert(0, WORD_START);
        }
        loop {
            let best = (0..pieces.len().saturating_sub(1))
                .filter_map(|i| rank.get(&format!("{}{}", pieces[i], pieces[i + 1])).map(|r| (*r, i)))
                .min();
            let Some((_, i)) = best else { break };
            let next = pieces.remove(i + 1);
            pieces[i].push_str(&next);
        }
        out.extend(pieces);
    }
    out
}

fn keywords_buf() -> Result<String> {
    let src = std::fs::read_to_string(root().join("models").join(KWS).join("tokens.txt"))?;
    let rank: HashMap<String, usize> = src
        .lines()
        .filter_map(|l| l.rsplit_once(' '))
        .filter_map(|(t, id)| Some((t.to_owned(), id.parse().ok()?)))
        .collect();
    let lines: Vec<String> = KEYWORDS
        .iter()
        .map(|(phrase, opts)| {
            // KWS_OPTS="BOOST THRESHOLD" overrides the wake phrases' options for tuning.
            let opts = match std::env::var("KWS_OPTS") {
                Ok(o) if *phrase != "STOP" => {
                    let (b, t) = o.split_once(' ').unwrap_or(("1.5", "0.25"));
                    let label = opts.rsplit_once('@').map_or("", |x| x.1);
                    format!(":{b} #{t} @{label}")
                }
                _ => opts.to_string(),
            };
            format!("{} {opts}", bpe(phrase, &rank).join(" "))
        })
        .collect();
    // Every way the model may spell the name: one line per variant, same label.
    let lines: Vec<String> = lines
        .iter()
        .flat_map(|l| {
            if !l.contains("\u{2581}BE E") {
                return vec![l.clone()];
            }
            // The model hears "bee" as BE or B, also run into the word before ("HEYBE").
            ["\u{2581}BE E", "\u{2581}B E E", "\u{2581}BE", "B E", "\u{2581}B"]
                .iter()
                .map(|v| l.replace("\u{2581}BE E", v))
                .collect()
        })
        .collect();
    println!("{}", lines.join(" | "));
    Ok(lines.join("\n"))
}

fn spotter(int8: bool) -> Result<so::KeywordSpotter> {
    let q = if int8 { ".int8" } else { "" };
    let mut c = so::KeywordSpotterConfig::default();
    let m = |name: &str| model(&format!("{KWS}/{name}-epoch-12-avg-2-chunk-16-left-64{q}.onnx"));
    c.model_config.transducer.encoder = m("encoder");
    c.model_config.transducer.decoder = m("decoder");
    c.model_config.transducer.joiner = m("joiner");
    c.model_config.tokens = model(&format!("{KWS}/tokens.txt"));
    c.model_config.num_threads = 1;
    c.keywords_buf = Some(keywords_buf()?);
    so::KeywordSpotter::create(&c).context("create keyword spotter")
}

/// Keywords found in `samples`, with their start time.
fn spot(k: &so::KeywordSpotter, samples: &[f32]) -> Vec<(String, f32)> {
    let stream = k.create_stream();
    let mut found = Vec::new();
    let mut run = |chunk: &[f32]| {
        stream.accept_waveform(RATE, chunk);
        while k.is_ready(&stream) {
            k.decode(&stream);
            if let Some(r) = k.get_result(&stream) {
                if !r.keyword.is_empty() {
                    found.push((r.keyword.clone(), r.start_time));
                    k.reset(&stream);
                }
            }
        }
    };
    // Live audio always has lead-in; the files start right at the word.
    run(&[0.0; 8_000]);
    for chunk in samples.chunks(CHUNK) {
        run(chunk);
    }
    run(&[0.0; 16_000]);
    found
}

/// `kws [int8] [wav files]`: spots keywords in the wav files, or in every recorded clip
/// (where any hit is a false alarm).
fn kws(args: &[String]) -> Result<()> {
    let int8 = args.first().is_some_and(|a| a == "int8");
    let files = &args[usize::from(int8)..];
    let loaded = Instant::now();
    let k = spotter(int8)?;
    println!("loaded in {} ms (int8: {int8})", loaded.elapsed().as_millis());
    let mut total = Duration::ZERO;
    let mut audio = 0.0;
    if files.is_empty() {
        for clip in load_clips(cases()?.len())? {
            let samples = [clip.speech.as_slice(), clip.tail.as_slice()].concat();
            let start = Instant::now();
            let found = spot(&k, &samples);
            total += start.elapsed();
            audio += samples.len() as f32 / RATE as f32;
            println!("clip {:02}: {found:?}", clip.number);
        }
    } else {
        for file in files {
            let wave = so::Wave::read(file).context("read wav")?;
            let start = Instant::now();
            let found = spot(&k, wave.samples());
            total += start.elapsed();
            audio += wave.num_samples() as f32 / wave.sample_rate() as f32;
            println!("{file}: {found:?}");
        }
    }
    println!("real-time factor {:.3}", total.as_secs_f32() / audio);
    Ok(())
}

// ---- eval ----

struct Clip {
    number: usize,
    speech: Vec<f32>,
    tail: Vec<f32>,
}

/// Cuts each clip at the end of speech found by Silero VAD; the endpoint silence follows.
fn load_clips(count: usize) -> Result<Vec<Clip>> {
    let mut c = so::VadModelConfig::default();
    c.silero_vad.model = model("silero_vad.onnx");
    c.silero_vad.threshold = 0.5;
    c.silero_vad.min_silence_duration = 0.25;
    c.silero_vad.min_speech_duration = 0.2;
    c.silero_vad.window_size = 512;
    c.silero_vad.max_speech_duration = MAX_CLIP_S as f32;
    c.sample_rate = RATE;
    c.num_threads = 1;
    let vad = so::VoiceActivityDetector::create(&c, MAX_CLIP_S as f32 + 5.0).context("create VAD")?;

    let tail_len = (ENDPOINT_S * RATE as f32) as usize;
    let mut clips = Vec::new();
    for number in 1..=count {
        let path = clip_path(number);
        if !path.exists() {
            continue;
        }
        let Some(wave) = so::Wave::read(&path.to_string_lossy()) else {
            println!("missing clip {number}, skipped");
            continue;
        };
        let samples = wave.samples();
        vad.reset();
        for window in samples.chunks(512) {
            vad.accept_waveform(window);
        }
        vad.flush();
        let (mut start, mut end) = (usize::MAX, 0);
        while let Some(seg) = vad.front() {
            let s = usize::try_from(seg.start()).unwrap_or(0);
            start = start.min(s);
            end = end.max(s + seg.samples().len());
            vad.pop();
        }
        if end == 0 {
            println!("clip {number}: no speech found, skipped");
            continue;
        }
        // A live capture keeps a pre-roll buffer; streaming models drop the first words without it.
        let begin = start.saturating_sub(PRE_ROLL);
        let end = end.min(samples.len());
        let mut tail: Vec<f32> = samples[end..].iter().copied().take(tail_len).collect();
        tail.resize(tail_len, 0.0);
        clips.push(Clip { number, speech: samples[begin..end].to_vec(), tail });
    }
    Ok(clips)
}

fn eval(filter: Option<&str>) -> Result<()> {
    let config = Config::from_toml(include_str!("../../../config/config.example.toml"))?;
    let phrases = PhraseSet::default_en()?;
    let matcher = RuleMatcher::new(&phrases, &config)?;
    let words = vocabulary(&phrases, &config);
    let cases = cases()?;
    let clips = load_clips(cases.len())?;
    if clips.is_empty() {
        bail!("no clips yet, run: ./spike.ps1 record");
    }
    let audio_s: f32 = clips.iter().map(|c| c.speech.len() as f32 / RATE as f32).sum();
    println!("{} clips, {audio_s:.1} s of speech, endpoint wait {} ms\n", clips.len(), (ENDPOINT_S * 1000.0) as u32);

    let said = said()?;
    let mut detail = String::new();
    println!("{:<16} {:>7} {:>8} {:>8} {:>6} {:>9} {:>9} {:>6}", "engine", "load ms", "command", "exact", "WER", "p50 ms", "max ms", "RTF");
    for &name in ENGINES.iter().filter(|n| filter.is_none_or(|f| n.contains(f))) {
        let loaded = Instant::now();
        let mut engine = match load(name, &words) {
            Ok(e) => e,
            Err(e) => {
                println!("{name:<16} failed: {e:#}");
                continue;
            }
        };
        let load_ms = loaded.elapsed().as_millis();
        let (mut command_ok, mut exact_ok, mut after, mut compute) = (0, 0, Vec::new(), Duration::ZERO);
        let (mut errors, mut ref_words) = (0, 0);
        let _ = writeln!(detail, "== {name}");
        for clip in &clips {
            let case = &cases[clip.number - 1];
            let (text, tail_time, total) = engine.run(&clip.speech, &clip.tail);
            after.push(tail_time);
            compute += total;
            let got = matcher.best(&text);
            let want_slots: BTreeMap<_, _> = case.slots.iter().map(|(k, v)| (k.clone(), normalize(v))).collect();
            let got_command = got.as_ref().map(|i| i.command.clone());
            let got_slots: BTreeMap<_, _> = got
                .as_ref()
                .map(|i| i.slots.iter().map(|(k, v)| (k.clone(), normalize(v))).collect())
                .unwrap_or_default();
            let command = got_command == case.command;
            let exact = command && got_slots == want_slots;
            command_ok += usize::from(command);
            exact_ok += usize::from(exact);
            let mark = if exact { "ok  " } else if command { "slot" } else { "MISS" };
            let spoken = said.get(&clip.number).unwrap_or(&case.text);
            let wer = said.get(&clip.number).map(|want| {
                let (want, got) = (wer_words(want), wer_words(&text));
                let e = edits(&want, &got);
                errors += e;
                ref_words += want.len();
                format!("{e}/{}", want.len())
            });
            let _ = writeln!(
                detail,
                "{mark} {:02} {:>4} ms  errors {:>5}  said {:?}  heard {:?}  got {:?} {:?}",
                clip.number,
                tail_time.as_millis(),
                wer.unwrap_or_default(),
                spoken,
                text.trim(),
                got_command,
                got_slots
            );
        }
        after.sort();
        let ms = |d: Option<&Duration>| d.map_or(0, |d| d.as_millis() + (ENDPOINT_S * 1000.0) as u128);
        println!(
            "{name:<16} {load_ms:>7} {:>8} {:>8} {:>6} {:>9} {:>9} {:>6.2}",
            format!("{command_ok}/{}", clips.len()),
            format!("{exact_ok}/{}", clips.len()),
            if ref_words == 0 { "-".to_owned() } else { format!("{:.0}%", 100.0 * errors as f32 / ref_words as f32) },
            ms(after.get(after.len() / 2)),
            ms(after.last()),
            compute.as_secs_f32() / audio_s
        );
    }
    let out = root().join("clips").join("results.txt");
    std::fs::write(&out, &detail)?;
    println!("\nLatency = endpoint wait + compute after speech ends. Per-clip detail: {}", out.display());
    Ok(())
}

/// What was said per clip, from `clips/said.txt` lines `NN text`, when the clips are not
/// the scripted cases. Empty if the file is missing.
fn said() -> Result<BTreeMap<usize, String>> {
    let path = root().join("clips").join("said.txt");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    Ok(std::fs::read_to_string(path)?
        .lines()
        .filter_map(|l| {
            let (n, text) = l.trim().split_once(' ')?;
            Some((n.parse().ok()?, text.to_owned()))
        })
        .collect())
}

/// Lowercase words for scoring; punctuation splits words, digits up to 10 become words.
fn wer_words(text: &str) -> Vec<String> {
    const DIGITS: [&str; 11] =
        ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(|w| w.parse::<usize>().ok().and_then(|n| DIGITS.get(n)).map_or(w, |d| d).to_owned())
        .collect()
}

/// Word-level edit distance (substitutions, insertions, deletions).
fn edits(want: &[String], got: &[String]) -> usize {
    let mut row: Vec<usize> = (0..=got.len()).collect();
    for (i, w) in want.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, g) in got.iter().enumerate() {
            let next = (prev + usize::from(w != g)).min(row[j] + 1).min(row[j + 1] + 1);
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[got.len()]
}