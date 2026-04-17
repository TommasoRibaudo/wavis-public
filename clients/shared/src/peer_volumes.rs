//! Owns per-peer volume tracking and the perceptual gain curve for the
//! playback path.
//!
//! This module does not own audio buffers, CPAL streams, or device
//! management — those concerns live in `audio_buffer`, `cpal_audio`,
//! and `cpal_device` respectively.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Default playback volume (0–100). Uses perceptual (cubic) gain curve.
/// 70 ≈ unity gain (1.0×), 100 = 3.0× boost.
pub const DEFAULT_VOLUME: u8 = 70;

/// Maximum gain multiplier at volume 100.
/// 3.0 = ~10 dB headroom above unity, enough to compensate for quiet sources.
const MAX_GAIN: f32 = 3.0;

/// Convert a 0–100 volume knob value to a perceptual gain multiplier.
///
/// Uses a cubic curve so the slider feels natural to human hearing:
/// - Volume   0 → gain 0.0 (silent)
/// - Volume  50 → gain 0.375 (quiet but audible)
/// - Volume  70 → gain ~1.03 (≈ unity)
/// - Volume 100 → gain 3.0 (+10 dB boost)
///
/// The cubic curve `(vol/100)^3 * MAX_GAIN` concentrates fine control in
/// the low-to-mid range where ears are most sensitive, while the upper
/// range provides real amplification for quiet sources.
pub fn perceptual_gain(vol: u8) -> f32 {
    let normalized = vol.min(100) as f32 / 100.0;
    let curved = normalized * normalized * normalized; // cubic
    curved * MAX_GAIN
}

/// Maximum number of peer volume entries. Matches the room capacity limit (6).
const MAX_PEER_VOLUMES: usize = 6;

/// Shared per-peer volume map. Thread-safe, cloneable.
/// Used to scale individual participant audio before mixing into the
/// playback buffer. Peers not in the map use `DEFAULT_VOLUME`.
/// Capped at `MAX_PEER_VOLUMES` entries to prevent unbounded growth
/// from malicious IPC calls.
#[derive(Clone, Debug)]
pub struct PeerVolumes {
    inner: Arc<Mutex<HashMap<String, u8>>>,
}

impl PeerVolumes {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Set volume for a peer (clamped to 0–100).
    ///
    /// If the peer already has an entry, updates it. Otherwise, inserts only
    /// if the map has fewer than `MAX_PEER_VOLUMES` entries (room capacity).
    /// Silently ignores the call if the map is full and the peer is unknown —
    /// this bounds memory growth from untrusted IPC inputs.
    pub fn set(&self, peer_id: &str, vol: u8) {
        let mut map = self.inner.lock().unwrap();
        if map.contains_key(peer_id) || map.len() < MAX_PEER_VOLUMES {
            map.insert(peer_id.to_string(), vol.min(100));
        }
    }

    /// Get volume for a peer. Returns `DEFAULT_VOLUME` if not explicitly set.
    pub fn get(&self, peer_id: &str) -> u8 {
        self.inner
            .lock()
            .unwrap()
            .get(peer_id)
            .copied()
            .unwrap_or(DEFAULT_VOLUME)
    }

    /// Get the gain multiplier for a peer (perceptual cubic curve, 0.0–2.0).
    pub fn gain(&self, peer_id: &str) -> f32 {
        perceptual_gain(self.get(peer_id))
    }

    /// Remove a peer's volume entry (e.g. when they leave).
    pub fn remove(&self, peer_id: &str) {
        self.inner.lock().unwrap().remove(peer_id);
    }

    /// Clear all entries.
    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }

    /// List all peers with custom volumes.
    pub fn list(&self) -> Vec<(String, u8)> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect()
    }
}

impl Default for PeerVolumes {
    fn default() -> Self {
        Self::new()
    }
}
