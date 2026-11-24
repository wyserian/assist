//! Cuts a continuous 16 kHz stream into utterances.

use std::collections::VecDeque;
use std::time::Duration;

use crate::vad::{Detector, VadSettings};
use crate::RATE;

/// Samples per detector decision (32 ms), the window Silero expects at 16 kHz.
pub(crate) const WINDOW: usize = 512;

/// Audio kept from before speech is detected. Streaming recognisers drop the first word
/// without about a second of lead-in (Phase 2 spike).
pub const PRE_ROLL: Duration = Duration::from_secs(1);

/// What [`Segmenter::push`] reports, in order: `Start`, one or more `Audio`, `End`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event<'a> {
    /// An utterance begins; the pre-roll follows as `Audio`.
    Start,
    /// Samples of the current utterance, 16 kHz mono.
    Audio(&'a [f32]),
    /// The utterance ended: silence for `min_silence`, or `max_utterance` reached. The
    /// trailing silence has already been sent as `Audio`.
    End,
}

/// Groups audio into utterances with a [`Detector`]. Memory is bounded by the pre-roll and
/// one partial window; utterance audio is streamed out, not stored.
pub struct Segmenter<D> {
    detector: D,
    /// Samples not yet judged, fewer than one window after each push.
    pending: Vec<f32>,
    /// The most recent audio while nobody speaks.
    pre_roll: VecDeque<f32>,
    pre_roll_cap: usize,
    speaking: bool,
    spoken: usize,
    max_utterance: usize,
}

impl<D: Detector> Segmenter<D> {
    /// The pre-roll covers [`PRE_ROLL`] plus the detector's `min_speech` lag.
    pub fn new(detector: D, settings: &VadSettings) -> Self {
        let pre_roll_cap = samples(PRE_ROLL + settings.min_speech).max(WINDOW);
        Self {
            detector,
            pending: Vec::with_capacity(2 * WINDOW),
            pre_roll: VecDeque::with_capacity(pre_roll_cap + WINDOW),
            pre_roll_cap,
            speaking: false,
            spoken: 0,
            max_utterance: samples(settings.max_utterance).max(WINDOW),
        }
    }

    /// Whether an utterance is in progress.
    pub fn is_speaking(&self) -> bool {
        self.speaking
    }

    /// Feeds 16 kHz mono samples of any length; `on` gets the events they complete.
    pub fn push(&mut self, samples: &[f32], mut on: impl FnMut(Event<'_>)) {
        self.pending.extend_from_slice(samples);
        let mut used = 0;
        while self.pending.len() - used >= WINDOW {
            let window = &self.pending[used..used + WINDOW];
            used += WINDOW;
            let speech = self.detector.is_speech(window);
            if self.speaking {
                on(Event::Audio(window));
                self.spoken += WINDOW;
                if !speech || self.spoken >= self.max_utterance {
                    if speech {
                        self.detector.reset();
                    }
                    self.speaking = false;
                    on(Event::End);
                }
            } else if speech {
                self.speaking = true;
                self.spoken = WINDOW;
                on(Event::Start);
                let (front, back) = self.pre_roll.as_slices();
                for part in [front, back].into_iter().filter(|p| !p.is_empty()) {
                    on(Event::Audio(part));
                }
                self.pre_roll.clear();
                on(Event::Audio(window));
            } else {
                self.pre_roll.extend(window);
                let excess = self.pre_roll.len().saturating_sub(self.pre_roll_cap);
                self.pre_roll.drain(..excess);
            }
        }
        self.pending.drain(..used);
    }
}

fn samples(duration: Duration) -> usize {
    (duration.as_secs_f64() * f64::from(RATE)) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Speech wherever the window's first sample is 1.0.
    struct Loud {
        resets: usize,
    }

    impl Detector for Loud {
        fn is_speech(&mut self, window: &[f32]) -> bool {
            window[0] == 1.0
        }
        fn reset(&mut self) {
            self.resets += 1;
        }
    }

    fn settings(max_utterance_windows: usize) -> VadSettings {
        VadSettings {
            min_speech: Duration::ZERO,
            max_utterance: Duration::from_secs_f64(
                (max_utterance_windows * WINDOW) as f64 / f64::from(RATE),
            ),
            ..VadSettings::default()
        }
    }

    /// Runs windows of silence (0.0) or speech (1.0), fed in uneven chunks; returns events
    /// as `S`, `E` and the audio length between them.
    fn run(windows: &[bool], max_utterance_windows: usize) -> (Vec<String>, usize) {
        let mut seg = Segmenter::new(Loud { resets: 0 }, &settings(max_utterance_windows));
        let audio: Vec<f32> = windows
            .iter()
            .flat_map(|&s| std::iter::repeat_n(if s { 1.0 } else { 0.0 }, WINDOW))
            .collect();
        let mut events = Vec::new();
        let mut audio_len = 0;
        for chunk in audio.chunks(300) {
            seg.push(chunk, |e| match e {
                Event::Start => events.push("S".to_owned()),
                Event::Audio(a) => audio_len += a.len(),
                Event::End => events.push(format!("E{audio_len}")),
            });
        }
        (events, seg.detector.resets)
    }

    #[test]
    fn utterance_carries_pre_roll_and_trailing_window() {
        // 40 silent windows (1.3 s) is more than the pre-roll keeps.
        let mut windows = vec![false; 40];
        windows.extend([true; 3]);
        windows.push(false);
        let (events, _) = run(&windows, 100);
        // Pre-roll, 3 speech windows, the silent window that ended it.
        let expected = samples(PRE_ROLL) + 4 * WINDOW;
        assert_eq!(events, ["S".to_owned(), format!("E{expected}")]);
    }

    #[test]
    fn short_lead_in_sends_what_there_is() {
        let (events, _) = run(&[false, false, true, false], 100);
        assert_eq!(events, ["S".to_owned(), format!("E{}", 4 * WINDOW)]);
    }

    #[test]
    fn long_speech_is_cut_and_detector_reset() {
        let (events, resets) = run(&[true; 6], 3);
        assert_eq!(
            events,
            [
                "S".to_owned(),
                format!("E{}", 3 * WINDOW),
                "S".to_owned(),
                format!("E{}", 6 * WINDOW),
            ]
        );
        assert_eq!(resets, 2);
    }

    #[test]
    fn silence_only_emits_nothing_and_stays_bounded() {
        let mut seg = Segmenter::new(Loud { resets: 0 }, &settings(10));
        let mut events = 0;
        for _ in 0..1000 {
            seg.push(&[0.0; WINDOW], |_| events += 1);
        }
        assert_eq!(events, 0);
        assert!(seg.pre_roll.len() <= seg.pre_roll_cap);
        assert!(seg.pending.is_empty());
    }
}
