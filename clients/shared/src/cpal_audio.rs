//! Real audio backend using CPAL for mic capture and speaker playback.
//!
//! Gated behind the `real-backends` feature flag so tests and mocks do not
//! pull in platform audio dependencies. Buffer management, per-peer volumes,
//! and CPAL device selection live in sibling modules and are re-exported here
//! to preserve the existing public API.
//!
//! ## Mutex strategy
//! All internal `Mutex` guards recover poisoned state with
//! `.unwrap_or_else(|e| e.into_inner())` via the module-local `recover_lock()`
//! helper instead of panicking. The guarded values (`Option<String>`, `f32`,
//! `Vec<SendStream>`, `bool`) are simple enough that recovering the inner value
//! is safe and avoids cascading panics during CPAL callback teardown.

use crate::audio::{AudioBackend, AudioError, AudioTrack};
use crate::audio_pipeline::SAMPLE_RATE;
use crate::cpal_device::{find_input_device, find_output_device, input_config, output_config};
use crate::resampler::create_resampler;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::Stream;
use log::{error, info, warn};
use rubato::Resampler;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

pub use crate::audio_buffer::{
    AudioBuffer, BufferStats, DEFAULT_BUFFER_DURATION_MS, DEFAULT_MAX_MARGIN_MS,
    DEFAULT_PLAYBACK_BUFFER_DURATION_MS, DEFAULT_PLAYBACK_MAX_MARGIN_MS,
    DEFAULT_PLAYBACK_TARGET_OCCUPANCY_MS, DEFAULT_TARGET_OCCUPANCY_MS,
};
pub use crate::peer_volumes::{perceptual_gain, PeerVolumes, DEFAULT_VOLUME};

/// Wrapper around `cpal::Stream` to make it `Send`.
/// CPAL streams on Windows (WASAPI) are not `Send` due to COM threading,
/// but we only access them through a Mutex from the creating thread
/// (to drop them on stop). This is safe for our usage pattern.
struct SendStream(#[allow(dead_code)] Stream);

// SAFETY: We only store streams in a Mutex and drop them from the same
// runtime. The streams themselves are not moved across threads — only
// the Vec holding them is behind Arc<Mutex<>>.
unsafe impl Send for SendStream {}

fn recover_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Real audio backend using CPAL.
///
/// On `capture_mic()`, opens the default input device at 48kHz mono f32
/// and starts writing samples into the shared `AudioBuffer`.
///
/// On `play_remote()`, opens the default output device at 48kHz mono f32
/// and starts reading from a separate playback `AudioBuffer`.
///
/// The `AudioBuffer` handles are exposed so the real `PeerConnectionBackend`
/// can read captured samples and write received samples.
pub struct CpalAudioBackend {
    /// Buffer that mic samples are written into (read by WebRTC send path)
    pub capture_buffer: AudioBuffer,
    /// Buffer that remote audio is written into (read by speaker output)
    pub playback_buffer: AudioBuffer,
    /// Active CPAL streams — kept alive so audio flows
    streams: Arc<Mutex<Vec<SendStream>>>,
    /// Track whether we're currently capturing
    active: Arc<Mutex<bool>>,
    /// User-selected input device name (CPAL device name, no prefix). None = use OS default.
    input_device_name: Mutex<Option<String>>,
    /// User-selected output device name (CPAL device name, no prefix). None = use OS default.
    output_device_name: Mutex<Option<String>>,
    /// Input gain multiplier (0.0–1.0). Shared with the CPAL capture callback.
    input_gain: Arc<Mutex<f32>>,
}

impl Default for CpalAudioBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CpalAudioBackend {
    pub fn new() -> Self {
        Self {
            capture_buffer: AudioBuffer::new(DEFAULT_BUFFER_DURATION_MS),
            playback_buffer: AudioBuffer::with_target(
                DEFAULT_PLAYBACK_BUFFER_DURATION_MS,
                DEFAULT_PLAYBACK_TARGET_OCCUPANCY_MS,
                DEFAULT_PLAYBACK_MAX_MARGIN_MS,
            ),
            streams: Arc::new(Mutex::new(Vec::new())),
            active: Arc::new(Mutex::new(false)),
            input_device_name: Mutex::new(None),
            output_device_name: Mutex::new(None),
            input_gain: Arc::new(Mutex::new(1.0)),
        }
    }

    /// Set the preferred input device by CPAL device name (no "input:" prefix).
    /// Takes effect on the next `capture_mic()` call.
    pub fn set_input_device_name(&self, name: Option<String>) {
        *recover_lock(&self.input_device_name) = name;
    }

    /// Set the preferred output device by CPAL device name (no "output:" prefix).
    /// Takes effect on the next `play_remote()` call.
    pub fn set_output_device_name(&self, name: Option<String>) {
        *recover_lock(&self.output_device_name) = name;
    }

    /// Set the microphone input gain multiplier (0.0–1.0). Takes effect immediately
    /// on the next CPAL callback — no stream restart needed.
    pub fn set_input_gain(&self, gain: f32) {
        *recover_lock(&self.input_gain) = gain.clamp(0.0, 1.0);
    }
}

impl AudioBackend for CpalAudioBackend {
    fn capture_mic(&self) -> Result<AudioTrack, AudioError> {
        let preferred_name = recover_lock(&self.input_device_name).clone();
        let device = find_input_device(preferred_name.as_deref())?;
        let config = input_config(&device);
        let buffer = self.capture_buffer.clone();
        let device_rate = config.sample_rate.0;

        // Tell the buffer how many channels CPAL is producing so it
        // can downmix to mono on write.
        self.capture_buffer.set_write_channels(config.channels);

        info!("Opening mic: {:?}", device.name().unwrap_or_default());

        // If device rate ≠ 48kHz, create a resampler (device_rate → 48kHz).
        let resampler = if device_rate != SAMPLE_RATE {
            match create_resampler(device_rate, SAMPLE_RATE) {
                Ok(r) => {
                    info!("Capture resampler: {}Hz → {}Hz", device_rate, SAMPLE_RATE);
                    Some(Arc::new(Mutex::new(r)))
                }
                Err(e) => {
                    warn!("Failed to create capture resampler: {e}, using raw samples");
                    None
                }
            }
        } else {
            None
        };

        let in_channels = config.channels as usize;
        let input_gain = Arc::clone(&self.input_gain);

        // CPAL callback chunk sizes are variable, while SincFixedIn expects
        // fixed-size input chunks (`input_frames_next`). Queue mono samples
        // across callbacks and feed the resampler in exact chunk sizes.
        let mut pending_mono = VecDeque::<f32>::new();

        let stream = device
            .build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let gain = *recover_lock(&input_gain);
                    if let Some(ref resampler) = resampler {
                        // Downmix to mono first, then queue for fixed-size resampling.
                        pending_mono.extend(data.chunks_exact(in_channels).map(|frame| {
                            let sum: f32 = frame.iter().sum();
                            sum / in_channels as f32
                        }));

                        if let Ok(mut r) = resampler.lock() {
                            let needed = r.input_frames_next();

                            while pending_mono.len() >= needed {
                                let mut chunk = vec![0.0f32; needed];
                                for sample in &mut chunk {
                                    if let Some(v) = pending_mono.pop_front() {
                                        *sample = v;
                                    }
                                }

                                let input_frames = vec![chunk];
                                match r.process(&input_frames, None) {
                                    Ok(output) => {
                                        if let Some(resampled) = output.first() {
                                            if (gain - 1.0).abs() > 0.001 {
                                                let gained: Vec<f32> =
                                                    resampled.iter().map(|s| s * gain).collect();
                                                buffer.write_mono(&gained);
                                            } else {
                                                // Write already-mono resampled data directly.
                                                buffer.write_mono(resampled);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Capture resampler process failed: {e}");
                                        break;
                                    }
                                }
                            }

                            // Guardrail: if producer outruns consumer briefly,
                            // keep queue bounded to avoid latency blow-up.
                            let max_pending = needed * 8;
                            while pending_mono.len() > max_pending {
                                let _ = pending_mono.pop_front();
                            }
                        }
                    } else {
                        // Device is already 48kHz — write directly (buffer handles downmix).
                        if (gain - 1.0).abs() > 0.001 {
                            let gained: Vec<f32> = data.iter().map(|s| s * gain).collect();
                            buffer.write(&gained);
                        } else {
                            buffer.write(data);
                        }
                    }
                },
                move |err| {
                    error!("Mic capture error: {}", err);
                },
                None,
            )
            .map_err(|e| AudioError::Other(format!("Failed to build input stream: {e}")))?;

        stream
            .play()
            .map_err(|e| AudioError::Other(format!("Failed to start mic: {e}")))?;

        recover_lock(&self.streams).push(SendStream(stream));
        *recover_lock(&self.active) = true;

        Ok(AudioTrack {
            id: "cpal-mic".to_string(),
        })
    }

    fn play_remote(&self, _track: AudioTrack) -> Result<(), AudioError> {
        let preferred_name = recover_lock(&self.output_device_name).clone();
        let device = find_output_device(preferred_name.as_deref())?;
        let config = output_config(&device);
        let buffer = self.playback_buffer.clone();
        let out_channels = config.channels as usize;
        let device_rate = config.sample_rate.0;

        info!("Opening speaker: {:?}", device.name().unwrap_or_default());

        // If device rate ≠ 48kHz, create a resampler (48kHz → device_rate).
        let resampler = if device_rate != SAMPLE_RATE {
            match create_resampler(SAMPLE_RATE, device_rate) {
                Ok(r) => {
                    info!("Playback resampler: {}Hz → {}Hz", SAMPLE_RATE, device_rate);
                    Some(Arc::new(Mutex::new(r)))
                }
                Err(e) => {
                    warn!("Failed to create playback resampler: {e}, using raw samples");
                    None
                }
            }
        } else {
            None
        };
        // Persist resampled output across callbacks so variable callback sizes
        // don't force repeated drops/restarts of partially generated audio.
        let mut pending_out = VecDeque::<f32>::new();

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if let Some(ref resampler) = resampler {
                        // Read mono 48kHz from buffer, resample to device rate,
                        // then duplicate across output channels. Keep a small
                        // pending queue so each callback can be fully satisfied.
                        const MAX_SPIN_CHUNKS: usize = 4;
                        if let Ok(mut r) = resampler.lock() {
                            let out_frames = data.len() / out_channels;

                            // Generate enough resampled samples to satisfy this callback.
                            let mut chunks = 0usize;
                            while pending_out.len() < out_frames && chunks < MAX_SPIN_CHUNKS {
                                let needed = r.input_frames_next();
                                let mut mono_buf = vec![0.0f32; needed];
                                let read = buffer.read(&mut mono_buf);
                                if read < needed {
                                    mono_buf[read..].fill(0.0);
                                }

                                let input_frames = vec![mono_buf];
                                match r.process(&input_frames, None) {
                                    Ok(output) => {
                                        if let Some(resampled) = output.first() {
                                            pending_out.extend(resampled.iter().copied());
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Playback resampler process failed: {e}");
                                        break;
                                    }
                                }
                                chunks += 1;
                            }

                            // Write exactly what the output callback asked for.
                            for frame in 0..out_frames {
                                let sample =
                                    pending_out.pop_front().unwrap_or(0.0).clamp(-0.98, 0.98);
                                for ch in 0..out_channels {
                                    data[frame * out_channels + ch] = sample;
                                }
                            }
                            return;
                        }
                        // Fallback: silence on resampler error.
                        data.fill(0.0);
                    } else {
                        // Device is already 48kHz — read directly.
                        let frames = data.len() / out_channels;
                        let mut mono_buf = vec![0.0f32; frames];
                        let read = buffer.read(&mut mono_buf);

                        for frame in 0..frames {
                            let sample = if frame < read { mono_buf[frame] } else { 0.0 };
                            let sample = sample.clamp(-0.98, 0.98);
                            for ch in 0..out_channels {
                                data[frame * out_channels + ch] = sample;
                            }
                        }
                    }
                },
                move |err| {
                    error!("Speaker playback error: {}", err);
                },
                None,
            )
            .map_err(|e| AudioError::Other(format!("Failed to build output stream: {e}")))?;

        stream
            .play()
            .map_err(|e| AudioError::Other(format!("Failed to start speaker: {e}")))?;

        recover_lock(&self.streams).push(SendStream(stream));

        Ok(())
    }

    fn stop(&self) -> Result<(), AudioError> {
        // Drop all streams — CPAL stops them on drop
        let mut streams = recover_lock(&self.streams);
        streams.clear();
        *recover_lock(&self.active) = false;
        info!("Audio stopped, all streams released");
        Ok(())
    }
}
