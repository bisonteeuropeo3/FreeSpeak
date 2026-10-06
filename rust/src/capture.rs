//! Microphone capture and WAV encoding.
//!
//! Capture runs at whatever rate the device's shared-mode mix format uses
//! (usually 48 kHz) and is downmixed to mono, then resampled to the model's
//! native 16 kHz to keep the upload small and avoid server-side resampling.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Native sample rate of `grok-voice-transcribe-2.0`.
pub const TARGET_RATE: u32 = 16_000;

pub struct Capture {
    stream: cpal::Stream,
    buffer: Arc<Mutex<Vec<f32>>>,
    active: Arc<AtomicBool>,
    sample_rate: u32,
    device_name: String,
}

/// Names of all input devices, for `--devices`.
pub fn list_devices() -> Result<Vec<String>, String> {
    let host = cpal::default_host();
    let devices = host
        .input_devices()
        .map_err(|e| format!("enumerating input devices: {e}"))?;
    Ok(devices
        .map(|d| d.name().unwrap_or_else(|_| "<unnamed>".to_string()))
        .collect())
}

impl Capture {
    /// Must be called from the thread that will keep this value alive:
    /// `cpal::Stream` is not `Send` on Windows.
    pub fn new(device_match: &str) -> Result<Capture, String> {
        let host = cpal::default_host();

        let device = if device_match.trim().is_empty() {
            host.default_input_device()
                .ok_or_else(|| "no default input device found".to_string())?
        } else {
            let needle = device_match.trim().to_ascii_lowercase();
            let mut found = None;
            for candidate in host
                .input_devices()
                .map_err(|e| format!("enumerating input devices: {e}"))?
            {
                let name = candidate.name().unwrap_or_default();
                if name.to_ascii_lowercase().contains(&needle) {
                    found = Some(candidate);
                    break;
                }
            }
            found.ok_or_else(|| format!("no input device matching '{device_match}'"))?
        };

        let device_name = device.name().unwrap_or_else(|_| "<unnamed>".to_string());
        let supported = device
            .default_input_config()
            .map_err(|e| format!("querying the microphone format: {e}"))?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let active = Arc::new(AtomicBool::new(false));

        let on_error = move |err: cpal::StreamError| {
            crate::logging::line(&format!("audio stream error: {err}"));
        };

        let channels = config.channels as usize;

        macro_rules! build {
            ($sample:ty, $convert:expr) => {{
                let buffer = buffer.clone();
                let active = active.clone();
                let convert = $convert;
                device.build_input_stream(
                    &config,
                    move |data: &[$sample], _: &cpal::InputCallbackInfo| {
                        if !active.load(Ordering::Relaxed) {
                            return;
                        }
                        let mut sink = match buffer.lock() {
                            Ok(sink) => sink,
                            Err(_) => return,
                        };
                        if channels <= 1 {
                            sink.reserve(data.len());
                            for &sample in data {
                                sink.push(convert(sample));
                            }
                        } else {
                            sink.reserve(data.len() / channels + 1);
                            for frame in data.chunks(channels) {
                                let mut sum = 0.0f32;
                                for &sample in frame {
                                    sum += convert(sample);
                                }
                                sink.push(sum / channels as f32);
                            }
                        }
                    },
                    on_error,
                    None,
                )
            }};
        }

        let stream = match sample_format {
            cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
            cpal::SampleFormat::I16 => build!(i16, |s: i16| s as f32 / 32_768.0),
            cpal::SampleFormat::U16 => build!(u16, |s: u16| (s as f32 - 32_768.0) / 32_768.0),
            cpal::SampleFormat::I32 => build!(i32, |s: i32| s as f32 / 2_147_483_648.0),
            cpal::SampleFormat::F64 => build!(f64, |s: f64| s as f32),
            other => return Err(format!("unsupported microphone sample format: {other:?}")),
        }
        .map_err(|e| format!("opening the microphone: {e}"))?;

        Ok(Capture {
            stream,
            buffer,
            active,
            sample_rate: config.sample_rate.0,
            device_name,
        })
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Starts (or restarts) a recording, discarding anything buffered before.
    pub fn start(&self) -> Result<(), String> {
        if let Ok(mut buffer) = self.buffer.lock() {
            buffer.clear();
        }
        self.active.store(true, Ordering::SeqCst);
        self.stream
            .play()
            .map_err(|e| format!("starting the microphone: {e}"))
    }

    /// Stops the recording and returns the mono samples at `sample_rate()`.
    pub fn stop(&self) -> Vec<f32> {
        self.active.store(false, Ordering::SeqCst);
        let _ = self.stream.pause();
        match self.buffer.lock() {
            Ok(mut buffer) => std::mem::take(&mut *buffer),
            Err(_) => Vec::new(),
        }
    }
}

/// Root-mean-square level, used to skip silent recordings.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32_767.0) as i16
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Wraps mono samples in a 44-byte PCM WAV header, downsampling to 16 kHz when
/// the device runs faster than that.
///
/// Downsampling averages each source window rather than dropping samples, which
/// keeps the aliasing that would otherwise fold high frequencies into the speech
/// band out of the transcription. Devices slower than 16 kHz are left alone and
/// declared honestly in the header: the API accepts 8000-48000 Hz.
pub fn wav_from_mono(mono: &[f32], input_rate: u32) -> Vec<u8> {
    let (pcm, rate): (Vec<i16>, u32) = if input_rate == 0 || input_rate <= TARGET_RATE {
        let rate = if input_rate == 0 { TARGET_RATE } else { input_rate };
        (mono.iter().map(|&s| to_i16(s)).collect(), rate)
    } else {
        let ratio = input_rate as f64 / TARGET_RATE as f64;
        let out_len = (mono.len() as f64 / ratio).floor() as usize;
        let mut out = Vec::with_capacity(out_len);
        for index in 0..out_len {
            let start = ((index as f64 * ratio) as usize).min(mono.len() - 1);
            let end = (((index + 1) as f64 * ratio) as usize)
                .max(start + 1)
                .min(mono.len());
            let window = &mono[start..end];
            let mean = window.iter().sum::<f32>() / window.len() as f32;
            out.push(to_i16(mean));
        }
        (out, TARGET_RATE)
    };

    let data_len = (pcm.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + pcm.len() * 2);
    wav.extend_from_slice(b"RIFF");
    push_u32(&mut wav, 36 + data_len);
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    push_u32(&mut wav, 16); // PCM header size
    push_u16(&mut wav, 1); // PCM
    push_u16(&mut wav, 1); // mono
    push_u32(&mut wav, rate);
    push_u32(&mut wav, rate * 2); // byte rate
    push_u16(&mut wav, 2); // block align
    push_u16(&mut wav, 16); // bits per sample
    wav.extend_from_slice(b"data");
    push_u32(&mut wav, data_len);
    for sample in pcm {
        push_u16(&mut wav, sample as u16);
    }
    wav
}
