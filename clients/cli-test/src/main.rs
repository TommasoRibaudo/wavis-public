//! Wavis CLI test client — minimal binary for manual P2P voice testing.
//!
//! Usage:
//!   wavis-cli-test --server ws://127.0.0.1:3000/ws --room <room-id>
//!
//! Or for local loopback testing without a signaling server:
//!   wavis-cli-test --loopback

use log::{error, info, warn};
use shared::signaling::{self, CreateRoomPayload, JoinPayload, SignalingMessage};
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio_tungstenite::{connect_async, connect_async_tls_with_config};
use wavis_cli_test::{
    parse_args, wait_for_joined_result, wait_for_room_created, CliArgs, CreateResult, TungsteniteWs,
};
use wavis_client_shared::call_session::CallSession;
use wavis_client_shared::cpal_audio::CpalAudioBackend;
use wavis_client_shared::ice_config::IceConfig;
use wavis_client_shared::signaling::WebSocketConnection;
use wavis_client_shared::webrtc::{CallManager, CallState, ConnectionState};
use wavis_client_shared::webrtc_backend::WebRtcPeerConnectionBackend;

fn default_ice_config() -> IceConfig {
    // Try loading from environment variables first (supports TURN config)
    match IceConfig::load() {
        Ok(config) => {
            info!("ICE config loaded from environment");
            config
        }
        Err(_) => {
            info!("Using default STUN-only ICE config (set WAVIS_STUN_URLS / WAVIS_TURN_URLS for TURN)");
            IceConfig {
                stun_urls: vec!["stun:stun.l.google.com:19302".to_string()],
                turn_urls: vec![],
                turn_username: String::new(),
                turn_credential: String::new(),
            }
        }
    }
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let raw_args: Vec<String> = env::args().skip(1).collect();

    match parse_args(&raw_args) {
        CliArgs::Loopback => run_loopback().await,
        CliArgs::ServerMode {
            server_url,
            room_id,
            invite_code,
            danger_insecure_tls,
        } => {
            run_server_mode(
                &server_url,
                &room_id,
                invite_code.as_deref(),
                danger_insecure_tls,
            )
            .await
        }
        CliArgs::SfuMode {
            server_url,
            room_id,
            invite_code,
            danger_insecure_tls,
        } => {
            run_sfu_mode(
                &server_url,
                &room_id,
                invite_code.as_deref(),
                danger_insecure_tls,
            )
            .await
        }
        CliArgs::MissingRoom { .. } => {
            eprintln!("--room is required for server/sfu mode");
            std::process::exit(1);
        }
        CliArgs::ShowUsage => {
            eprintln!("Usage:");
            eprintln!("  wavis-cli-test --loopback");
            eprintln!("  wavis-cli-test --server <url> --room <room-id> [--invite <code>]");
            eprintln!("  wavis-cli-test --server <url> --room <room-id> --sfu [--invite <code>]");
            eprintln!("  wavis-cli-test --server <url> --room <room-id> --danger-insecure-tls");
            std::process::exit(1);
        }
    }
}

/// Connect to a WebSocket server, optionally disabling TLS certificate validation.
async fn ws_connect(
    server_url: &str,
    danger_insecure_tls: bool,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    if danger_insecure_tls {
        eprintln!("WARNING: TLS certificate validation is DISABLED. Do not use in production.");
        let connector = wavis_cli_test::insecure_tls_connector();
        let (ws_stream, _) =
            connect_async_tls_with_config(server_url, None, false, Some(connector))
                .await
                .unwrap_or_else(|e| {
                    error!("WebSocket connection failed: {e}");
                    std::process::exit(1);
                });
        ws_stream
    } else {
        let (ws_stream, _) = connect_async(server_url).await.unwrap_or_else(|e| {
            error!("WebSocket connection failed: {e}");
            std::process::exit(1);
        });
        ws_stream
    }
}

async fn run_server_mode(
    server_url: &str,
    room_id: &str,
    invite_code: Option<&str>,
    danger_insecure_tls: bool,
) {
    // 1. Connect
    info!("connecting to {}", server_url);
    let ws_stream = ws_connect(server_url, danger_insecure_tls).await;
    info!("connected");

    // 2. Create TungsteniteWs + incoming receiver
    let (ws, mut incoming_rx) = TungsteniteWs::new(ws_stream);

    // 3. If invite code provided, join existing room. Otherwise, create a new one.
    let is_creator = if let Some(code) = invite_code {
        info!("joining room '{}' with invite code", room_id);
        let join_msg = signaling::to_json(&SignalingMessage::Join(JoinPayload {
            room_id: room_id.to_string(),
            room_type: Some("p2p".to_string()),
            invite_code: Some(code.to_string()),
            display_name: None,
            profile_color: None,
        }))
        .unwrap();
        ws.send_text(&join_msg).unwrap();

        let joined = match wait_for_joined_result(
            &mut incoming_rx,
            std::time::Duration::from_secs(10),
        )
        .await
        {
            wavis_cli_test::JoinResult::Joined(p) => p,
            wavis_cli_test::JoinResult::ServerError(e) => {
                error!("server error during join: {e}");
                std::process::exit(1);
            }
            wavis_cli_test::JoinResult::ChannelClosed => {
                error!("WebSocket closed before Joined received");
                std::process::exit(1);
            }
            wavis_cli_test::JoinResult::TimedOut => {
                error!("timed out waiting for Joined response (10s)");
                std::process::exit(1);
            }
        };
        info!(
            "joined room {} as peer {} (peer_count={})",
            joined.room_id, joined.peer_id, joined.peer_count
        );
        false
    } else {
        info!("creating room '{}'", room_id);
        let create_msg = signaling::to_json(&SignalingMessage::CreateRoom(CreateRoomPayload {
            room_id: room_id.to_string(),
            room_type: Some("p2p".to_string()),
            display_name: None,
            profile_color: None,
        }))
        .unwrap();
        ws.send_text(&create_msg).unwrap();

        let created =
            match wait_for_room_created(&mut incoming_rx, std::time::Duration::from_secs(10)).await
            {
                CreateResult::Created(p) => p,
                CreateResult::ServerError(e) => {
                    error!("server error during create: {e}");
                    std::process::exit(1);
                }
                CreateResult::ChannelClosed => {
                    error!("WebSocket closed before RoomCreated received");
                    std::process::exit(1);
                }
                CreateResult::TimedOut => {
                    error!("timed out waiting for RoomCreated response (10s)");
                    std::process::exit(1);
                }
            };
        info!(
            "created room {} as peer {} (invite_code={})",
            created.room_id, created.peer_id, created.invite_code
        );
        info!(
            ">>> Share this invite code with the second client: {}",
            created.invite_code
        );
        true
    };

    // 5. Create CallSession
    let ice_config = default_ice_config();
    let audio = CpalAudioBackend::new();
    let pc_backend = WebRtcPeerConnectionBackend::new(&audio, true);
    let session = CallSession::new(audio, pc_backend, ice_config, ws);

    // 6. Wire state change logging + shutdown flag
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);
    session.on_call_state_changed(move |state| match state {
        CallState::Connected => info!("audio is flowing"),
        CallState::Failed => {
            error!("call failed");
            shutdown_clone.store(true, Ordering::Release);
        }
        CallState::Closed => {
            info!("remote peer left");
            shutdown_clone.store(true, Ordering::Release);
        }
        _ => {}
    });

    // 7. Creator waits for a peer; joiner waits for the offer
    let call_initiated = Arc::new(AtomicBool::new(false));
    if is_creator {
        info!("waiting for peer to join");
    } else {
        info!("waiting for peer to initiate call");
    }

    // 8. Event loop
    loop {
        if shutdown.load(Ordering::Acquire) {
            info!("shutting down");
            let _ = session.end_call();
            break;
        }
        tokio::select! {
            msg = incoming_rx.recv() => {
                match msg {
                    Some(text) => {
                        // Check for Joined notification (peer 2 arrived while we were waiting)
                        if !call_initiated.load(Ordering::Acquire) {
                            if let Ok(SignalingMessage::Joined(payload)) = signaling::parse(&text) {
                                if payload.peer_count >= 2 {
                                    info!("peer arrived (peer_count={}), initiating call", payload.peer_count);
                                    if let Err(e) = session.initiate_call() {
                                        error!("failed to initiate call: {e}");
                                        std::process::exit(1);
                                    }
                                    call_initiated.store(true, Ordering::Release);
                                    continue;
                                }
                            }
                        }
                        if let Err(e) = session.handle_incoming(&text) {
                            warn!("failed to handle message: {e}");
                        }
                    }
                    None => {
                        error!("WebSocket connection dropped");
                        std::process::exit(1);
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("shutting down");
                let _ = session.end_call();
                break;
            }
        }
    }
}

/// SFU mode: connect to backend, join an SFU room, and let RoomSession +
/// RealLiveKitConnection handle the LiveKit media path. You should hear
/// other participants (and yourself if LiveKit echoes your track back).
#[cfg(feature = "livekit")]
async fn run_sfu_mode(
    server_url: &str,
    room_id: &str,
    invite_code: Option<&str>,
    danger_insecure_tls: bool,
) {
    use wavis_client_shared::audio::AudioBackend;
    use wavis_client_shared::audio_network_monitor::new_network_monitor;
    use wavis_client_shared::livekit_connection::RealLiveKitConnection;
    use wavis_client_shared::room_session::{LiveKitConnection, RoomSession};

    // 1. Connect WebSocket
    info!("connecting to {} (SFU mode)", server_url);
    let ws_stream = ws_connect(server_url, danger_insecure_tls).await;
    info!("connected");

    let (ws, mut incoming_rx) = TungsteniteWs::new(ws_stream);

    // 2. Create audio backend and start speaker playback early so incoming
    //    audio frames have somewhere to go.
    let audio = CpalAudioBackend::new();
    if let Err(e) = audio.play_remote(wavis_client_shared::audio::AudioTrack {
        id: "livekit-remote".to_string(),
    }) {
        warn!("failed to open speaker: {e}");
    }

    // 3. Create RoomSession with RealLiveKitConnection
    let ice_config = default_ice_config();
    let pc_backend = WebRtcPeerConnectionBackend::new(&audio, true);
    let lk = RealLiveKitConnection::new();

    // Give the LiveKit connection access to the mic capture buffer so
    // publish_audio() can push real PCM into the NativeAudioSource.
    lk.set_capture_buffer(audio.capture_buffer.clone());
    lk.set_playback_buffer(audio.playback_buffer.clone());

    // Wire network monitor so LiveKit transport stats (RTT, loss, jitter)
    // flow into the unified Pipeline: telemetry line.
    let (_monitor, net_handle) = new_network_monitor();
    lk.set_network_monitor(net_handle);

    // Trace-log incoming LiveKit audio frames (playback is handled by
    // set_playback_buffer — do NOT write to the buffer here to avoid double-write).
    lk.on_audio_frame(Box::new(move |participant_id, samples| {
        log::trace!("audio from {participant_id}: {} samples", samples.len());
    }));

    let repl_ws = ws.clone();
    let session = RoomSession::with_livekit(audio, pc_backend, ice_config, ws, lk);

    // 4. If invite code provided, join existing room. Otherwise, create a new one.
    if let Some(code) = invite_code {
        info!("joining room '{}' with invite code (SFU)", room_id);
        if let Err(e) = session.join_room(room_id, Some(code)) {
            error!("failed to join room: {e}");
            std::process::exit(1);
        }
        info!("joined room '{}'", room_id);
    } else {
        info!("creating room '{}' as SFU", room_id);
        let create_msg = signaling::to_json(&SignalingMessage::CreateRoom(CreateRoomPayload {
            room_id: room_id.to_string(),
            room_type: Some("sfu".to_string()),
            display_name: None,
            profile_color: None,
        }))
        .unwrap();
        repl_ws.send_text(&create_msg).unwrap();

        let created =
            match wait_for_room_created(&mut incoming_rx, std::time::Duration::from_secs(10)).await
            {
                CreateResult::Created(p) => p,
                CreateResult::ServerError(e) => {
                    error!("server error during create: {e}");
                    std::process::exit(1);
                }
                CreateResult::ChannelClosed => {
                    error!("WebSocket closed before RoomCreated received");
                    std::process::exit(1);
                }
                CreateResult::TimedOut => {
                    error!("timed out waiting for RoomCreated response (10s)");
                    std::process::exit(1);
                }
            };

        info!(
            "created room {} as peer {} (invite_code={})",
            created.room_id, created.peer_id, created.invite_code
        );
        info!(
            ">>> Share this invite code with the second client: {}",
            created.invite_code
        );

        // Start media without sending Join — backend already joined us via CreateRoom
        if let Err(e) = session.start_media() {
            error!("failed to start media: {e}");
            std::process::exit(1);
        }
    }

    // 5. Log participant events
    session.on_participant_joined(|info| {
        info!(
            "participant joined: {} ({})",
            info.display_name, info.participant_id
        );
    });
    session.on_participant_left(|id| {
        info!("participant left: {id}");
    });
    session.on_room_state(|participants| {
        info!("room state: {} participant(s)", participants.len());
        for p in &participants {
            info!("  - {} ({})", p.display_name, p.participant_id);
        }
    });

    // 6. Register share event callbacks
    session.on_share_started(|id| {
        info!("participant {} started sharing", id);
    });
    session.on_share_stopped(|id| {
        info!("participant {} stopped sharing", id);
    });
    session.on_share_state(|ids| {
        info!("active sharers: {:?}", ids);
    });

    info!("waiting for MediaToken from backend (LiveKit credentials)...");
    info!("commands: start-share | stop-share [<id>] | stop-all-shares");
    info!("press Ctrl+C to leave");

    // 7. Event loop — feed incoming WS messages to RoomSession + REPL
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut lines = tokio::io::AsyncBufReadExt::lines(stdin);

    loop {
        tokio::select! {
            msg = incoming_rx.recv() => {
                match msg {
                    Some(text) => {
                        // Log interesting messages
                        if text.contains("\"media_token\"") {
                            info!("received MediaToken — connecting to LiveKit...");
                        }
                        if text.contains("\"error\"") {
                            warn!("server error: {text}");
                        }
                        if text.contains("\"join_rejected\"") {
                            error!("join rejected: {text}");
                        }
                        // Log share-related messages from raw text
                        if text.contains("\"share_started\"") {
                            if let Ok(SignalingMessage::ShareStarted(ref p)) = signaling::parse(&text) {
                                info!("participant {} started sharing", p.participant_id);
                            }
                        }
                        if text.contains("\"share_stopped\"") {
                            if let Ok(SignalingMessage::ShareStopped(ref p)) = signaling::parse(&text) {
                                info!("participant {} stopped sharing", p.participant_id);
                            }
                        }
                        if text.contains("\"share_state\"") {
                            if let Ok(SignalingMessage::ShareState(ref p)) = signaling::parse(&text) {
                                info!("active sharers: {:?}", p.participant_ids);
                            }
                        }
                        if let Err(e) = session.handle_incoming(&text) {
                            warn!("failed to handle message: {e}");
                        }
                        // Check if we switched to LiveKit mode
                        if matches!(session.sfu_mode(), wavis_client_shared::room_session::SfuConnectionMode::LiveKit { .. }) {
                            // Only log once — the mode won't flip back
                            static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                            if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                                info!("LiveKit connected — audio should be flowing");
                            }
                        }
                    }
                    None => {
                        error!("WebSocket connection dropped");
                        break;
                    }
                }
            }
            line = lines.next_line() => {
                match line {
                    Ok(Some(input)) => {
                        let trimmed = input.trim();
                        match trimmed {
                            "start-share" => {
                                let json = r#"{"type":"start_share"}"#;
                                if let Err(e) = repl_ws.send_text(json) {
                                    warn!("failed to send start-share: {e}");
                                }
                            }
                            "stop-share" => {
                                let json = r#"{"type":"stop_share"}"#;
                                if let Err(e) = repl_ws.send_text(json) {
                                    warn!("failed to send stop-share: {e}");
                                }
                            }
                            "stop-all-shares" => {
                                let json = r#"{"type":"stop_all_shares"}"#;
                                if let Err(e) = repl_ws.send_text(json) {
                                    warn!("failed to send stop-all-shares: {e}");
                                }
                            }
                            cmd if cmd.starts_with("stop-share ") => {
                                let target_id = cmd.strip_prefix("stop-share ").unwrap().trim();
                                if target_id.is_empty() {
                                    warn!("usage: stop-share <participant_id>");
                                } else {
                                    let json = format!(
                                        r#"{{"type":"stop_share","targetParticipantId":"{}"}}"#,
                                        target_id
                                    );
                                    if let Err(e) = repl_ws.send_text(&json) {
                                        warn!("failed to send stop-share: {e}");
                                    }
                                }
                            }
                            "" => {} // ignore empty lines
                            _ => {
                                info!("unknown command: {trimmed}");
                                info!("commands: start-share | stop-share [<id>] | stop-all-shares");
                            }
                        }
                    }
                    Ok(None) => {
                        // stdin closed
                        break;
                    }
                    Err(e) => {
                        warn!("stdin read error: {e}");
                        break;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("leaving room...");
                let _ = session.leave_room();
                break;
            }
        }
    }

    info!("done");
}

#[cfg(not(feature = "livekit"))]
async fn run_sfu_mode(
    _server_url: &str,
    _room_id: &str,
    _invite_code: Option<&str>,
    _danger_insecure_tls: bool,
) {
    error!("SFU mode requires the 'livekit' feature. Rebuild with:");
    error!(
        "  cargo run -p wavis-cli-test --features livekit -- --server <url> --room <room> --sfu"
    );
    std::process::exit(1);
}

/// Loopback test: two peers in the same process, audio goes
/// mic → peer A → WebRTC → peer B → speaker.
async fn run_loopback() {
    info!("=== Wavis Loopback Audio Test ===");
    info!("This will capture your mic and play it back through speakers.");
    info!("You should hear yourself with a slight delay.");
    info!("Press Ctrl+C to stop.");
    info!("");

    let ice_config = default_ice_config();

    let audio_a = CpalAudioBackend::new();
    let pc_a = WebRtcPeerConnectionBackend::new(&audio_a, true);
    let manager_a = Arc::new(CallManager::new(audio_a, pc_a, ice_config.clone()));

    let audio_b = CpalAudioBackend::new();
    let pc_b = WebRtcPeerConnectionBackend::new(&audio_b, true);
    let manager_b = Arc::new(CallManager::new(audio_b, pc_b, ice_config));

    let state_a: Arc<Mutex<CallState>> = Arc::new(Mutex::new(CallState::Idle));
    let state_b: Arc<Mutex<CallState>> = Arc::new(Mutex::new(CallState::Idle));

    let sa = Arc::clone(&state_a);
    manager_a.on_connection_state(move |s| {
        info!("[Peer A] Connection state: {:?}", s);
        let call_state = match s {
            ConnectionState::Connected | ConnectionState::Completed => CallState::Connected,
            ConnectionState::Failed => CallState::Failed,
            ConnectionState::Checking => CallState::Connecting,
            _ => return,
        };
        *sa.lock().unwrap() = call_state;
    });

    let sb = Arc::clone(&state_b);
    manager_b.on_connection_state(move |s| {
        info!("[Peer B] Connection state: {:?}", s);
        let call_state = match s {
            ConnectionState::Connected | ConnectionState::Completed => CallState::Connected,
            ConnectionState::Failed => CallState::Failed,
            ConnectionState::Checking => CallState::Connecting,
            _ => return,
        };
        *sb.lock().unwrap() = call_state;
    });

    let ice_from_a: Arc<Mutex<Vec<shared::signaling::IceCandidate>>> =
        Arc::new(Mutex::new(Vec::new()));
    let ice_from_b: Arc<Mutex<Vec<shared::signaling::IceCandidate>>> =
        Arc::new(Mutex::new(Vec::new()));

    let ia = Arc::clone(&ice_from_a);
    manager_a.on_ice_candidate(move |c| {
        info!("[Peer A] ICE candidate gathered");
        ia.lock().unwrap().push(c);
    });

    let ib = Arc::clone(&ice_from_b);
    manager_b.on_ice_candidate(move |c| {
        info!("[Peer B] ICE candidate gathered");
        ib.lock().unwrap().push(c);
    });

    info!("Step 1: Peer A creating offer...");
    let offer_sdp = match manager_a.start_call() {
        Ok(sdp) => sdp,
        Err(e) => {
            error!("Failed to start call: {}", e);
            return;
        }
    };
    info!("Offer created ({} bytes)", offer_sdp.len());

    info!("Step 2: Peer B accepting offer...");
    let answer_sdp = match manager_b.accept_call(&offer_sdp) {
        Ok(sdp) => sdp,
        Err(e) => {
            error!("Failed to accept call: {}", e);
            return;
        }
    };
    info!("Answer created ({} bytes)", answer_sdp.len());

    info!("Step 3: Peer A setting remote answer...");
    if let Err(e) = manager_a.set_answer(&answer_sdp) {
        error!("Failed to set answer: {}", e);
        return;
    }

    info!("Step 4: Exchanging ICE candidates...");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let candidates_a = ice_from_a.lock().unwrap().clone();
    info!("Peer A gathered {} candidates", candidates_a.len());
    for c in &candidates_a {
        if let Err(e) = manager_b.add_ice_candidate(c) {
            error!("Failed to add A's ICE candidate to B: {}", e);
        }
    }

    let candidates_b = ice_from_b.lock().unwrap().clone();
    info!("Peer B gathered {} candidates", candidates_b.len());
    for c in &candidates_b {
        if let Err(e) = manager_a.add_ice_candidate(c) {
            error!("Failed to add B's ICE candidate to A: {}", e);
        }
    }

    info!("Step 5: Waiting for ICE connection...");
    let timeout = std::time::Duration::from_secs(15);
    let start = std::time::Instant::now();

    loop {
        let sa = *state_a.lock().unwrap();
        let sb = *state_b.lock().unwrap();

        if sa == CallState::Connected && sb == CallState::Connected {
            info!("=== CONNECTED ===");
            info!("Audio should be flowing. Press Ctrl+C to stop.");
            break;
        }

        if sa == CallState::Failed || sb == CallState::Failed {
            error!("ICE connection failed.");
            let _ = manager_a.hangup();
            let _ = manager_b.hangup();
            return;
        }

        if start.elapsed() > timeout {
            error!("Timed out waiting for ICE connection.");
            let _ = manager_a.hangup();
            let _ = manager_b.hangup();
            return;
        }

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    tokio::signal::ctrl_c().await.ok();
    info!("Shutting down...");
    let _ = manager_a.hangup();
    let _ = manager_b.hangup();
    info!("Done.");
}
