//! Screen capture with xcap and text recognition with the OCR built into Windows (WinRT
//! `Windows.Media.Ocr`, safe calls). Blocking; the actions run these off the async runtime.

use std::path::Path;

use windows::core::HSTRING;
use windows::Graphics::Imaging::{BitmapDecoder, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Storage::{FileAccessMode, StorageFile};

/// Saves the primary monitor as a PNG.
pub fn screenshot(path: &Path) -> Result<(), String> {
    capture()?
        .save(path)
        .map_err(|err| format!("cannot save {}: {err}", path.display()))
}

/// The text on the primary monitor, one line each.
pub fn ocr_screen() -> Result<Vec<String>, String> {
    let image = capture()?;
    let (width, height) = image.dimensions();
    let buffer = CryptographicBuffer::CreateFromByteArray(image.as_raw()).map_err(error)?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
        &buffer,
        BitmapPixelFormat::Rgba8,
        i32::try_from(width).map_err(error)?,
        i32::try_from(height).map_err(error)?,
    )
    .map_err(error)?;
    recognize(&bitmap)
}

/// The text in an image file (PNG, JPEG, BMP and others), one line each.
pub fn ocr_file(path: &Path) -> Result<Vec<String>, String> {
    let open = || -> windows::core::Result<SoftwareBitmap> {
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str()))?.join()?;
        let stream = file.OpenAsync(FileAccessMode::Read)?.join()?;
        BitmapDecoder::CreateAsync(&stream)?
            .join()?
            .GetSoftwareBitmapAsync()?
            .join()
    };
    let bitmap =
        open().map_err(|err| format!("cannot read {}: {}", path.display(), err.message()))?;
    recognize(&bitmap)
}

fn capture() -> Result<xcap::image::RgbaImage, String> {
    let monitors = xcap::Monitor::all().map_err(error)?;
    let primary = monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .ok_or("no monitor found")?;
    primary.capture_image().map_err(error)
}

fn recognize(bitmap: &SoftwareBitmap) -> Result<Vec<String>, String> {
    let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(|_| {
        "no OCR language is installed (Windows Settings, Time and language, Language)".to_owned()
    })?;
    let bitmap = SoftwareBitmap::Convert(bitmap, BitmapPixelFormat::Bgra8).map_err(error)?;
    let result = engine
        .RecognizeAsync(&bitmap)
        .map_err(error)?
        .join()
        .map_err(error)?;
    let lines = result.Lines().map_err(error)?;
    lines
        .into_iter()
        .map(|line| line.Text().map(|text| text.to_string()).map_err(error))
        .collect()
}

fn error(err: impl std::fmt::Display) -> String {
    err.to_string()
}
