//! Speaker verification: a voiceprint (speaker embedding) of the owner, compared with each
//! utterance. A voiceprint is biometric data: it stays in the data directory, user-only.

use std::path::Path;

use sherpa_onnx as so;

use crate::{VoiceError, RATE};

/// First line of a saved voiceprint, with the format version.
const HEADER: &str = "assist voiceprint 1";

/// Turns speech into voiceprints.
pub struct SpeakerModel {
    inner: so::SpeakerEmbeddingExtractor,
    name: String,
}

impl SpeakerModel {
    /// Loads a speaker embedding model file ([`crate::SPEAKER_MODEL`]).
    pub fn new(path: &Path) -> Result<Self, VoiceError> {
        if !path.is_file() {
            return Err(VoiceError::Model(path.to_owned()));
        }
        let config = so::SpeakerEmbeddingExtractorConfig {
            model: Some(path.to_string_lossy().into_owned()),
            num_threads: 1,
            ..Default::default()
        };
        let inner = so::SpeakerEmbeddingExtractor::create(&config)
            .ok_or(VoiceError::Engine("speaker model"))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Self { inner, name })
    }

    /// The voiceprint of one utterance (16 kHz), or `None` when it is too short to tell.
    pub fn voiceprint(&self, samples: &[f32]) -> Option<Voiceprint> {
        let stream = self.inner.create_stream()?;
        stream.accept_waveform(RATE as i32, samples);
        stream.input_finished();
        if !self.inner.is_ready(&stream) {
            return None;
        }
        Some(Voiceprint::new(
            self.name.clone(),
            self.inner.compute(&stream)?,
        ))
    }
}

/// A unit-length speaker embedding and the model that made it.
#[derive(Debug, Clone, PartialEq)]
pub struct Voiceprint {
    model: String,
    values: Vec<f32>,
}

impl Voiceprint {
    fn new(model: String, mut values: Vec<f32>) -> Self {
        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            values.iter_mut().for_each(|v| *v /= norm);
        }
        Self { model, values }
    }

    /// The average of several voiceprints from the same model, e.g. the enrolment
    /// utterances; `None` if there are none or they come from different models.
    pub fn average(prints: &[Voiceprint]) -> Option<Self> {
        let first = prints.first()?;
        let mut sum = vec![0.0; first.values.len()];
        for print in prints {
            if print.model != first.model || print.values.len() != sum.len() {
                return None;
            }
            sum.iter_mut().zip(&print.values).for_each(|(s, v)| *s += v);
        }
        Some(Self::new(first.model.clone(), sum))
    }

    /// Cosine similarity, -1 to 1; 0 when the models differ.
    pub fn score(&self, other: &Voiceprint) -> f32 {
        if self.model != other.model {
            return 0.0;
        }
        self.values
            .iter()
            .zip(&other.values)
            .map(|(a, b)| a * b)
            .sum()
    }

    /// Text form for the voiceprint file.
    pub fn to_text(&self) -> String {
        let values: Vec<String> = self.values.iter().map(f32::to_string).collect();
        format!("{HEADER}\n{}\n{}\n", self.model, values.join(" "))
    }

    /// Parses [`Voiceprint::to_text`]; `None` if the text is not a voiceprint.
    pub fn from_text(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let model = lines.next()?.to_owned();
        let values = lines
            .next()?
            .split(' ')
            .map(|v| v.parse::<f32>().ok().filter(|v| v.is_finite()))
            .collect::<Option<Vec<f32>>>()?;
        (!values.is_empty()).then_some(Self { model, values })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn print(values: &[f32]) -> Voiceprint {
        Voiceprint::new("m".to_owned(), values.to_vec())
    }

    #[test]
    fn score_is_cosine_of_unit_vectors() {
        let a = print(&[3.0, 4.0]);
        assert!((a.score(&a) - 1.0).abs() < 1e-6);
        assert!(a.score(&print(&[-3.0, -4.0])) < -0.99);
        let other_model = Voiceprint::new("n".to_owned(), vec![3.0, 4.0]);
        assert_eq!(a.score(&other_model), 0.0);
    }

    #[test]
    fn average_points_between_its_parts() {
        let avg = Voiceprint::average(&[print(&[1.0, 0.0]), print(&[0.0, 1.0])]).unwrap();
        assert!((avg.score(&print(&[1.0, 1.0])) - 1.0).abs() < 1e-6);
        assert!(Voiceprint::average(&[]).is_none());
    }

    #[test]
    fn text_round_trips_and_rejects_junk() {
        let a = print(&[0.6, 0.8]);
        assert_eq!(Voiceprint::from_text(&a.to_text()), Some(a));
        assert!(Voiceprint::from_text("assist voiceprint 1\nm\n0.5 nan\n").is_none());
        assert!(Voiceprint::from_text("hello").is_none());
    }
}
