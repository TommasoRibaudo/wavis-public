use shared::signaling::{self, ParticipantInfo, SignalingMessage};
use tokio::sync::mpsc;
use wavis_cli_test::TungsteniteWs;
use wavis_client_shared::audio::AudioBackend;
use wavis_client_shared::cpal_audio::{AudioBuffer, CpalAudioBackend, PeerVolumes, DEFAULT_VOLUME};
use wavis_client_shared::ice_config::IceConfig;
use wavis_client_shared::room_session::{RoomSession, SfuConnectionMode};
use wavis_client_shared::signaling::WebSocketConnection;
use wavis_client_shared::webrtc_backend::WebRtcPeerConnectionBackend;

use crate::commands::{self, Command, ParseResult};
use crate::output;

// --- LiveKit type aliases (feature-gated) ---

#[cfg(feature = "livekit")]
use wavis_client_shared::livekit_connection::RealLiveKitConnection;

#[cfg(feature = "livekit")]
type LiveKitBackend = RealLiveKitConnection;

#[cfg(not(feature = "livekit"))]
use wavis_client_shared::room_session::NoLiveKit;

#[cfg(not(feature = "livekit"))]
type LiveKitBackend = NoLiveKit;

/// Concrete RoomSession type used by the REPL.
type CliRoomSession =
    RoomSession<CpalAudioBackend, WebRtcPeerConnectionBackend, TungsteniteWs, LiveKitBackend>;

/// The client's connection phase.
#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Lobby,
    Joining,
    InRoom,
}

/// Tracks the client's current session state within the REPL.
struct ClientState {
    /// Current phase.
    phase: Phase,
    /// Current room ID (set during Joining, cleared on leave).
    room_id: Option<String>,
    /// Server-assigned peer ID (set on Joined).
    peer_id: Option<String>,
    /// Known participants (updated via ParticipantJoined/Left events).
    participants: Vec<ParticipantInfo>,
    /// Current SFU connection mode, read from `RoomSession::sfu_mode()`.
    sfu_mode: Option<SfuConnectionMode>,
    /// True while waiting for an InviteCreated response.
    pending_invite: bool,
    /// Set to the invite code while waiting for an InviteRevoked response.
    pending_revoke: Option<String>,
    /// True while waiting for a RoomCreated response.
    pending_create: bool,
    /// The active RoomSession (created lazily on create/join).
    session: Option<CliRoomSession>,
    /// User-chosen display name (local only, sent with future joins).
    display_name: String,
    /// Handle to the playback buffer for volume control.
    playback_buffer: Option<AudioBuffer>,
    /// Per-peer volume map (shared with the LiveKit audio path).
    peer_volumes: PeerVolumes,
}

impl ClientState {
    fn new() -> Self {
        Self {
            phase: Phase::Lobby,
            room_id: None,
            peer_id: None,
            participants: Vec::new(),
            sfu_mode: None,
            pending_invite: false,
            pending_revoke: None,
            pending_create: false,
            session: None,
            display_name: String::new(),
            playback_buffer: None,
            peer_volumes: PeerVolumes::new(),
        }
    }

    fn reset_to_lobby(&mut self) {
        self.phase = Phase::Lobby;
        self.room_id = None;
        self.peer_id = None;
        self.participants.clear();
        self.sfu_mode = None;
        self.pending_invite = false;
        self.pending_revoke = None;
        self.pending_create = false;
        self.session = None;
        self.playback_buffer = None;
        self.peer_volumes.clear();
    }
}

/// Run the interactive REPL loop.
///
/// Returns exit code: `0` for clean exit (quit/Ctrl+C), `2` for WebSocket drop.
pub async fn run_repl(
    ws: TungsteniteWs,
    mut incoming_rx: mpsc::Receiver<String>,
    _server_url: &str,
) -> i32 {
    let mut state = ClientState::new();

    // Read stdin on a dedicated OS thread — tokio::io::Stdin doesn't
    // work reliably on Windows terminals (keystrokes don't echo, lines
    // never arrive). A real std::thread with std::io::BufRead is the
    // standard workaround.
    let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(64);
    std::thread::spawn(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        let reader = stdin.lock();
        for line in reader.lines() {
            match line {
                Ok(l) => {
                    if stdin_tx.blocking_send(l).is_err() {
                        break; // receiver dropped, REPL is shutting down
                    }
                }
                Err(_) => break, // EOF or read error
            }
        }
    });

    // Print a prompt so the user knows the REPL is ready.
    eprint!("> ");

    loop {
        tokio::select! {
            // Branch 1: stdin — user commands
            line = stdin_rx.recv() => {
                match line {
                    Some(line) => {
                        let should_exit = handle_stdin_line(&line, &mut state, &ws);
                        if should_exit {
                            return 0;
                        }
                        eprint!("> ");
                    }
                    None => {
                        // EOF on stdin — treat as quit
                        return 0;
                    }
                }
            }

            // Branch 2: WebSocket incoming — signaling messages
            msg = incoming_rx.recv() => {
                match msg {
                    Some(text) => {
                        handle_incoming_message(&text, &mut state);
                    }
                    None => {
                        // Channel closed — WebSocket dropped
                        output::event("Disconnected from server");
                        return 2;
                    }
                }
            }

            // Branch 3: Ctrl+C — graceful shutdown
            _ = tokio::signal::ctrl_c() => {
                if matches!(state.phase, Phase::Joining | Phase::InRoom) {
                    handle_leave(&mut state, &ws);
                }
                return 0;
            }
        }
    }
}

/// Handle a single line of stdin input. Returns `true` if the REPL should exit.
fn handle_stdin_line(line: &str, state: &mut ClientState, ws: &TungsteniteWs) -> bool {
    match commands::parse_command(line) {
        ParseResult::Ok(cmd) => handle_command(cmd, state, ws),
        ParseResult::UnknownCommand(token) => {
            output::err(&format!(
                "Unknown command: '{}'. Type 'help' for available commands.",
                token
            ));
            false
        }
        ParseResult::WrongArgCount { command, usage } => {
            output::err(&format!(
                "Wrong arguments for '{}'. Usage: {}",
                command, usage
            ));
            false
        }
        ParseResult::EmptyInput => false,
    }
}

/// Dispatch a parsed command. Returns `true` if the REPL should exit.
fn handle_command(cmd: Command, state: &mut ClientState, ws: &TungsteniteWs) -> bool {
    match cmd {
        Command::Create { room_id } => {
            handle_create(&room_id, state, ws);
            false
        }
        Command::Join {
            room_id,
            invite_code,
        } => {
            handle_join(&room_id, &invite_code, state, ws);
            false
        }
        Command::Invite { max_uses } => {
            handle_invite(max_uses, state, ws);
            false
        }
        Command::Revoke { invite_code } => {
            handle_revoke(&invite_code, state, ws);
            false
        }
        Command::Leave => {
            handle_leave(state, ws);
            false
        }
        Command::Status => {
            handle_status(state);
            false
        }
        Command::Name { new_name } => {
            handle_name(&new_name, state);
            false
        }
        Command::Volume { level } => {
            handle_volume(level, state);
            false
        }
        Command::PeerVolume { peer, level } => {
            handle_peer_volume(&peer, level, state);
            false
        }
        Command::Help => {
            output::print_help();
            false
        }
        Command::Quit => {
            // Leave room if currently in one before exiting
            if matches!(state.phase, Phase::InRoom | Phase::Joining) {
                handle_leave(state, ws);
            }
            true
        }
    }
}

// --- Helper: construct RoomSession ---

/// Build the default ICE config from env vars, falling back to STUN-only.
fn default_ice_config() -> IceConfig {
    match IceConfig::load() {
        Ok(config) => {
            log::info!("ICE config loaded from environment");
            config
        }
        Err(_) => {
            log::info!("Using default STUN-only ICE config");
            IceConfig {
                stun_urls: vec!["stun:stun.l.google.com:19302".to_string()],
                turn_urls: vec![],
                turn_username: String::new(),
                turn_credential: String::new(),
            }
        }
    }
}

/// Create a `CliRoomSession` with audio, WebRTC, and LiveKit backends.
/// Returns `Err(String)` if audio initialization fails.
/// Also returns the playback `AudioBuffer` handle for volume control.
fn create_room_session(
    ws: &TungsteniteWs,
    peer_volumes: &PeerVolumes,
) -> Result<(CliRoomSession, AudioBuffer), String> {
    let _ = peer_volumes; // Used only with the `livekit` feature.
    let audio = CpalAudioBackend::new();

    // Set initial volume to default (70%).
    audio.playback_buffer.set_volume(DEFAULT_VOLUME);

    // Start speaker playback early so incoming audio has somewhere to go.
    if let Err(e) = audio.play_remote(wavis_client_shared::audio::AudioTrack {
        id: "livekit-remote".to_string(),
    }) {
        return Err(format!("Failed to open speaker: {e}"));
    }

    let playback_buf = audio.playback_buffer.clone();
    let ice_config = default_ice_config();
    let pc_backend = WebRtcPeerConnectionBackend::new(&audio, true);

    #[cfg(feature = "livekit")]
    {
        use wavis_client_shared::audio_network_monitor::new_network_monitor;
        use wavis_client_shared::room_session::LiveKitConnection;

        let lk = RealLiveKitConnection::new();
        lk.set_capture_buffer(audio.capture_buffer.clone());
        lk.set_playback_buffer(audio.playback_buffer.clone());

        let (_monitor, net_handle) = new_network_monitor();
        lk.set_network_monitor(net_handle);
        lk.set_peer_volumes(peer_volumes.clone());

        lk.on_audio_frame(Box::new(move |participant_id, samples| {
            log::trace!("audio from {participant_id}: {} samples", samples.len());
        }));

        Ok((
            RoomSession::with_livekit(audio, pc_backend, ice_config, ws.clone(), lk),
            playback_buf,
        ))
    }

    #[cfg(not(feature = "livekit"))]
    {
        Ok((
            RoomSession::new(audio, pc_backend, ice_config, ws.clone()),
            playback_buf,
        ))
    }
}

// --- Command handlers ---

fn handle_create(room_id: &str, state: &mut ClientState, ws: &TungsteniteWs) {
    if state.phase != Phase::Lobby {
        output::err("Already in a room. Use 'leave' first.");
        return;
    }

    let (session, playback_buf) = match create_room_session(ws, &state.peer_volumes) {
        Ok(s) => s,
        Err(e) => {
            output::err(&format!("Audio init failed: {e}"));
            return;
        }
    };

    let msg = SignalingMessage::CreateRoom(shared::signaling::CreateRoomPayload {
        room_id: room_id.to_string(),
        room_type: Some("sfu".to_string()),
        display_name: if state.display_name.is_empty() {
            None
        } else {
            Some(state.display_name.clone())
        },
        profile_color: None,
    });
    match signaling::to_json(&msg) {
        Ok(json) => {
            if let Err(e) = ws.send_text(&json) {
                output::err(&format!("Failed to send create room request: {e}"));
                return;
            }
            state.phase = Phase::Joining;
            state.room_id = Some(room_id.to_string());
            state.pending_create = true;
            state.session = Some(session);
            state.playback_buffer = Some(playback_buf);
        }
        Err(e) => output::err(&format!("Failed to serialize create room request: {e}")),
    }
}

fn handle_join(room_id: &str, invite_code: &str, state: &mut ClientState, ws: &TungsteniteWs) {
    if state.phase != Phase::Lobby {
        output::err("Already in a room. Use 'leave' first.");
        return;
    }

    let (session, playback_buf) = match create_room_session(ws, &state.peer_volumes) {
        Ok(s) => s,
        Err(e) => {
            output::err(&format!("Audio init failed: {e}"));
            return;
        }
    };

    let display_name_opt = if state.display_name.is_empty() {
        None
    } else {
        Some(state.display_name.as_str())
    };

    if let Err(e) = session.join_room_with_name(room_id, Some(invite_code), display_name_opt) {
        output::err(&format!("Failed to join room: {e}"));
        return;
    }

    state.phase = Phase::Joining;
    state.room_id = Some(room_id.to_string());
    state.session = Some(session);
    state.playback_buffer = Some(playback_buf);
}

fn handle_invite(max_uses: Option<u32>, state: &mut ClientState, ws: &TungsteniteWs) {
    if state.phase != Phase::InRoom {
        output::err("Not in a room. Join or create a room first.");
        return;
    }
    let msg = commands::build_invite_create(max_uses);
    match signaling::to_json(&msg) {
        Ok(json) => {
            if let Err(e) = ws.send_text(&json) {
                output::err(&format!("Failed to send invite request: {e}"));
                return;
            }
            state.pending_invite = true;
        }
        Err(e) => output::err(&format!("Failed to serialize invite request: {e}")),
    }
}

fn handle_revoke(invite_code: &str, state: &mut ClientState, ws: &TungsteniteWs) {
    if state.phase != Phase::InRoom {
        output::err("Not in a room. Join or create a room first.");
        return;
    }
    let msg = SignalingMessage::InviteRevoke(shared::signaling::InviteRevokePayload {
        invite_code: invite_code.to_string(),
    });
    match signaling::to_json(&msg) {
        Ok(json) => {
            if let Err(e) = ws.send_text(&json) {
                output::err(&format!("Failed to send revoke request: {e}"));
                return;
            }
            state.pending_revoke = Some(invite_code.to_string());
        }
        Err(e) => output::err(&format!("Failed to serialize revoke request: {e}")),
    }
}

fn handle_leave(state: &mut ClientState, _ws: &TungsteniteWs) {
    match state.phase {
        Phase::Lobby => {
            output::err("Not in a room.");
        }
        Phase::Joining | Phase::InRoom => {
            // Call leave_room() on the active session before dropping it
            if let Some(ref session) = state.session {
                if let Err(e) = session.leave_room() {
                    output::err(&format!("leave_room failed: {e}"));
                }
            }
            state.reset_to_lobby();
            output::ok("Left room.");
        }
    }
}

fn handle_name(new_name: &str, state: &mut ClientState) {
    state.display_name = new_name.to_string();
    if state.phase == Phase::InRoom {
        output::ok(&format!(
            "Display name set to '{}' (takes effect next join)",
            new_name
        ));
    } else {
        output::ok(&format!("Display name set to '{}'", new_name));
    }
}

fn handle_volume(level: u8, state: &mut ClientState) {
    if let Some(ref buf) = state.playback_buffer {
        buf.set_volume(level);
        output::ok(&format!("Master volume set to {}", level));
    } else {
        output::err("Not in a room. Volume will apply when you join.");
    }
}

fn handle_peer_volume(peer: &str, level: u8, state: &mut ClientState) {
    if state.phase != Phase::InRoom {
        output::err("Not in a room.");
        return;
    }
    // Match by participant_id or display_name (case-insensitive).
    let matched = state.participants.iter().find(|p| {
        p.participant_id.eq_ignore_ascii_case(peer) || p.display_name.eq_ignore_ascii_case(peer)
    });
    match matched {
        Some(p) => {
            state.peer_volumes.set(&p.participant_id, level);
            output::ok(&format!(
                "Volume for '{}' ({}) set to {}",
                p.display_name, p.participant_id, level
            ));
        }
        None => {
            output::err(&format!(
                "No participant matching '{}'. Use 'status' to see participants.",
                peer
            ));
        }
    }
}

fn handle_status(state: &ClientState) {
    match state.phase {
        Phase::Lobby => {
            let name_info = if state.display_name.is_empty() {
                String::new()
            } else {
                format!(" | Name: {}", state.display_name)
            };
            output::ok(&format!("Not in a room.{name_info}"));
        }
        Phase::Joining => {
            output::ok(&format!(
                "Joining room: {} (waiting for server response)",
                state.room_id.as_deref().unwrap_or("unknown")
            ));
        }
        Phase::InRoom => {
            if let (Some(room_id), Some(peer_id), Some(sfu_mode)) =
                (&state.room_id, &state.peer_id, &state.sfu_mode)
            {
                let vol = state
                    .playback_buffer
                    .as_ref()
                    .map(|b| b.volume())
                    .unwrap_or(DEFAULT_VOLUME);
                let name_info = if state.display_name.is_empty() {
                    String::new()
                } else {
                    format!(" | Name: {}", state.display_name)
                };
                output::ok(&format!(
                    "{} | Volume: {}{}",
                    output::format_status(room_id, peer_id, &state.participants, sfu_mode),
                    vol,
                    name_info,
                ));
            } else {
                output::ok("In room (state incomplete).");
            }
        }
    }
}

// --- Incoming message routing ---

fn handle_incoming_message(text: &str, state: &mut ClientState) {
    let msg = match signaling::parse(text) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("Failed to parse incoming message: {e:?}");
            return;
        }
    };

    match msg {
        SignalingMessage::Joined(payload) => {
            if state.phase == Phase::Joining {
                // Transition to InRoom
                state.phase = Phase::InRoom;
                state.peer_id = Some(payload.peer_id.clone());
                state.participants = payload.participants.clone();
                state.sfu_mode = Some(SfuConnectionMode::Proxy);
                output::ok(&output::format_joined(&payload));
            } else {
                // Late Joined after cancelled join — ignore
                log::debug!("Ignoring late Joined message (phase={:?})", state.phase);
            }
        }
        SignalingMessage::JoinRejected(payload) => {
            if state.phase == Phase::Joining {
                output::err(&output::format_join_rejected(&payload));
                state.reset_to_lobby();
            } else {
                log::debug!("Ignoring JoinRejected message (phase={:?})", state.phase);
            }
        }
        SignalingMessage::RoomCreated(payload) => {
            if state.phase == Phase::Joining && state.pending_create {
                state.pending_create = false;

                // Start mic capture and media transport for the room creator.
                // The creator joins implicitly via CreateRoom (no Join message
                // needed), but the RoomSession still needs its audio pipeline
                // initialised so MediaToken / LiveKit can work.
                if let Some(ref session) = state.session {
                    if let Err(e) = session.start_media() {
                        output::err(&format!("Audio start failed: {e}"));
                        state.reset_to_lobby();
                        return;
                    }
                }

                state.phase = Phase::InRoom;
                state.peer_id = Some(payload.peer_id.clone());
                state.participants = Vec::new();
                state.sfu_mode = Some(SfuConnectionMode::Proxy);
                output::ok(&output::format_room_created(&payload));
            } else {
                log::debug!("Ignoring RoomCreated message (phase={:?})", state.phase);
            }
        }
        // MediaToken: route to RoomSession, then check for LiveKit mode transition
        SignalingMessage::MediaToken(_) => {
            if let Some(ref session) = state.session {
                if let Err(e) = session.handle_incoming(text) {
                    log::warn!("RoomSession failed to handle MediaToken: {e}");
                }
                let new_mode = session.sfu_mode();
                let was_livekit = matches!(state.sfu_mode, Some(SfuConnectionMode::LiveKit { .. }));
                let is_livekit = matches!(new_mode, SfuConnectionMode::LiveKit { .. });
                state.sfu_mode = Some(new_mode);
                if is_livekit && !was_livekit {
                    output::event("LiveKit connected");
                }
            }
        }
        // Route other SFU lifecycle messages to RoomSession
        SignalingMessage::Answer(_)
        | SignalingMessage::IceCandidate(_)
        | SignalingMessage::RoomState(_) => {
            if let Some(ref session) = state.session {
                if let Err(e) = session.handle_incoming(text) {
                    log::warn!("RoomSession failed to handle message: {e}");
                }
            }
        }
        SignalingMessage::InviteCreated(payload) => {
            if state.pending_invite {
                state.pending_invite = false;
                output::ok(&output::format_invite_created(&payload));
            } else {
                log::debug!("Received InviteCreated without pending invite request");
            }
        }
        SignalingMessage::InviteRevoked(payload) => {
            if let Some(ref code) = state.pending_revoke {
                if code == &payload.invite_code {
                    state.pending_revoke = None;
                    output::ok(&output::format_invite_revoked(&payload));
                } else {
                    log::debug!(
                        "Received InviteRevoked for '{}' but pending revoke is for '{}'",
                        payload.invite_code,
                        code
                    );
                }
            } else {
                log::debug!("Received InviteRevoked without pending revoke request");
            }
        }
        SignalingMessage::ParticipantJoined(payload) => {
            state.participants.push(ParticipantInfo {
                participant_id: payload.participant_id.clone(),
                display_name: payload.display_name.clone(),
                user_id: None,
                profile_color: payload.profile_color.clone(),
            });
            output::event(&output::format_participant_joined(&payload));
            // Also route to RoomSession so it tracks participants internally
            if let Some(ref session) = state.session {
                if let Err(e) = session.handle_incoming(text) {
                    log::warn!("RoomSession failed to handle ParticipantJoined: {e}");
                }
            }
        }
        SignalingMessage::ParticipantLeft(payload) => {
            state
                .participants
                .retain(|p| p.participant_id != payload.participant_id);
            output::event(&output::format_participant_left(&payload));
            // Also route to RoomSession so it tracks participants internally
            if let Some(ref session) = state.session {
                if let Err(e) = session.handle_incoming(text) {
                    log::warn!("RoomSession failed to handle ParticipantLeft: {e}");
                }
            }
        }
        SignalingMessage::Error(payload) => {
            if state.pending_create {
                state.pending_create = false;
                output::err(&format!("Create room failed: {}", payload.message));
                state.reset_to_lobby();
            } else if state.pending_invite {
                state.pending_invite = false;
                output::err(&format!("Invite failed: {}", payload.message));
            } else if state.pending_revoke.is_some() {
                let code = state.pending_revoke.take().unwrap();
                output::err(&format!("Revoke '{}' failed: {}", code, payload.message));
            } else {
                output::err(&format!("Server error: {}", payload.message));
            }
        }
        other => {
            log::debug!("Received unhandled message: {:?}", other);
        }
    }
}
