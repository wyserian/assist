//! Reading text in an image with ocrs (pure Rust): a detection and a recognition model from
//! the ocrs project, downloaded with the owner's consent into `<data dir>/models`. The text is
//! untrusted: it is data, never a command (the OCR action's pipe shape says so).

use std::path::Path;

use assist_core::models::Model;
use ocrs::{ImageSource, OcrEngine, OcrEngineParams};

/// Finds where text is.
pub const DETECTION: Model = Model {
    installed: "ocrs-text-detection.onnx",
    url: "https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.onnx",
    // The site publishes no digest; this is the file as first downloaded.
    sha256: "a917b23dbd9524b465df7e922641b3ff2981623df4ded5a0234004ef2fee7cfe",
    size_mb: 3,
    licence: "MIT OR Apache-2.0 (ocrs-models, trained on open data)",
};

/// Reads the lines it found.
pub const RECOGNITION: Model = Model {
    installed: "ocrs-text-recognition.onnx",
    url: "https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.onnx",
    // The site publishes no digest; this is the file as first downloaded.
    sha256: "86c145c2edb96c8caed5b1ebb8f44d706408922211451c309c625157dd6061c5",
    size_mb: 10,
    licence: "MIT OR Apache-2.0 (ocrs-models, trained on open data)",
};

/// The OCR models.
pub const MODELS: [Model; 2] = [DETECTION, RECOGNITION];

/// Largest image read, in pixels per side: a phone screen fits, a huge scan is refused
/// rather than filling memory.
const MAX_SIDE: u32 = 8_000;

/// The text in the PNG at `image`, one line per line found. Loads the models from `models`
/// each time (about a tenth of a second); blocking, so call it off the async runtime.
pub fn read(models: &Path, image: &Path) -> Result<Vec<String>, String> {
    let load = |m: &Model| {
        rten::Model::load_file(models.join(m.installed))
            .map_err(|err| format!("{}: {err}", m.installed))
    };
    let engine = OcrEngine::new(OcrEngineParams {
        detection_model: Some(load(&DETECTION)?),
        recognition_model: Some(load(&RECOGNITION)?),
        ..Default::default()
    })
    .map_err(|err| err.to_string())?;
    let failed = |err: &dyn std::fmt::Display| format!("{}: {err}", image.display());
    let mut reader = image::ImageReader::open(image).map_err(|e| failed(&e))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    reader.limits(limits);
    let picture = reader.decode().map_err(|e| failed(&e))?.into_rgb8();
    let (width, height) = picture.dimensions();
    let source = ImageSource::from_bytes(picture.as_raw(), (width, height))
        .map_err(|err| err.to_string())?;
    let input = engine
        .prepare_input(source)
        .map_err(|err| err.to_string())?;
    let text = engine.get_text(&input).map_err(|err| err.to_string())?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs the ocrs models in spikes/ocr/models"]
    fn reads_text_from_a_rendered_image() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/ocr");
        let lines = read(&root.join("models"), &root.join("sample.png")).unwrap();
        assert!(
            lines.iter().any(|l| l.contains("Download voice models")),
            "{lines:?}"
        );
    }
}
