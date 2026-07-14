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

#[cfg(test)]
mod tests {
    use super::*;

    // ── perceptual_gain ──────────────────────────────────────────────

    #[test]
    fn perceptual_gain_zero_is_silent() {
        assert_eq!(perceptual_gain(0), 0.0);
    }

    #[test]
    fn perceptual_gain_100_is_max_gain() {
        let g = perceptual_gain(100);
        assert!((g - MAX_GAIN).abs() < 1e-5, "expected {MAX_GAIN}, got {g}");
    }

    #[test]
    fn perceptual_gain_70_is_near_unity() {
        let g = perceptual_gain(70);
        assert!(g > 0.99 && g < 1.1, "expected ~1.0 at vol=70, got {g}");
    }

    #[test]
    fn perceptual_gain_clamps_above_100() {
        assert_eq!(perceptual_gain(200), perceptual_gain(100));
    }

    #[test]
    fn perceptual_gain_is_monotonically_increasing() {
        let mut prev = perceptual_gain(0);
        for v in 1u8..=100 {
            let g = perceptual_gain(v);
            assert!(g >= prev, "gain not monotone at vol={v}: {g} < {prev}");
            prev = g;
        }
    }

    // ── PeerVolumes::get / set ───────────────────────────────────────

    #[test]
    fn get_returns_default_for_unknown_peer() {
        let pv = PeerVolumes::new();
        assert_eq!(pv.get("unknown"), DEFAULT_VOLUME);
    }

    #[test]
    fn set_and_get_round_trip() {
        let pv = PeerVolumes::new();
        pv.set("peer-1", 42);
        assert_eq!(pv.get("peer-1"), 42);
    }

    #[test]
    fn set_clamps_volume_above_100() {
        let pv = PeerVolumes::new();
        pv.set("peer-1", 200);
        assert_eq!(pv.get("peer-1"), 100);
    }

    #[test]
    fn set_allows_zero_volume() {
        let pv = PeerVolumes::new();
        pv.set("peer-1", 0);
        assert_eq!(pv.get("peer-1"), 0);
    }

    #[test]
    fn set_updates_existing_entry() {
        let pv = PeerVolumes::new();
        pv.set("peer-1", 50);
        pv.set("peer-1", 80);
        assert_eq!(pv.get("peer-1"), 80);
    }

    // ── capacity cap ────────────────────────────────────────────────

    #[test]
    fn set_ignores_new_peer_when_map_is_full() {
        let pv = PeerVolumes::new();
        for i in 0..MAX_PEER_VOLUMES {
            pv.set(&format!("peer-{i}"), 50);
        }
        assert_eq!(pv.list().len(), MAX_PEER_VOLUMES);
        pv.set("peer-overflow", 99);
        assert_eq!(
            pv.get("peer-overflow"),
            DEFAULT_VOLUME,
            "overflow peer should return default"
        );
        assert_eq!(
            pv.list().len(),
            MAX_PEER_VOLUMES,
            "map should not grow beyond cap"
        );
    }

    #[test]
    fn set_updates_existing_peer_even_when_map_is_full() {
        let pv = PeerVolumes::new();
        for i in 0..MAX_PEER_VOLUMES {
            pv.set(&format!("peer-{i}"), 50);
        }
        pv.set("peer-0", 99);
        assert_eq!(pv.get("peer-0"), 99);
    }

    // ── remove / clear ───────────────────────────────────────────────

    #[test]
    fn remove_deletes_entry() {
        let pv = PeerVolumes::new();
        pv.set("peer-1", 42);
        pv.remove("peer-1");
        assert_eq!(pv.get("peer-1"), DEFAULT_VOLUME);
    }

    #[test]
    fn remove_nonexistent_is_noop() {
        let pv = PeerVolumes::new();
        pv.remove("ghost");
        assert_eq!(pv.get("ghost"), DEFAULT_VOLUME);
    }

    #[test]
    fn clear_removes_all_entries() {
        let pv = PeerVolumes::new();
        pv.set("peer-1", 10);
        pv.set("peer-2", 20);
        pv.clear();
        assert_eq!(pv.list().len(), 0);
        assert_eq!(pv.get("peer-1"), DEFAULT_VOLUME);
    }

    #[test]
    fn remove_frees_capacity_for_new_peer() {
        let pv = PeerVolumes::new();
        for i in 0..MAX_PEER_VOLUMES {
            pv.set(&format!("peer-{i}"), 50);
        }
        pv.remove("peer-0");
        pv.set("peer-new", 77);
        assert_eq!(pv.get("peer-new"), 77);
    }

    // ── gain ─────────────────────────────────────────────────────────

    #[test]
    fn gain_returns_perceptual_gain_for_set_peer() {
        let pv = PeerVolumes::new();
        pv.set("peer-1", 50);
        let expected = perceptual_gain(50);
        assert!((pv.gain("peer-1") - expected).abs() < 1e-6);
    }

    #[test]
    fn gain_returns_default_gain_for_unknown_peer() {
        let pv = PeerVolumes::new();
        let expected = perceptual_gain(DEFAULT_VOLUME);
        assert!((pv.gain("unknown") - expected).abs() < 1e-6);
    }

    // ── list ─────────────────────────────────────────────────────────

    #[test]
    fn list_returns_all_set_peers() {
        let pv = PeerVolumes::new();
        pv.set("a", 10);
        pv.set("b", 20);
        let mut entries = pv.list();
        entries.sort_by_key(|(k, _)| k.clone());
        assert_eq!(entries, vec![("a".to_string(), 10), ("b".to_string(), 20)]);
    }

    #[test]
    fn list_is_empty_on_new_instance() {
        let pv = PeerVolumes::new();
        assert!(pv.list().is_empty());
    }

    // ── Clone shares the same underlying map ─────────────────────────

    #[test]
    fn clone_shares_inner_arc() {
        let pv1 = PeerVolumes::new();
        let pv2 = pv1.clone();
        pv1.set("peer-1", 55);
        assert_eq!(pv2.get("peer-1"), 55);
    }

    // ── thread safety ────────────────────────────────────────────────

    #[test]
    fn concurrent_set_and_get_do_not_panic() {
        use std::sync::Arc;
        use std::thread;

        let pv = Arc::new(PeerVolumes::new());
        let mut handles = Vec::new();

        for i in 0..6usize {
            let pv_clone = Arc::clone(&pv);
            handles.push(thread::spawn(move || {
                pv_clone.set(&format!("peer-{i}"), (i * 10) as u8);
                let _ = pv_clone.get(&format!("peer-{i}"));
            }));
        }

        for h in handles {
            h.join().expect("thread panicked");
        }
    }
}
