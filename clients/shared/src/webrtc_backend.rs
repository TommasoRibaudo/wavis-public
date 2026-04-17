//! Real WebRTC PeerConnection backend using `webrtc-rs` + audio pipeline.
//!
//! Gated behind the `real-backends` feature flag.
//!
//! This module owns PeerConnection lifecycle: creation, SDP negotiation,
//! ICE candidate exchange, event callbacks, and teardown. The async task
//! loops (send, receive, control) live in `webrtc_loops`, and APM/denoise
//! coordination lives in `webrtc_apm`.
//!
//! Audio pipeline (Opus, 48kHz mono throughout):
//! - Outbound: reads mono f32 samples from `CpalAudioBackend::capture_buffer`,
//!   processes through APM (AEC/NS/AGC) in 10ms chunks, encodes with Opus
//!   via `OpusEncode` trait, writes to `TrackLocalStaticSample`.
//! - Inbound: reads RTP packets from `TrackRemote`, feeds into
//!   `AdaptiveJitterBuffer`, decodes via `OpusDecode` trait (with PLC for
//!   missing packets), writes mono f32 samples to playback buffer.
//! - Control: 1-second interval task polls `RealNetworkMonitor`, feeds
//!   `AdaptiveBitrateController`, applies bitrate/FEC decisions to encoder,
//!   and updates jitter buffer stats.
//!
//! ## Mutex strategy
//! All internal `Mutex` guards recover poisoned state with
//! `.unwrap_or_else(|e| e.into_inner())` instead of panicking. In this module,
//! the guarded values are `Option<T>`, `bool`, callback slots, and small control
//! state, so recovering the inner value is safe and avoids cascading panics
//! during teardown and callback dispatch.

use crate::audio::AudioTrack;
use crate::audio_meter::AudioMeter;
use crate::audio_network_monitor::{new_network_monitor, NetworkMonitorHandle};
use crate::audio_pipeline::{AdaptiveJitterBuffer, ApmMode, JitterBuffering, OpusEncode};
use crate::cpal_audio::{AudioBuffer, CpalAudioBackend};
use crate::denoise_filter::DenoiseFilter;
use crate::ice_config::IceConfig;
use crate::webrtc::{CallError, ConnectionState, PeerConnectionBackend};
use crate::webrtc_loops;
use log::{info, warn};
use shared::signaling::IceCandidate;
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use std::time::Instant;
use tokio::runtime::Handle;
use tokio::task::block_in_place;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

/// Shared callback type used for ICE candidate and connection state event handlers.
pub(crate) type EventCb<T> = Arc<Mutex<Option<Box<dyn Fn(T) + Send + 'static>>>>;

/// Opus at 48kHz mono — the WebRTC standard audio codec.
pub(crate) const AUDIO_MIME_TYPE: &str = "audio/opus";

/// Opus always uses 48kHz clock rate in WebRTC (RFC 7587).
pub(crate) const AUDIO_CLOCK_RATE: u32 = 48000;

/// Duration of one audio frame (20ms).
pub(crate) const FRAME_DURATION: Duration = Duration::from_millis(20);

/// Initial Opus encoder bitrate (bps).
pub(crate) const INITIAL_BITRATE: u32 = 32_000;

fn recover_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct WebRtcPeerConnectionBackend {
    /// The active RTCPeerConnection (set after create_peer_connection)
    pub(crate) pc: Arc<Mutex<Option<Arc<RTCPeerConnection>>>>,
    /// Local audio track for sending mic audio
    pub(crate) local_track: Arc<Mutex<Option<Arc<TrackLocalStaticSample>>>>,
    /// Capture buffer from CpalAudioBackend — we read mic samples from here
    pub(crate) capture_buffer: AudioBuffer,
    /// Playback buffer from CpalAudioBackend — we write remote audio here
    pub(crate) playback_buffer: AudioBuffer,
    /// ICE candidate callback
    pub(crate) ice_cb: EventCb<IceCandidate>,
    /// Connection state callback
    pub(crate) conn_state_cb: EventCb<ConnectionState>,
    /// Whether the PC is active
    pub(crate) active: Arc<Mutex<bool>>,
    /// Handle to the tokio runtime for blocking on async calls
    pub(crate) rt_handle: Handle,
    /// Handle to the audio send task so we can abort it on close
    pub(crate) send_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Handle to the control loop task so we can abort it on close
    pub(crate) control_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Shared encoder behind a Mutex so the control loop can adjust bitrate/FEC.
    pub(crate) shared_encoder: Arc<Mutex<Option<Box<dyn OpusEncode>>>>,
    /// Shared jitter buffer behind a Mutex so the control loop can update stats.
    pub(crate) shared_jitter: Arc<Mutex<Option<Box<dyn JitterBuffering>>>>,
    /// Network monitor input handle for the RTP receive path to record packets.
    pub(crate) net_monitor_handle: NetworkMonitorHandle,
    /// Audio meter: raw capture output (before APM).
    pub(crate) capture_meter: Arc<AudioMeter>,
    /// Audio meter: post-APM (after AEC/NS/AGC, before Opus encode).
    pub(crate) post_apm_meter: Arc<AudioMeter>,
    /// Audio meter: pre-playback (decoded remote audio before speaker).
    pub(crate) pre_playback_meter: Arc<AudioMeter>,
    /// Per-interval sender counter: frames successfully sent.
    pub(crate) sender_frames_sent: Arc<AtomicU64>,
    /// Per-interval sender counter: frames dropped (stale backlog).
    pub(crate) sender_frames_dropped: Arc<AtomicU64>,
    /// Per-interval sender counter: peak capture buffer depth in frames.
    pub(crate) sender_max_backlog_frames: Arc<AtomicU64>,
    /// APM mode captured at processor creation time, shared with control loop.
    pub(crate) apm_mode: Arc<Mutex<ApmMode>>,
    /// Handle to the RTCP reader task so we can abort it on close.
    pub(crate) rtcp_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Noise suppression filter (nnnoiseless RNNoise), shared with IPC toggle path.
    pub(crate) denoise: Arc<DenoiseFilter>,
    /// Audio meter: post-denoise (after DenoiseFilter, before APM).
    pub(crate) post_denoise_meter: Arc<AudioMeter>,
}

impl WebRtcPeerConnectionBackend {
    /// Create a new backend. Must be called from within a tokio runtime.
    ///
    /// Takes the capture and playback buffers from a `CpalAudioBackend`
    /// so audio flows between CPAL and WebRTC.
    ///
    /// `denoise_enabled` sets the initial state of the nnnoiseless noise
    /// suppression filter (typically read from persisted user preference).
    pub fn new(audio: &CpalAudioBackend, denoise_enabled: bool) -> Self {
        let (_monitor, handle) = new_network_monitor();
        Self {
            pc: Arc::new(Mutex::new(None)),
            local_track: Arc::new(Mutex::new(None)),
            capture_buffer: audio.capture_buffer.clone(),
            playback_buffer: audio.playback_buffer.clone(),
            ice_cb: Arc::new(Mutex::new(None)),
            conn_state_cb: Arc::new(Mutex::new(None)),
            active: Arc::new(Mutex::new(false)),
            rt_handle: Handle::current(),
            send_task: Arc::new(Mutex::new(None)),
            control_task: Arc::new(Mutex::new(None)),
            shared_encoder: Arc::new(Mutex::new(None)),
            shared_jitter: Arc::new(Mutex::new(None)),
            net_monitor_handle: handle,
            capture_meter: Arc::new(AudioMeter::new("capture")),
            post_apm_meter: Arc::new(AudioMeter::new("post-apm")),
            pre_playback_meter: Arc::new(AudioMeter::new("pre-playback")),
            sender_frames_sent: Arc::new(AtomicU64::new(0)),
            sender_frames_dropped: Arc::new(AtomicU64::new(0)),
            sender_max_backlog_frames: Arc::new(AtomicU64::new(0)),
            apm_mode: Arc::new(Mutex::new(ApmMode::Bypass)),
            rtcp_task: Arc::new(Mutex::new(None)),
            denoise: Arc::new(DenoiseFilter::new(denoise_enabled)),
            post_denoise_meter: Arc::new(AudioMeter::new("post-denoise")),
        }
    }
}

impl PeerConnectionBackend for WebRtcPeerConnectionBackend {
    fn create_peer_connection(&self, ice_config: &IceConfig) -> Result<(), CallError> {
        let rtc_config = ice_config.to_rtc_config();

        let pc = block_in_place(|| {
            self.rt_handle.block_on(async {
                let mut media_engine = MediaEngine::default();
                media_engine
                    .register_default_codecs()
                    .map_err(|e| CallError::NegotiationFailed(format!("media engine: {e}")))?;

                let mut registry = Registry::new();
                registry = register_default_interceptors(registry, &mut media_engine)
                    .map_err(|e| CallError::NegotiationFailed(format!("interceptors: {e}")))?;

                let api = APIBuilder::new()
                    .with_media_engine(media_engine)
                    .with_interceptor_registry(registry)
                    .build();

                api.new_peer_connection(rtc_config)
                    .await
                    .map_err(|e| CallError::NegotiationFailed(format!("create PC: {e}")))
            })
        })?;

        let pc = Arc::new(pc);

        // Create local audio track (Opus 48kHz mono)
        let local_track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: AUDIO_MIME_TYPE.to_owned(),
                clock_rate: AUDIO_CLOCK_RATE,
                channels: 1,
                ..Default::default()
            },
            "audio".to_owned(),
            "wavis-mic".to_owned(),
        ));

        // Set up on_track handler for receiving remote audio.
        // Uses AdaptiveJitterBuffer + OpusDecode with PLC.
        let playback_buf = self.playback_buffer.clone();
        let active = Arc::clone(&self.active);
        let shared_jitter = Arc::clone(&self.shared_jitter);
        let net_monitor_handle = Arc::clone(&self.net_monitor_handle);
        let pre_playback_meter = Arc::clone(&self.pre_playback_meter);

        // Create the shared jitter buffer.
        let jitter_buffer: Box<dyn JitterBuffering> = Box::new(AdaptiveJitterBuffer::new());
        *recover_lock(&shared_jitter) = Some(jitter_buffer);

        // Shared map of SR send instants for RTCP RTT computation (Option B).
        // Key: NTP timestamp compact (middle 32 bits) from the SR we sent.
        // Value: local Instant when we sent it.
        // In webrtc-rs 0.11, we cannot intercept outgoing SRs directly, so we
        // track the local Instant when we first see an RR referencing a given LSR.
        // This is populated lazily — see the RTCP reader task below.
        let sr_send_instants: Arc<Mutex<HashMap<u32, Instant>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let rtcp_task_handle = Arc::clone(&self.rtcp_task);

        let pc_clone = Arc::clone(&pc);
        block_in_place(|| {
            self.rt_handle.block_on(async {
                let active_inner = active;
                let playback_inner = playback_buf;
                let jitter_inner = shared_jitter;
                let net_handle_inner = net_monitor_handle;
                let meter_inner = pre_playback_meter;

                pc_clone.on_track(Box::new(move |track, receiver, _transceiver| {
                    let track = Arc::clone(&track);
                    let receiver = Arc::clone(&receiver);
                    let pb = playback_inner.clone();
                    let act = Arc::clone(&active_inner);
                    let jitter = Arc::clone(&jitter_inner);
                    let net_handle = Arc::clone(&net_handle_inner);
                    let net_handle_rtcp = Arc::clone(&net_handle);
                    let meter = Arc::clone(&meter_inner);
                    let sr_instants = Arc::clone(&sr_send_instants);
                    let act_rtcp = Arc::clone(&act);
                    let rtcp_task = Arc::clone(&rtcp_task_handle);

                    info!("Remote track received: kind={}", track.kind());

                    Box::pin(async move {
                        // Spawn background RTCP reader task for RTT extraction.
                        let rtcp_handle = tokio::spawn(webrtc_loops::run_rtcp_reader(
                            receiver,
                            act_rtcp,
                            net_handle_rtcp,
                            sr_instants,
                        ));

                        *recover_lock(&rtcp_task) = Some(rtcp_handle);

                        // Run the receive loop (RTP → decode → playback).
                        webrtc_loops::run_receive_loop(track, pb, act, jitter, net_handle, meter)
                            .await;
                    })
                }));
            })
        });

        // Set up ICE candidate handler
        let ice_cb = Arc::clone(&self.ice_cb);
        block_in_place(|| {
            self.rt_handle.block_on(async {
                pc.on_ice_candidate(Box::new(move |candidate| {
                    if let Some(candidate) = candidate {
                        let json = candidate.to_json();
                        match json {
                            Ok(init) => {
                                let ice = IceCandidate {
                                    candidate: init.candidate,
                                    sdp_mid: init.sdp_mid.unwrap_or_default(),
                                    sdp_mline_index: init.sdp_mline_index.unwrap_or(0),
                                };
                                let cb = recover_lock(&ice_cb);
                                if let Some(ref f) = *cb {
                                    f(ice);
                                }
                            }
                            Err(e) => {
                                warn!("Failed to serialize ICE candidate: {}", e);
                            }
                        }
                    }
                    Box::pin(async {})
                }));
            })
        });

        // Set up ICE connection state handler
        let conn_cb = Arc::clone(&self.conn_state_cb);
        block_in_place(|| {
            self.rt_handle.block_on(async {
                pc.on_ice_connection_state_change(Box::new(move |state| {
                    let mapped = match state {
                        RTCIceConnectionState::New => ConnectionState::New,
                        RTCIceConnectionState::Checking => ConnectionState::Checking,
                        RTCIceConnectionState::Connected => ConnectionState::Connected,
                        RTCIceConnectionState::Completed => ConnectionState::Completed,
                        RTCIceConnectionState::Failed => ConnectionState::Failed,
                        RTCIceConnectionState::Disconnected => ConnectionState::Disconnected,
                        RTCIceConnectionState::Closed => ConnectionState::Closed,
                        _ => ConnectionState::New,
                    };
                    info!("ICE connection state: {:?}", mapped);
                    let cb = recover_lock(&conn_cb);
                    if let Some(ref f) = *cb {
                        f(mapped);
                    }
                    Box::pin(async {})
                }));
            })
        });

        *recover_lock(&self.local_track) = Some(local_track);
        *recover_lock(&self.pc) = Some(pc);
        *recover_lock(&self.active) = true;

        Ok(())
    }

    fn add_audio_track(&self, _track: &AudioTrack) -> Result<(), CallError> {
        let pc = recover_lock(&self.pc)
            .clone()
            .ok_or(CallError::NoActiveCall)?;
        let local_track = recover_lock(&self.local_track)
            .clone()
            .ok_or(CallError::NoActiveCall)?;

        block_in_place(|| {
            self.rt_handle.block_on(async {
                pc.add_track(
                    local_track as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>,
                )
                .await
                .map_err(|e| CallError::NegotiationFailed(format!("add track: {e}")))?;
                Ok::<(), CallError>(())
            })
        })?;

        // Start the audio send loop and control loop.
        self.start_audio_send_loop();
        self.start_control_loop();

        Ok(())
    }

    fn create_offer(&self) -> Result<String, CallError> {
        let pc = recover_lock(&self.pc)
            .clone()
            .ok_or(CallError::NoActiveCall)?;

        block_in_place(|| {
            self.rt_handle.block_on(async {
                let offer = pc
                    .create_offer(None)
                    .await
                    .map_err(|e| CallError::NegotiationFailed(format!("create offer: {e}")))?;

                pc.set_local_description(offer.clone())
                    .await
                    .map_err(|e| CallError::NegotiationFailed(format!("set local desc: {e}")))?;

                Ok(offer.sdp)
            })
        })
    }

    fn create_answer(&self, offer_sdp: &str) -> Result<String, CallError> {
        let pc = recover_lock(&self.pc)
            .clone()
            .ok_or(CallError::NoActiveCall)?;

        let offer_sdp = offer_sdp.to_string();
        block_in_place(|| {
            self.rt_handle.block_on(async {
                let offer = RTCSessionDescription::offer(offer_sdp)
                    .map_err(|e| CallError::NegotiationFailed(format!("parse offer: {e}")))?;

                pc.set_remote_description(offer)
                    .await
                    .map_err(|e| CallError::NegotiationFailed(format!("set remote desc: {e}")))?;

                let answer = pc
                    .create_answer(None)
                    .await
                    .map_err(|e| CallError::NegotiationFailed(format!("create answer: {e}")))?;

                pc.set_local_description(answer.clone())
                    .await
                    .map_err(|e| CallError::NegotiationFailed(format!("set local desc: {e}")))?;

                Ok(answer.sdp)
            })
        })
    }

    fn set_remote_answer(&self, answer_sdp: &str) -> Result<(), CallError> {
        let pc = recover_lock(&self.pc)
            .clone()
            .ok_or(CallError::NoActiveCall)?;

        let answer_sdp = answer_sdp.to_string();
        block_in_place(|| {
            self.rt_handle.block_on(async {
                let answer = RTCSessionDescription::answer(answer_sdp)
                    .map_err(|e| CallError::NegotiationFailed(format!("parse answer: {e}")))?;

                pc.set_remote_description(answer)
                    .await
                    .map_err(|e| CallError::NegotiationFailed(format!("set remote desc: {e}")))?;

                Ok(())
            })
        })
    }

    fn add_ice_candidate(&self, candidate: &IceCandidate) -> Result<(), CallError> {
        let pc = recover_lock(&self.pc)
            .clone()
            .ok_or(CallError::NoActiveCall)?;

        let init = RTCIceCandidateInit {
            candidate: candidate.candidate.clone(),
            sdp_mid: Some(candidate.sdp_mid.clone()),
            sdp_mline_index: Some(candidate.sdp_mline_index),
            username_fragment: None,
        };

        block_in_place(|| {
            self.rt_handle.block_on(async {
                pc.add_ice_candidate(init)
                    .await
                    .map_err(|e| CallError::NegotiationFailed(format!("add ICE: {e}")))?;
                Ok(())
            })
        })
    }

    fn on_ice_candidate(&self, cb: Box<dyn Fn(IceCandidate) + Send + 'static>) {
        *recover_lock(&self.ice_cb) = Some(cb);
    }

    fn on_connection_state_change(&self, cb: Box<dyn Fn(ConnectionState) + Send + 'static>) {
        *recover_lock(&self.conn_state_cb) = Some(cb);
    }

    fn close(&self) -> Result<(), CallError> {
        *recover_lock(&self.active) = false;

        // Abort the send task.
        if let Some(handle) = recover_lock(&self.send_task).take() {
            handle.abort();
        }

        // Abort the control loop task.
        if let Some(handle) = recover_lock(&self.control_task).take() {
            handle.abort();
        }

        // Abort the RTCP reader task.
        if let Some(handle) = recover_lock(&self.rtcp_task).take() {
            handle.abort();
        }

        let pc = recover_lock(&self.pc).take();
        if let Some(pc) = pc {
            block_in_place(|| {
                self.rt_handle.block_on(async {
                    let _ = pc.close().await;
                })
            });
        }

        *recover_lock(&self.local_track) = None;
        *recover_lock(&self.shared_encoder) = None;
        *recover_lock(&self.shared_jitter) = None;
        info!("PeerConnection closed");
        Ok(())
    }

    fn is_active(&self) -> bool {
        *recover_lock(&self.active)
    }
}
