//! Network quality monitoring for the LiveKit SFU path.
//!
//! Provides [`run_stats_task`] and [`run_telemetry_task`] (spawned once per
//! connection) and the pure helpers [`extract_livekit_rtt_from_stats`] and
//! [`extract_livekit_loss_jitter`]. All items are `pub(super)`.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

#[cfg(feature = "real-backends")]
use std::sync::atomic::AtomicU64;

use livekit::Room as LkRoom;

#[cfg(feature = "real-backends")]
use crate::audio_network_monitor::{NetworkMonitorHandle, RealNetworkMonitor};
#[cfg(feature = "real-backends")]
use crate::audio_pipeline::{ApmMode, PipelineTelemetry, RttSource};
#[cfg(feature = "real-backends")]
use crate::cpal_audio::AudioBuffer;

// ---------------------------------------------------------------------------
// Pure extraction helpers
// ---------------------------------------------------------------------------

/// Extract transport-level RTT from LiveKit `SessionStats`.
///
/// Prefers the `CandidatePair` transport-level `current_round_trip_time` (seconds)
/// over per-track `RemoteInboundRtp` RTT. Returns the RTT in milliseconds, or
/// `None` if no valid RTT is available.
///
/// Note: `SessionStats` is not directly nameable from outside the `livekit` crate
/// (private module re-export), so this function uses the concrete type from the
/// `livekit::webrtc::stats` module to pattern-match on the stats entries.
#[cfg(feature = "real-backends")]
pub(super) fn extract_livekit_rtt_from_stats(
    publisher_stats: &[livekit::webrtc::stats::RtcStats],
    subscriber_stats: &[livekit::webrtc::stats::RtcStats],
) -> Option<f64> {
    use livekit::webrtc::stats::RtcStats;

    // First pass: look for CandidatePair stats with a non-zero RTT.
    // Check both publisher and subscriber stats.
    for stat in publisher_stats.iter().chain(subscriber_stats.iter()) {
        if let RtcStats::CandidatePair(cp) = stat {
            let rtt_s = cp.candidate_pair.current_round_trip_time;
            if rtt_s > 0.0 && rtt_s.is_finite() {
                return Some(rtt_s * 1000.0); // seconds → milliseconds
            }
        }
    }

    // Fallback: look for RemoteInboundRtp stats with a non-zero RTT.
    for stat in publisher_stats.iter().chain(subscriber_stats.iter()) {
        if let RtcStats::RemoteInboundRtp(ri) = stat {
            let rtt_s = ri.remote_inbound.round_trip_time;
            if rtt_s > 0.0 && rtt_s.is_finite() {
                return Some(rtt_s * 1000.0);
            }
        }
    }

    None
}

/// Extract packet loss percentage and jitter from LiveKit `SessionStats`.
///
/// Looks for `InboundRtp` stats (audio) for packet loss and jitter.
/// Returns `(packet_loss_percent, jitter_ms)`. Both default to 0.0 if
/// no relevant stats are found.
#[cfg(feature = "real-backends")]
pub(super) fn extract_livekit_loss_jitter(
    publisher_stats: &[livekit::webrtc::stats::RtcStats],
    subscriber_stats: &[livekit::webrtc::stats::RtcStats],
) -> (f64, f64) {
    use livekit::webrtc::stats::RtcStats;

    let mut total_packets: u64 = 0;
    let mut lost_packets: i64 = 0;
    let mut jitter_ms: f64 = 0.0;

    for stat in publisher_stats.iter().chain(subscriber_stats.iter()) {
        if let RtcStats::InboundRtp(ib) = stat {
            // Only count audio streams
            if ib.stream.kind == "audio" {
                let received = ib.received.packets_received;
                let lost = ib.received.packets_lost;
                total_packets += received + lost.max(0) as u64;
                lost_packets += lost;

                let j = ib.received.jitter;
                if j > 0.0 && j.is_finite() {
                    jitter_ms = j * 1000.0; // seconds → ms
                }
            }
        }
    }

    let packet_loss_pct = if total_packets > 0 {
        let loss = (lost_packets.max(0) as f64 / total_packets as f64) * 100.0;
        (loss * 10.0).round() / 10.0 // one decimal
    } else {
        0.0
    };

    (packet_loss_pct, jitter_ms)
}

// ---------------------------------------------------------------------------
// Background task bodies
// ---------------------------------------------------------------------------

/// Stats polling task body — polls LiveKit `get_stats()` once per second,
/// extracts RTT/loss/jitter, feeds RTT into `NetworkMonitorInput`, and
/// invokes the stats callback.
///
/// Spawned once per connection in `livekit_connection::connect()`.
#[cfg(feature = "real-backends")]
#[allow(clippy::type_complexity)]
pub(super) async fn run_stats_task(
    net_handle: Arc<Mutex<Option<NetworkMonitorHandle>>>,
    room_ref: Arc<Mutex<Option<LkRoom>>>,
    closing: Arc<AtomicBool>,
    stats_cb: Arc<Mutex<Option<Box<dyn Fn(f64, f64, f64) + Send + 'static>>>>,
) {
    use std::time::Instant;
    use tokio::time::{interval, Duration};

    let mut ticker = interval(Duration::from_secs(1));

    loop {
        ticker.tick().await;

        if closing.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }

        // We can't hold std::sync::MutexGuard across an await point,
        // and Room doesn't implement Clone. Use block_in_place to
        // bridge the async get_stats() call synchronously while
        // holding the lock briefly.
        let stats_result = tokio::task::block_in_place(|| {
            let guard = room_ref.lock().unwrap();
            if let Some(ref room) = *guard {
                let handle = tokio::runtime::Handle::current();
                match handle.block_on(room.get_stats()) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        log::debug!("livekit_stats: get_stats failed: {e}");
                        None
                    }
                }
            } else {
                None
            }
        });

        if let Some(session_stats) = stats_result {
            let rtt_ms = extract_livekit_rtt_from_stats(
                &session_stats.publisher_stats,
                &session_stats.subscriber_stats,
            );
            let (packet_loss_pct, jitter_ms) = extract_livekit_loss_jitter(
                &session_stats.publisher_stats,
                &session_stats.subscriber_stats,
            );

            // Feed RTT into NetworkMonitorInput
            let monitor = net_handle.lock().unwrap().clone();
            if let Some(monitor) = monitor {
                let now = Instant::now();
                let mut input = monitor.lock().unwrap();
                if let Some(rtt) = rtt_ms {
                    if rtt >= 0.0 {
                        input.set_rtt(now, rtt, RttSource::LiveKit);
                    }
                }
            }

            // Emit stats to frontend via callback
            if let Some(ref cb) = *stats_cb.lock().unwrap() {
                cb(rtt_ms.unwrap_or(0.0), packet_loss_pct, jitter_ms);
            }
        }
    }
}

/// Unified pipeline telemetry loop — logs one `Pipeline:` line per second,
/// mirroring the control loop in `webrtc_backend.rs` so LiveKit mode emits
/// the same structured telemetry line documented in §35 of TESTING.md.
///
/// Spawned once per connection in `livekit_connection::connect()`.
///
/// # Note on buffer parameters
/// `cap_buf` and `play_buf` are snapshots taken at connect time (not Arcs).
/// This matches the original semantics: the telemetry task observes the
/// buffer that was wired up at connection, not any later replacement.
#[cfg(feature = "real-backends")]
pub(super) async fn run_telemetry_task(
    closing: Arc<AtomicBool>,
    cap_buf: Option<AudioBuffer>,
    play_buf: Option<AudioBuffer>,
    net_handle: Option<NetworkMonitorHandle>,
    frames_sent_counter: Arc<AtomicU64>,
    frames_dropped_counter: Arc<AtomicU64>,
) {
    use crate::audio_pipeline::NetworkMonitoring;
    use std::sync::atomic::Ordering;
    use tokio::time::{interval, Duration};

    // Build a RealNetworkMonitor from the shared input handle (if wired).
    let mut monitor = net_handle.map(RealNetworkMonitor::new);

    let mut ticker = interval(Duration::from_secs(1));

    // Log APM mode once — LiveKit handles audio processing internally.
    log::info!("APM mode: {}", ApmMode::Bypass);

    loop {
        ticker.tick().await;

        if closing.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }

        // Poll network stats (RTT fed by the stats_task above).
        let stats = match monitor.as_mut() {
            Some(m) => m.poll_stats(),
            None => Default::default(),
        };

        // Buffer health.
        let (cap_stats, cap_fill) = match &cap_buf {
            Some(b) => (b.stats(), b.fill_ms()),
            None => (Default::default(), 0),
        };
        let (play_stats, play_fill) = match &play_buf {
            Some(b) => (b.stats(), b.fill_ms()),
            None => (Default::default(), 0),
        };

        // Read and reset per-interval sender counters.
        let sent = frames_sent_counter.swap(0, Ordering::Relaxed);
        let dropped = frames_dropped_counter.swap(0, Ordering::Relaxed);

        let telemetry = PipelineTelemetry {
            network: stats,
            jitter: Default::default(), // LiveKit handles jitter internally
            bitrate: crate::audio_pipeline::BitrateDecision {
                bitrate_bps: 0,
                fec_enabled: false,
                fec_depth: 0,
            },
            capture_fill_ms: cap_fill,
            playback_fill_ms: play_fill,
            capture_underruns: cap_stats.underruns,
            capture_overruns: cap_stats.overruns,
            playback_underruns: play_stats.underruns,
            playback_overruns: play_stats.overruns,
            backlog_drops: cap_stats.backlog_drops + play_stats.backlog_drops,
            backlog_dropped_samples: cap_stats.backlog_dropped_samples
                + play_stats.backlog_dropped_samples,
            frames_sent: sent,
            frames_dropped: dropped,
            max_backlog_frames: 0,
            apm_mode: ApmMode::Bypass,
        };

        log::info!("Pipeline: {}", telemetry);

        let total_backlog_drops = cap_stats.backlog_drops + play_stats.backlog_drops;
        if total_backlog_drops > 0 {
            log::warn!(
                "buffer backlog drop: {} events, latency was capped",
                total_backlog_drops,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // Unit tests for LiveKit stats extraction (Task 8.4)
    // Requirements: 2.6

    #[cfg(feature = "real-backends")]
    mod livekit_stats_tests {
        use super::super::extract_livekit_rtt_from_stats;
        use livekit::webrtc::stats::{self, RtcStats};

        #[test]
        fn extract_rtt_from_candidate_pair() {
            let cp = stats::CandidatePairStats {
                rtc: Default::default(),
                candidate_pair: stats::dictionaries::CandidatePairStats {
                    current_round_trip_time: 0.045, // 45ms in seconds
                    ..Default::default()
                },
            };
            let publisher = vec![RtcStats::CandidatePair(cp)];
            let subscriber = vec![];

            let rtt = extract_livekit_rtt_from_stats(&publisher, &subscriber);
            assert!(rtt.is_some());
            let rtt = rtt.unwrap();
            assert!((rtt - 45.0).abs() < 0.1, "expected ~45ms, got {}", rtt);
        }

        #[test]
        fn extract_rtt_prefers_candidate_pair_over_remote_inbound() {
            let cp = stats::CandidatePairStats {
                rtc: Default::default(),
                candidate_pair: stats::dictionaries::CandidatePairStats {
                    current_round_trip_time: 0.050, // 50ms
                    ..Default::default()
                },
            };
            let ri = stats::RemoteInboundRtpStats {
                rtc: Default::default(),
                stream: Default::default(),
                received: Default::default(),
                remote_inbound: stats::dictionaries::RemoteInboundRtpStreamStats {
                    round_trip_time: 0.100, // 100ms — should NOT be used
                    ..Default::default()
                },
            };
            let publisher = vec![RtcStats::RemoteInboundRtp(ri), RtcStats::CandidatePair(cp)];
            let subscriber = vec![];

            let rtt = extract_livekit_rtt_from_stats(&publisher, &subscriber);
            assert!(rtt.is_some());
            let rtt = rtt.unwrap();
            // Should use CandidatePair (50ms), not RemoteInboundRtp (100ms).
            assert!((rtt - 50.0).abs() < 0.1, "expected ~50ms, got {}", rtt);
        }

        #[test]
        fn extract_rtt_falls_back_to_remote_inbound() {
            let ri = stats::RemoteInboundRtpStats {
                rtc: Default::default(),
                stream: Default::default(),
                received: Default::default(),
                remote_inbound: stats::dictionaries::RemoteInboundRtpStreamStats {
                    round_trip_time: 0.080, // 80ms
                    ..Default::default()
                },
            };
            let publisher = vec![RtcStats::RemoteInboundRtp(ri)];
            let subscriber = vec![];

            let rtt = extract_livekit_rtt_from_stats(&publisher, &subscriber);
            assert!(rtt.is_some());
            let rtt = rtt.unwrap();
            assert!((rtt - 80.0).abs() < 0.1, "expected ~80ms, got {}", rtt);
        }

        #[test]
        fn extract_rtt_returns_none_when_no_stats() {
            let rtt = extract_livekit_rtt_from_stats(&[], &[]);
            assert!(rtt.is_none());
        }

        #[test]
        fn extract_rtt_skips_zero_rtt() {
            let cp = stats::CandidatePairStats {
                rtc: Default::default(),
                candidate_pair: stats::dictionaries::CandidatePairStats {
                    current_round_trip_time: 0.0, // zero — skip
                    ..Default::default()
                },
            };
            let publisher = vec![RtcStats::CandidatePair(cp)];
            let subscriber = vec![];

            let rtt = extract_livekit_rtt_from_stats(&publisher, &subscriber);
            assert!(rtt.is_none(), "expected None for zero RTT");
        }

        #[test]
        fn extract_rtt_from_subscriber_stats() {
            let cp = stats::CandidatePairStats {
                rtc: Default::default(),
                candidate_pair: stats::dictionaries::CandidatePairStats {
                    current_round_trip_time: 0.030, // 30ms
                    ..Default::default()
                },
            };
            let publisher = vec![];
            let subscriber = vec![RtcStats::CandidatePair(cp)];

            let rtt = extract_livekit_rtt_from_stats(&publisher, &subscriber);
            assert!(rtt.is_some());
            let rtt = rtt.unwrap();
            assert!((rtt - 30.0).abs() < 0.1, "expected ~30ms, got {}", rtt);
        }
    }
}
