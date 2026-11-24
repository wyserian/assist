//! Microphone capture on a dedicated thread, converted to 16 kHz mono.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use sherpa_onnx as so;

use crate::{VoiceError, RATE};

/// Chunks buffered between the audio callback, the converter and the reader. At the usual
/// 10 ms callback this is about 0.6 s; when the reader falls further behind, chunks are
/// dropped and counted instead of growing memory.
const QUEUE: usize = 64;

/// The names of the input devices, for choosing one in config.
pub fn input_devices() -> Result<Vec<String>, VoiceError> {
    let devices = cpal::default_host()
        .input_devices()
        .map_err(|e| VoiceError::Device(e.to_string()))?;
    Ok(devices.map(|d| d.to_string()).collect())
}

/// An open microphone. Audio is read and converted on its own thread; [`Mic::recv`] returns
/// 16 kHz mono chunks. Dropping it stops the thread and closes the device.
pub struct Mic {
    name: String,
    chunks: Receiver<Vec<f32>>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

/// State the capture thread and the audio callback report through.
#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    dropped: AtomicU64,
    error: Mutex<Option<String>>,
}

impl Shared {
    fn fail(&self, message: String) {
        if let Ok(mut error) = self.error.lock() {
            error.get_or_insert(message);
        }
    }
}

impl Mic {
    /// Opens the input device whose name contains `device` (case-insensitive), or the
    /// system default.
    pub fn open(device: Option<&str>) -> Result<Self, VoiceError> {
        let shared = Arc::new(Shared::default());
        let (chunk_tx, chunks) = mpsc::sync_channel(QUEUE);
        let (ready_tx, ready) = mpsc::sync_channel(1);
        let device = device.map(str::to_lowercase);
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("assist-mic".into())
            .spawn(move || capture(device.as_deref(), &thread_shared, &chunk_tx, &ready_tx))
            .map_err(|e| VoiceError::Device(e.to_string()))?;
        let name = ready
            .recv()
            .map_err(|_| VoiceError::Device("capture thread stopped".into()))??;
        Ok(Self {
            name,
            chunks,
            shared,
            thread: Some(thread),
        })
    }

    /// The device name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The next chunk, or `None` if none arrives within `timeout`. Fails once the device
    /// reports an error or disappears.
    pub fn recv(&self, timeout: Duration) -> Result<Option<Vec<f32>>, VoiceError> {
        match self.chunks.recv_timeout(timeout) {
            Ok(chunk) => Ok(Some(chunk)),
            Err(RecvTimeoutError::Timeout) => match self.error() {
                Some(message) => Err(VoiceError::Device(message)),
                None => Ok(None),
            },
            Err(RecvTimeoutError::Disconnected) => Err(VoiceError::Device(
                self.error().unwrap_or_else(|| "capture stopped".into()),
            )),
        }
    }

    /// Chunks dropped because the reader was too slow.
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    fn error(&self) -> Option<String> {
        self.shared.error.lock().ok().and_then(|e| e.clone())
    }
}

impl Drop for Mic {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Body of the capture thread: owns the stream (not `Send` on every platform) and the
/// resampler, and runs until [`Mic`] is dropped or the device fails.
fn capture(
    wanted: Option<&str>,
    shared: &Arc<Shared>,
    out: &SyncSender<Vec<f32>>,
    ready: &SyncSender<Result<String, VoiceError>>,
) {
    let opened = open_stream(wanted, shared);
    let (stream, raw, resampler, name) = match opened {
        Ok(parts) => parts,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(name));

    while !shared.stop.load(Ordering::Relaxed) {
        let chunk = match raw.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => chunk,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let chunk = match &resampler {
            Some(r) => r.resample(&chunk, false),
            None => chunk,
        };
        if chunk.is_empty() {
            continue;
        }
        match out.try_send(chunk) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                shared.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => break,
        }
    }
    drop(stream);
}

type Opened = (
    cpal::Stream,
    Receiver<Vec<f32>>,
    Option<so::LinearResampler>,
    String,
);

fn open_stream(wanted: Option<&str>, shared: &Arc<Shared>) -> Result<Opened, VoiceError> {
    let host = cpal::default_host();
    let device = match wanted {
        None => host.default_input_device().ok_or(VoiceError::NoDevice)?,
        Some(wanted) => host
            .input_devices()
            .map_err(|e| VoiceError::Device(e.to_string()))?
            .find(|d| d.to_string().to_lowercase().contains(wanted))
            .ok_or_else(|| VoiceError::DeviceNotFound(wanted.to_owned()))?,
    };
    let name = device.to_string();
    let config = device
        .default_input_config()
        .map_err(|e| VoiceError::Device(e.to_string()))?;
    let channels = usize::from(config.channels()).max(1);
    let rate = config.sample_rate();
    let format = config.sample_format();
    let (raw_tx, raw) = mpsc::sync_channel::<Vec<f32>>(QUEUE);

    let callback_shared = Arc::clone(shared);
    let error_shared = Arc::clone(shared);
    let on_error = move |e: cpal::Error| error_shared.fail(e.to_string());
    // Downmixes one callback's frames and queues them; never blocks the audio thread.
    // shortcut: allocates one Vec per callback, use a lock-free ring buffer if dropouts show up.
    let send = move |mono: Vec<f32>| {
        if let Err(TrySendError::Full(_)) = raw_tx.try_send(mono) {
            callback_shared.dropped.fetch_add(1, Ordering::Relaxed);
        }
    };
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config.config(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                send(data.chunks(channels).map(mean).collect());
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            config.config(),
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                send(
                    data.chunks(channels)
                        .map(|f| {
                            f.iter().map(|&s| f32::from(s) / 32_768.0).sum::<f32>() / f.len() as f32
                        })
                        .collect(),
                );
            },
            on_error,
            None,
        ),
        other => return Err(VoiceError::Format(other.to_string())),
    }
    .map_err(|e| VoiceError::Device(e.to_string()))?;
    stream
        .play()
        .map_err(|e| VoiceError::Device(e.to_string()))?;

    let resampler = if rate == RATE {
        None
    } else {
        Some(
            so::LinearResampler::create(rate as i32, RATE as i32)
                .ok_or(VoiceError::Engine("resampler"))?,
        )
    };
    Ok((stream, raw, resampler, name))
}

fn mean(frame: &[f32]) -> f32 {
    frame.iter().sum::<f32>() / frame.len() as f32
}
