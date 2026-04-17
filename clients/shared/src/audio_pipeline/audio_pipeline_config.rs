//! Configuration, constants, error types, and shared data models for the audio pipeline.

#![warn(missing_docs)]

use std::fmt;
use std::time::{Duration, Instant};

/// 48 kHz sample rate used throughout the pipeline.
pub const SAMPLE_RATE: u32 = 48_000;

/// Mono channel count.
pub const CHANNELS: u16 = 1;

/// Samples per 20 ms Opus frame at 48 kHz.
pub const FRAME_SAMPLES: usize = 960;

/// Samples per 10 ms APM frame at 48 kHz.
pub const APM_FRAME_SAMPLES: usize = 480;

/// Maximum encoded Opus frame size in bytes.
pub const MAX_OPUS_PACKET: usize = 4000;

/// Jitter buffer minimum delay in milliseconds.
pub const MIN_JITTER_DELAY_MS: f64 = 20.0;

/// Jitter buffer maximum delay in milliseconds.
pub const MAX_JITTER_DELAY_MS: f64 = 200.0;

/// Maximum jitter buffer shrink rate: 5 ms per second.
pub const MAX_DELAY_SHRINK_PER_SEC_MS: f64 = 5.0;

/// Minimum prefetch duration in milliseconds for the jitter buffer startup phase.
/// Even when the adaptive target delay is lower, the buffer holds packets for at
/// least this long before releasing any audio.
pub const MIN_PREFETCH_MS: u64 = 60;

/// Duration of one audio frame (20 ms at 48 kHz).
/// Used in playout deadline computation so any future ptime change propagates automatically.
pub const FRAME_DURATION: Duration = Duration::from_millis(20);

/// Maximum allowed single RTP packet payload size in bytes (MTU-aligned).
pub const MAX_PACKET_SIZE: usize = 1500;

/// Maximum number of packets the jitter buffer may hold at any time.
/// At 20 ms/frame, 500 packets ~= 10 seconds of audio.
pub const MAX_BUFFERED_PACKETS: usize = 500;

/// Indicates the origin of the current RTT measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RttSource {
    /// No RTT data available.
    #[default]
    None,
    /// RTT computed from RTCP round-trip calculation.
    Rtcp,
    /// RTT obtained from LiveKit stats API.
    LiveKit,
}

impl fmt::Display for RttSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RttSource::None => write!(f, "none"),
            RttSource::Rtcp => write!(f, "rtcp"),
            RttSource::LiveKit => write!(f, "livekit"),
        }
    }
}

/// Runtime state of the audio processor (AEC/NS/AGC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApmMode {
    /// Full AEC/NS/AGC processing active.
    Enabled,
    /// Feature disabled or not compiled - audio passes through unmodified.
    Bypass,
    /// Feature enabled but APM initialization failed at runtime.
    FailedInit,
}

impl fmt::Display for ApmMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApmMode::Enabled => write!(f, "enabled"),
            ApmMode::Bypass => write!(f, "bypass"),
            ApmMode::FailedInit => write!(f, "failed_init"),
        }
    }
}

/// Snapshot of jitter buffer health metrics.
#[derive(Debug, Clone, Default)]
pub struct JitterBufferStats {
    /// Current adaptive target delay in milliseconds.
    pub target_delay_ms: f64,
    /// Time span of buffered packets in milliseconds.
    pub current_buffered_ms: f64,
    /// Count of packets whose arrival was after their computed playout deadline.
    pub late_packets: u64,
    /// Count of `Missing` results returned from `pop()` (PLC invocations).
    pub plc_frames: u64,
    /// Count of `NotReady` results returned from `pop()`.
    pub not_ready_count: u64,
}

/// Errors that can occur within the audio pipeline.
#[derive(Debug, thiserror::Error)]
pub enum AudioPipelineError {
    /// Opus encoding failed while converting a PCM frame into a network packet.
    #[error("opus encode error: {0}")]
    EncodeError(String),
    /// Opus decoding failed while converting a network packet into PCM samples.
    #[error("opus decode error: {0}")]
    DecodeError(String),
    /// Audio post-processing failed while applying AEC, NS, or AGC.
    #[error("audio processing error: {0}")]
    ProcessingError(String),
    /// Sample-rate conversion failed inside the capture or playback path.
    #[error("resample error: {0}")]
    ResampleError(String),
    /// Jitter-buffer state or packet handling failed.
    #[error("jitter buffer error: {0}")]
    JitterError(String),
    /// Invalid or inconsistent pipeline configuration was provided.
    #[error("configuration error: {0}")]
    ConfigError(String),
}

/// Top-level configuration for the audio pipeline.
pub struct AudioPipelineConfig {
    /// Minimum Opus encoder bitrate (bps).
    pub min_bitrate: u32,
    /// Maximum Opus encoder bitrate (bps).
    pub max_bitrate: u32,
    /// Initial Opus encoder bitrate (bps).
    pub initial_bitrate: u32,
    /// Jitter buffer minimum delay (ms).
    pub min_jitter_delay_ms: f64,
    /// Jitter buffer maximum delay (ms).
    pub max_jitter_delay_ms: f64,
    /// Enable audio processing (AEC + NS + AGC).
    pub enable_audio_processing: bool,
    /// Enable DTX (discontinuous transmission).
    pub enable_dtx: bool,
    /// Network monitor poll interval (ms).
    pub stats_poll_interval_ms: u64,
}

impl Default for AudioPipelineConfig {
    fn default() -> Self {
        Self {
            min_bitrate: 16_000,
            max_bitrate: 64_000,
            initial_bitrate: 32_000,
            min_jitter_delay_ms: MIN_JITTER_DELAY_MS,
            max_jitter_delay_ms: MAX_JITTER_DELAY_MS,
            enable_audio_processing: true,
            enable_dtx: true,
            stats_poll_interval_ms: 1_000,
        }
    }
}

/// Configuration for the adaptive bitrate controller.
pub struct BitrateConfig {
    /// Minimum bitrate (bps).
    pub min_bitrate: u32,
    /// Maximum bitrate (bps).
    pub max_bitrate: u32,
    /// Initial bitrate (bps).
    pub initial_bitrate: u32,
    /// Seconds of sustained low loss before ramping up.
    pub ramp_up_delay_secs: f64,
}

impl Default for BitrateConfig {
    fn default() -> Self {
        Self {
            min_bitrate: 16_000,
            max_bitrate: 64_000,
            initial_bitrate: 32_000,
            ramp_up_delay_secs: 5.0,
        }
    }
}

/// Snapshot of network statistics from the WebRTC transport.
#[derive(Debug, Clone)]
pub struct NetworkStats {
    /// Packet loss ratio (0.0 to 1.0).
    pub packet_loss: f64,
    /// Round-trip time in milliseconds.
    pub rtt_ms: f64,
    /// Average inter-arrival jitter in milliseconds (EMA).
    pub jitter_ms: f64,
    /// Standard deviation of jitter over the last 100 packets.
    pub jitter_stddev_ms: f64,
    /// Origin of the current RTT measurement.
    pub rtt_source: RttSource,
}

impl Default for NetworkStats {
    fn default() -> Self {
        Self {
            packet_loss: 0.0,
            rtt_ms: 0.0,
            jitter_ms: 0.0,
            jitter_stddev_ms: 0.0,
            rtt_source: RttSource::None,
        }
    }
}

/// Decision output from the bitrate controller.
#[derive(Debug)]
pub struct BitrateDecision {
    /// Target bitrate in bits per second.
    pub bitrate_bps: u32,
    /// Whether in-band FEC should be enabled.
    pub fec_enabled: bool,
    /// FEC redundancy depth: 0 (off), 1 (normal), or 2 (burst loss).
    pub fec_depth: u8,
}

/// Unified snapshot of all pipeline health metrics for a single log line.
///
/// Assembled once per second by the control loop. Implements `Display` in a
/// compact key=value format suitable for log grep. Field names follow an
/// append-only evolution rule: new fields may be added, but existing names
/// must not be renamed or removed.
#[derive(Debug)]
pub struct PipelineTelemetry {
    /// Network statistics (loss, rtt, jitter).
    pub network: NetworkStats,
    /// Jitter buffer health counters.
    pub jitter: JitterBufferStats,
    /// Current bitrate decision (bitrate, fec, fec_depth).
    pub bitrate: BitrateDecision,
    /// Capture ring buffer fill level in milliseconds.
    pub capture_fill_ms: usize,
    /// Playback ring buffer fill level in milliseconds.
    pub playback_fill_ms: usize,
    /// Capture buffer underrun count (interval).
    pub capture_underruns: u64,
    /// Capture buffer overrun count (interval).
    pub capture_overruns: u64,
    /// Playback buffer underrun count (interval).
    pub playback_underruns: u64,
    /// Playback buffer overrun count (interval).
    pub playback_overruns: u64,
    /// Number of backlog drop events (interval).
    pub backlog_drops: u64,
    /// Total samples dropped by backlog enforcement (interval).
    pub backlog_dropped_samples: u64,
    /// Frames successfully sent this interval.
    pub frames_sent: u64,
    /// Frames dropped (stale) this interval.
    pub frames_dropped: u64,
    /// Peak capture buffer depth in frames during the interval.
    pub max_backlog_frames: u64,
    /// Current APM mode.
    pub apm_mode: ApmMode,
}

impl fmt::Display for PipelineTelemetry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "loss={:.1}% rtt={:.0}ms({}) jitter={:.0}ms target_delay={:.0}ms \
             buffered={:.0}ms late={} plc={} not_ready={} \
             bitrate={}bps fec={} \
             cap_fill={}ms play_fill={}ms \
             cap_under={} cap_over={} play_under={} play_over={} \
             backlog_drops={} backlog_dropped={}ms \
             sent={} dropped={} max_backlog={} apm={}",
            self.network.packet_loss * 100.0,
            self.network.rtt_ms,
            self.network.rtt_source,
            self.network.jitter_ms,
            self.jitter.target_delay_ms,
            self.jitter.current_buffered_ms,
            self.jitter.late_packets,
            self.jitter.plc_frames,
            self.jitter.not_ready_count,
            self.bitrate.bitrate_bps,
            if self.bitrate.fec_enabled {
                "on"
            } else {
                "off"
            },
            self.capture_fill_ms,
            self.playback_fill_ms,
            self.capture_underruns,
            self.capture_overruns,
            self.playback_underruns,
            self.playback_overruns,
            self.backlog_drops,
            self.backlog_dropped_samples / 48,
            self.frames_sent,
            self.frames_dropped,
            self.max_backlog_frames,
            self.apm_mode,
        )
    }
}

/// A single buffered packet in the jitter buffer.
pub struct JitterPacket {
    /// RTP sequence number.
    pub sequence_number: u16,
    /// Encoded Opus payload.
    pub data: Vec<u8>,
    /// Timestamp when the packet was received.
    pub received_at: Instant,
}

/// Result of requesting the next packet from the jitter buffer.
#[derive(Debug)]
pub enum JitterResult {
    /// Packet available for decoding.
    Packet(Vec<u8>),
    /// Packet missing - caller should invoke PLC.
    Missing,
    /// Buffer not yet ready - wait before requesting again.
    NotReady,
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use proptest::prelude::*;

    pub(crate) fn arb_network_stats() -> impl Strategy<Value = NetworkStats> {
        (
            0.0f64..1.0,
            0.0f64..1000.0,
            0.0f64..500.0,
            0.0f64..500.0,
            prop_oneof![
                Just(RttSource::None),
                Just(RttSource::Rtcp),
                Just(RttSource::LiveKit),
            ],
        )
            .prop_map(
                |(packet_loss, rtt_ms, jitter_ms, jitter_stddev_ms, rtt_source)| NetworkStats {
                    packet_loss,
                    rtt_ms,
                    jitter_ms,
                    jitter_stddev_ms,
                    rtt_source,
                },
            )
    }

    pub(crate) fn arb_apm_mode() -> impl Strategy<Value = ApmMode> {
        prop_oneof![
            Just(ApmMode::Enabled),
            Just(ApmMode::Bypass),
            Just(ApmMode::FailedInit),
        ]
    }

    pub(crate) fn arb_jitter_buffer_stats() -> impl Strategy<Value = JitterBufferStats> {
        (
            0.0f64..500.0,
            0.0f64..500.0,
            0u64..10_000,
            0u64..10_000,
            0u64..10_000,
        )
            .prop_map(
                |(
                    target_delay_ms,
                    current_buffered_ms,
                    late_packets,
                    plc_frames,
                    not_ready_count,
                )| JitterBufferStats {
                    target_delay_ms,
                    current_buffered_ms,
                    late_packets,
                    plc_frames,
                    not_ready_count,
                },
            )
    }

    pub(crate) fn arb_bitrate_decision() -> impl Strategy<Value = BitrateDecision> {
        (16_000u32..=64_000, any::<bool>(), 0u8..=2).prop_map(
            |(bitrate_bps, fec_enabled, fec_depth)| BitrateDecision {
                bitrate_bps,
                fec_enabled,
                fec_depth,
            },
        )
    }

    pub(crate) fn arb_pipeline_telemetry() -> impl Strategy<Value = PipelineTelemetry> {
        let group_a = (
            arb_network_stats(),
            arb_jitter_buffer_stats(),
            arb_bitrate_decision(),
            0usize..1000,
            0usize..1000,
            0u64..10_000,
            0u64..10_000,
            0u64..10_000,
            0u64..10_000,
        );
        let group_b = (
            0u64..10_000,
            0u64..500_000,
            0u64..10_000,
            0u64..10_000,
            0u64..100,
            arb_apm_mode(),
        );

        (group_a, group_b).prop_map(
            |(
                (
                    network,
                    jitter,
                    bitrate,
                    capture_fill_ms,
                    playback_fill_ms,
                    capture_underruns,
                    capture_overruns,
                    playback_underruns,
                    playback_overruns,
                ),
                (
                    backlog_drops,
                    backlog_dropped_samples,
                    frames_sent,
                    frames_dropped,
                    max_backlog_frames,
                    apm_mode,
                ),
            )| PipelineTelemetry {
                network,
                jitter,
                bitrate,
                capture_fill_ms,
                playback_fill_ms,
                capture_underruns,
                capture_overruns,
                playback_underruns,
                playback_overruns,
                backlog_drops,
                backlog_dropped_samples,
                frames_sent,
                frames_dropped,
                max_backlog_frames,
                apm_mode,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use proptest::prelude::*;

    const BASELINE_KEYS: &[&str] = &[
        "loss=",
        "rtt=",
        "jitter=",
        "target_delay=",
        "buffered=",
        "late=",
        "plc=",
        "not_ready=",
        "bitrate=",
        "fec=",
        "cap_fill=",
        "play_fill=",
        "cap_under=",
        "cap_over=",
        "play_under=",
        "play_over=",
        "backlog_drops=",
        "backlog_dropped=",
        "sent=",
        "dropped=",
        "max_backlog=",
        "apm=",
    ];

    proptest! {
        #![proptest_config(ProptestConfig { cases: 100, .. ProptestConfig::default() })]

        /// Feature: audio-transport-hardening, Property 9: PipelineTelemetry Display format
        #[test]
        fn pipeline_telemetry_display_format(
            telemetry in arb_pipeline_telemetry(),
        ) {
            let output = format!("{}", telemetry);

            prop_assert!(
                !output.contains('\n'),
                "Display output must be a single line, got: {:?}",
                output,
            );

            for key in BASELINE_KEYS {
                prop_assert!(
                    output.contains(key),
                    "Display output missing baseline key '{}' in: {}",
                    key,
                    output,
                );
            }
        }
    }

    #[test]
    fn telemetry_display_known_input() {
        let telemetry = PipelineTelemetry {
            network: NetworkStats {
                packet_loss: 0.002,
                rtt_ms: 45.0,
                jitter_ms: 12.0,
                jitter_stddev_ms: 3.0,
                rtt_source: RttSource::Rtcp,
            },
            jitter: JitterBufferStats {
                target_delay_ms: 40.0,
                current_buffered_ms: 35.0,
                late_packets: 0,
                plc_frames: 0,
                not_ready_count: 12,
            },
            bitrate: BitrateDecision {
                bitrate_bps: 32_000,
                fec_enabled: false,
                fec_depth: 0,
            },
            capture_fill_ms: 22,
            playback_fill_ms: 18,
            capture_underruns: 0,
            capture_overruns: 0,
            playback_underruns: 0,
            playback_overruns: 0,
            backlog_drops: 0,
            backlog_dropped_samples: 0,
            frames_sent: 50,
            frames_dropped: 0,
            max_backlog_frames: 1,
            apm_mode: ApmMode::Enabled,
        };

        let output = format!("{}", telemetry);
        assert_eq!(
            output,
            "loss=0.2% rtt=45ms(rtcp) jitter=12ms target_delay=40ms \
             buffered=35ms late=0 plc=0 not_ready=12 \
             bitrate=32000bps fec=off \
             cap_fill=22ms play_fill=18ms \
             cap_under=0 cap_over=0 play_under=0 play_over=0 \
             backlog_drops=0 backlog_dropped=0ms \
             sent=50 dropped=0 max_backlog=1 apm=enabled"
        );
    }

    #[test]
    fn telemetry_golden_log_baseline_keys() {
        let telemetry = PipelineTelemetry {
            network: NetworkStats {
                packet_loss: 0.05,
                rtt_ms: 100.0,
                jitter_ms: 20.0,
                jitter_stddev_ms: 8.0,
                rtt_source: RttSource::LiveKit,
            },
            jitter: JitterBufferStats {
                target_delay_ms: 60.0,
                current_buffered_ms: 50.0,
                late_packets: 3,
                plc_frames: 1,
                not_ready_count: 5,
            },
            bitrate: BitrateDecision {
                bitrate_bps: 24_000,
                fec_enabled: true,
                fec_depth: 1,
            },
            capture_fill_ms: 40,
            playback_fill_ms: 30,
            capture_underruns: 2,
            capture_overruns: 1,
            playback_underruns: 3,
            playback_overruns: 0,
            backlog_drops: 1,
            backlog_dropped_samples: 1920,
            frames_sent: 100,
            frames_dropped: 5,
            max_backlog_frames: 4,
            apm_mode: ApmMode::FailedInit,
        };

        let output = format!("{}", telemetry);

        for key in BASELINE_KEYS {
            assert!(
                output.contains(key),
                "Golden log missing baseline key '{}' in: {}",
                key,
                output,
            );
        }

        let mut search_from = 0;
        for key in BASELINE_KEYS {
            let pos = output[search_from..]
                .find(key)
                .map(|p| p + search_from)
                .unwrap_or_else(|| {
                    panic!(
                        "Key '{}' not found after position {} in output: {}",
                        key, search_from, output
                    )
                });
            search_from = pos + key.len();
        }

        assert!(!output.contains('\n'), "Golden log must be a single line");
        assert!(
            output.contains("loss=5.0%"),
            "Expected loss=5.0% in: {}",
            output
        );
        assert!(
            output.contains("rtt=100ms(livekit)"),
            "Expected rtt=100ms(livekit) in: {}",
            output
        );
        assert!(output.contains("fec=on"), "Expected fec=on in: {}", output);
        assert!(
            output.contains("backlog_dropped=40ms"),
            "Expected backlog_dropped=40ms (1920/48) in: {}",
            output
        );
        assert!(
            output.contains("apm=failed_init"),
            "Expected apm=failed_init in: {}",
            output
        );
    }
}
