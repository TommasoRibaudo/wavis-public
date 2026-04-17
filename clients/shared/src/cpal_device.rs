//! Owns CPAL device enumeration and stream config selection.
//!
//! This module does not own stream lifecycle, audio buffering, or per-peer
//! volume state. Those concerns live in `cpal_audio`, `audio_buffer`, and
//! `peer_volumes` respectively.

use crate::audio::AudioError;
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{Device, StreamConfig};
use log::{info, warn};

/// Find the preferred input device by name, falling back to the OS default.
pub(crate) fn find_input_device(preferred_name: Option<&str>) -> Result<Device, AudioError> {
    if let Some(name) = preferred_name {
        let host = cpal::default_host();
        if let Ok(mut devices) = host.input_devices() {
            if let Some(device) = devices.find(|d| d.name().ok().as_deref() == Some(name)) {
                info!("Using selected input device: {name}");
                return Ok(device);
            }
            warn!("Selected input device '{name}' not found, falling back to OS default");
        }
    }

    cpal::default_host()
        .default_input_device()
        .ok_or(AudioError::MicrophoneDenied)
}

/// Find the preferred output device by name, falling back to the OS default.
pub(crate) fn find_output_device(preferred_name: Option<&str>) -> Result<Device, AudioError> {
    if let Some(name) = preferred_name {
        let host = cpal::default_host();
        if let Ok(mut devices) = host.output_devices() {
            if let Some(device) = devices.find(|d| d.name().ok().as_deref() == Some(name)) {
                info!("Using selected output device: {name}");
                return Ok(device);
            }
            warn!("Selected output device '{name}' not found, falling back to OS default");
        }
    }

    cpal::default_host()
        .default_output_device()
        .ok_or(AudioError::OutputUnavailable)
}

/// Query the device's default input stream config, falling back to 48kHz mono.
pub(crate) fn input_config(device: &Device) -> StreamConfig {
    match device.default_input_config() {
        Ok(supported) => {
            let config: StreamConfig = supported.into();
            info!(
                "Using input config: {}ch @ {}Hz",
                config.channels, config.sample_rate.0
            );
            config
        }
        Err(_) => {
            info!("Falling back to 48kHz mono input config");
            StreamConfig {
                channels: 1,
                sample_rate: cpal::SampleRate(48000),
                buffer_size: cpal::BufferSize::Default,
            }
        }
    }
}

/// Query the device's default output stream config, falling back to 48kHz mono.
pub(crate) fn output_config(device: &Device) -> StreamConfig {
    match device.default_output_config() {
        Ok(supported) => {
            let config: StreamConfig = supported.into();
            info!(
                "Using output config: {}ch @ {}Hz",
                config.channels, config.sample_rate.0
            );
            config
        }
        Err(_) => {
            info!("Falling back to 48kHz mono output config");
            StreamConfig {
                channels: 1,
                sample_rate: cpal::SampleRate(48000),
                buffer_size: cpal::BufferSize::Default,
            }
        }
    }
}
