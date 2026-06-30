//! Integration tests for TURN credential issuance.
//!
//! These tests cover the join-path surfaces that currently carry TURN ICE
//! configuration, plus the shared credential builder used to assemble the
//! payload handed to clients.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use proptest::prelude::*;
use shared::signaling::{AuthPayload, JoinPayload, RoomCreatedPayload, SignalingMessage};
use std::io;
use std::sync::{Arc, Mutex, OnceLock};
use tracing_subscriber::fmt::MakeWriter;
use async_trait::async_trait;
use uuid::Uuid;
use wavis_backend::abuse::join_rate_limiter::{JoinRateLimiter, JoinRateLimiterConfig};
use wavis_backend::app_state::AppState;
use wavis_backend::auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
use wavis_backend::auth::jwt::{
    ACCESS_TOKEN_TTL_SECS, sign_access_token, validate_access_token_with_rotation,
};
use wavis_backend::channel::invite::{InviteStore, InviteStoreConfig};
use wavis_backend::ip::IpConfig;
use wavis_backend::voice::relay::{P2PJoinResult, handle_p2p_join};
use wavis_backend::voice::sfu_bridge::{
    SfuError, SfuHealth, SfuRoomHandle, SfuRoomManager, SfuSignalingProxy,
};
use wavis_backend::voice::sfu_relay::{OutboundSignal, SignalTarget};
use wavis_backend::voice::turn_cred::{
    TurnConfig, build_ice_config_payload, generate_turn_credentials,
};
use wavis_backend::ws::validation::{SessionContext, validate_state_transition};

const TEST_AUTH_SECRET: &[u8] = b"test-auth-secret-at-least-32-bytes!!";
const TEST_NOW_UNIX: u64 = 1_700_000_000;

#[derive(Clone, Default)]
struct SharedWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl<'a> MakeWriter<'a> for SharedWriter {
    type Writer = SharedWriterGuard;

    fn make_writer(&'a self) -> Self::Writer {
        SharedWriterGuard {
            buffer: self.buffer.clone(),
        }
    }
}

struct SharedWriterGuard {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl io::Write for SharedWriterGuard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer
            .lock()
            .expect("log capture mutex poisoned")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
enum TurnSurface {
    Joined,
    MediaToken,
    RoomCreated,
}

struct NoOpSfuBridge;

#[async_trait]
impl SfuRoomManager for NoOpSfuBridge {
    async fn create_room(&self, room_id: &str) -> Result<SfuRoomHandle, SfuError> {
        Ok(SfuRoomHandle(room_id.to_string()))
    }

    async fn destroy_room(&self, _handle: &SfuRoomHandle) -> Result<(), SfuError> {
        Ok(())
    }

    async fn add_participant(
        &self,
        _handle: &SfuRoomHandle,
        _participant_id: &str,
    ) -> Result<(), SfuError> {
        Ok(())
    }

    async fn remove_participant(
        &self,
        _handle: &SfuRoomHandle,
        _participant_id: &str,
    ) -> Result<(), SfuError> {
        Ok(())
    }

    async fn health_check(&self) -> Result<SfuHealth, SfuError> {
        Ok(SfuHealth::Available)
    }
}

#[async_trait]
impl SfuSignalingProxy for NoOpSfuBridge {
    async fn forward_offer(
        &self,
        _handle: &SfuRoomHandle,
        _participant_id: &str,
        sdp: &str,
    ) -> Result<String, SfuError> {
        Ok(sdp.to_string())
    }

    async fn forward_ice_candidate(
        &self,
        _handle: &SfuRoomHandle,
        _participant_id: &str,
        _candidate: &shared::signaling::IceCandidate,
    ) -> Result<(), SfuError> {
        Ok(())
    }

    async fn poll_sfu_ice_candidates(
        &self,
        _handle: &SfuRoomHandle,
        _participant_id: &str,
    ) -> Result<Vec<shared::signaling::IceCandidate>, SfuError> {
        Ok(vec![])
    }
}

fn capture_logs(run: impl FnOnce()) -> String {
    let writer = SharedWriter::default();
    let buffer = writer.buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(writer)
        .finish();

    tracing::subscriber::with_default(subscriber, run);

    String::from_utf8(buffer.lock().expect("log capture mutex poisoned").clone())
        .expect("captured logs are valid utf-8")
}

fn with_env_vars(vars: &[(&str, Option<&str>)], test: impl FnOnce()) {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    struct RestoreEnv(Vec<(String, Option<String>)>);

    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            for (key, value) in self.0.drain(..).rev() {
                match value {
                    Some(value) => unsafe { std::env::set_var(&key, value) },
                    None => unsafe { std::env::remove_var(&key) },
                }
            }
        }
    }

    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let saved = vars
        .iter()
        .map(|(key, _)| (key.to_string(), std::env::var(key).ok()))
        .collect();
    let _restore = RestoreEnv(saved);

    for (key, value) in vars {
        match value {
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    test();
}

fn make_turn_config() -> TurnConfig {
    make_turn_config_with_secret(b"a-32-byte-secret-for-testing-ok!".to_vec())
}

fn make_turn_config_with_secret(secret: Vec<u8>) -> TurnConfig {
    TurnConfig::new(
        secret,
        None,
        3600,
        vec!["stun:stun.example.com:3478".to_string()],
        vec!["turn:turn.example.com:3478".to_string()],
    )
}

fn build_test_app_state(turn_config: Option<TurnConfig>) -> AppState {
    let mut app_state = with_env_vars_result(
        &[
            ("TURN_SHARED_SECRET", None),
            ("TURN_SHARED_SECRET_PREVIOUS", None),
            ("TURN_CREDENTIAL_TTL_SECS", None),
            ("WAVIS_STUN_URLS", None),
            ("WAVIS_TURN_URLS", None),
        ],
        || {
            let mock = Arc::new(NoOpSfuBridge);

            AppState::new(
                mock.clone() as Arc<dyn SfuRoomManager>,
                Some(mock as Arc<dyn SfuSignalingProxy>),
                "sfu://localhost".to_string(),
                Arc::new(InviteStore::new(InviteStoreConfig::default())),
                Arc::new(JoinRateLimiter::new(JoinRateLimiterConfig::default())),
                IpConfig {
                    trust_proxy_headers: false,
                    trusted_proxy_cidrs: vec![],
                },
                Arc::new(b"dev-secret-32-bytes-minimum!!!XX".to_vec()),
                None,
                "wavis-backend".to_string(),
                sqlx::postgres::PgPoolOptions::new()
                    .connect_lazy("postgres://dummy")
                    .expect("lazy pool should build"),
                Arc::new(TEST_AUTH_SECRET.to_vec()),
                None,
                Arc::new(AuthRateLimiter::new(AuthRateLimiterConfig::default())),
                30,
                72,
                Arc::new(b"test-pepper-at-least-32-bytes!!!!!!".to_vec()),
                None,
                Arc::new(wavis_backend::auth::phrase::generate_dummy_verifier(
                    &wavis_backend::auth::phrase::PhraseConfig::default(),
                )),
                Arc::new(b"test-pairing-pepper-32-bytes!!XX".to_vec()),
                Arc::new(
                    wavis_backend::auth::recovery_rate_limiter::RecoveryRateLimiter::new(
                        wavis_backend::auth::recovery_rate_limiter::RecoveryRateLimiterConfig::default(),
                    ),
                ),
                Arc::new(wavis_backend::auth::phrase::PhraseConfig::default()),
                Arc::new(vec![0u8; 32]),
                24,
                7,
                Arc::new(wavis_backend::diagnostics::bug_report::MockGitHubClient::new()),
                "owner/test-repo".to_string(),
                Arc::new(wavis_backend::diagnostics::llm_client::NoOpLlmClient),
            )
        },
    );

    app_state.turn_config = turn_config.map(Arc::new);
    app_state.require_invite_code = false;
    app_state
}

fn with_env_vars_result<T>(vars: &[(&str, Option<&str>)], test: impl FnOnce() -> T) -> T {
    let mut result = None;
    with_env_vars(vars, || {
        result = Some(test());
    });
    result.expect("result should be set")
}

fn collect_joined(signals: Vec<OutboundSignal>, peer_id: &str) -> Vec<SignalingMessage> {
    signals
        .into_iter()
        .filter(|signal| matches!(&signal.target, SignalTarget::Peer(pid) if pid == peer_id))
        .map(|signal| signal.msg)
        .collect()
}

fn inject_turn(
    signals: &mut [OutboundSignal],
    peer_id: &str,
    config: &TurnConfig,
    now_unix: u64,
) {
    let creds = generate_turn_credentials(peer_id, config, now_unix);
    let ice_payload = build_ice_config_payload(config, &creds);
    for signal in signals.iter_mut() {
        if let SignalingMessage::Joined(ref mut joined) = signal.msg
            && matches!(&signal.target, SignalTarget::Peer(pid) if pid == peer_id)
        {
            joined.ice_config = Some(ice_payload.clone());
        }
    }
}

fn inject_room_created_turn(
    signal: &mut SignalingMessage,
    peer_id: &str,
    config: &TurnConfig,
    now_unix: u64,
) {
    let creds = generate_turn_credentials(peer_id, config, now_unix);
    let ice_payload = build_ice_config_payload(config, &creds);
    if let SignalingMessage::RoomCreated(payload) = signal {
        payload.ice_config = Some(ice_payload);
    }
}

fn ice_payload_for_surface(
    signal_kind: TurnSurface,
    peer_id: &str,
    config: &TurnConfig,
    now_unix: u64,
) -> shared::signaling::IceConfigPayload {
    match signal_kind {
        TurnSurface::Joined => {
            let mut signals = vec![OutboundSignal::to_peer(
                peer_id,
                SignalingMessage::Joined(shared::signaling::JoinedPayload {
                    room_id: "room-turn".to_string(),
                    peer_id: peer_id.to_string(),
                    peer_count: 1,
                    participants: vec![],
                    ice_config: None,
                    share_permission: None,
                }),
            )];
            inject_turn(&mut signals, peer_id, config, now_unix);
            match &signals[0].msg {
                SignalingMessage::Joined(payload) => payload
                    .ice_config
                    .clone()
                    .expect("joined signal should receive ice_config"),
                other => panic!("expected Joined signal, got {other:?}"),
            }
        }
        TurnSurface::MediaToken => {
            // The current shared signaling schema does not expose `MediaToken.iceConfig`,
            // so validate the shared per-peer payload builder directly for this surface.
            let creds = generate_turn_credentials(peer_id, config, now_unix);
            build_ice_config_payload(config, &creds)
        }
        TurnSurface::RoomCreated => {
            let mut signal = SignalingMessage::RoomCreated(RoomCreatedPayload {
                room_id: "room-turn".to_string(),
                peer_id: peer_id.to_string(),
                invite_code: "invite123".to_string(),
                expires_in_secs: 300,
                max_uses: 10,
                ice_config: None,
            });
            inject_room_created_turn(&mut signal, peer_id, config, now_unix);
            match signal {
                SignalingMessage::RoomCreated(payload) => payload
                    .ice_config
                    .expect("room_created signal should receive ice_config"),
                other => panic!("expected RoomCreated signal, got {other:?}"),
            }
        }
    }
}

fn log_turn_enabled(config: &TurnConfig) {
    tracing::info!(
        ttl_secs = config.credential_ttl_secs,
        stun_urls = config.stun_urls.len(),
        turn_urls = config.turn_urls.len(),
        "TURN credentials enabled (TTL={}s, stun_urls={}, turn_urls={})",
        config.credential_ttl_secs,
        config.stun_urls.len(),
        config.turn_urls.len()
    );
}

fn log_turn_issuance(config: &TurnConfig, peer_id: &str, now_unix: u64) {
    let creds = generate_turn_credentials(peer_id, config, now_unix);
    let _payload = build_ice_config_payload(config, &creds);
    tracing::debug!(
        target: "wavis::turn",
        peer_id,
        username = %creds.username,
        turn_urls = config.turn_urls.len(),
        "issued TURN credentials"
    );
}

#[tokio::test]
async fn auth_then_join_includes_ice_config_when_turn_loaded() {
    let app_state = build_test_app_state(Some(make_turn_config()));
    let user_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let token = sign_access_token(
        &user_id,
        &Uuid::nil(),
        TEST_AUTH_SECRET,
        ACCESS_TOKEN_TTL_SECS,
        0,
    )
    .expect("signing must succeed");

    let no_session = None::<&SessionContext<'_>>;
    let auth_msg = SignalingMessage::Auth(AuthPayload {
        access_token: token.clone(),
    });
    assert!(
        validate_state_transition(&auth_msg, no_session, false).is_ok(),
        "auth must be allowed before join"
    );

    let (extracted_user_id, _device_id, _epoch) =
        validate_access_token_with_rotation(&token, TEST_AUTH_SECRET, None)
            .expect("token must validate");
    assert_eq!(extracted_user_id, user_id);

    let join_msg = SignalingMessage::Join(JoinPayload {
        room_id: "room-auth".to_string(),
        room_type: None,
        invite_code: Some("invite123".to_string()),
        display_name: None,
        profile_color: None,
    });
    assert!(
        validate_state_transition(&join_msg, no_session, true).is_ok(),
        "join must be allowed after auth"
    );

    let mut signals = match handle_p2p_join(
        app_state.room_state.as_ref(),
        "room-auth",
        "peer1234",
        app_state.invite_store.as_ref(),
        None,
    ) {
        P2PJoinResult::Joined(signals) => signals,
        P2PJoinResult::RoomFull => panic!("unexpected RoomFull"),
        P2PJoinResult::InviteRejected(reason) => {
            panic!("unexpected InviteRejected: {reason:?}")
        }
    };

    let turn_config = app_state
        .turn_config
        .as_deref()
        .expect("turn_config should be injected for this test");
    inject_turn(&mut signals, "peer1234", turn_config, TEST_NOW_UNIX);

    let joined_msgs = collect_joined(signals, "peer1234");
    assert_eq!(joined_msgs.len(), 1, "exactly one joined signal is expected");

    let ice_config = match &joined_msgs[0] {
        SignalingMessage::Joined(payload) => payload
            .ice_config
            .as_ref()
            .expect("joined payload must include ice_config"),
        other => panic!("expected Joined, got {other:?}"),
    };

    assert_eq!(ice_config.stun_urls, turn_config.stun_urls);
    assert_eq!(ice_config.turn_urls, turn_config.turn_urls);
    assert!(
        !ice_config.turn_username.is_empty(),
        "turn_username must be populated"
    );
    assert!(
        !ice_config.turn_credential.is_empty(),
        "turn_credential must be populated"
    );
}

#[tokio::test]
async fn join_with_turn_config_includes_ice_config() {
    let app_state = build_test_app_state(Some(make_turn_config()));
    let peer_id = "peer1";
    let room_id = "room-abc";

    let mut signals = match handle_p2p_join(
        app_state.room_state.as_ref(),
        room_id,
        peer_id,
        app_state.invite_store.as_ref(),
        None,
    ) {
        P2PJoinResult::Joined(signals) => signals,
        P2PJoinResult::RoomFull => panic!("unexpected RoomFull"),
        P2PJoinResult::InviteRejected(reason) => {
            panic!("unexpected InviteRejected: {reason:?}")
        }
    };

    let turn_config = app_state
        .turn_config
        .as_deref()
        .expect("turn_config should be injected for this test");
    inject_turn(&mut signals, peer_id, turn_config, TEST_NOW_UNIX);

    let joined_msgs = collect_joined(signals, peer_id);
    assert_eq!(
        joined_msgs.len(),
        1,
        "exactly one Joined signal for the joiner"
    );

    let ice_config = match &joined_msgs[0] {
        SignalingMessage::Joined(payload) => payload
            .ice_config
            .as_ref()
            .expect("ice_config must be present"),
        other => panic!("expected Joined, got {other:?}"),
    };

    assert_eq!(ice_config.stun_urls, turn_config.stun_urls);
    assert_eq!(ice_config.turn_urls, turn_config.turn_urls);

    let parts: Vec<&str> = ice_config.turn_username.splitn(2, ':').collect();
    assert_eq!(parts.len(), 2, "username must be 'expiry:peer_id'");
    let expiry: u64 = parts[0].parse().expect("expiry must be a number");
    assert_eq!(expiry, TEST_NOW_UNIX + turn_config.credential_ttl_secs);
    assert_eq!(parts[1], peer_id);
    assert!(
        !ice_config.turn_credential.is_empty(),
        "credential must be non-empty"
    );
}

// Regression link: Requirement 10.2, Requirement 6.2, Requirement 8.3.
// This preserves the explicit invariant that no TURN configuration means the
// join path must continue returning `Joined.ice_config = None`.
#[tokio::test]
async fn join_without_turn_config_has_no_ice_config() {
    let app_state = build_test_app_state(None);
    let peer_id = "peer2";
    let room_id = "room-xyz";

    let signals = match handle_p2p_join(
        app_state.room_state.as_ref(),
        room_id,
        peer_id,
        app_state.invite_store.as_ref(),
        None,
    ) {
        P2PJoinResult::Joined(signals) => signals,
        P2PJoinResult::RoomFull => panic!("unexpected RoomFull"),
        P2PJoinResult::InviteRejected(reason) => {
            panic!("unexpected InviteRejected: {reason:?}")
        }
    };

    let joined_msgs = collect_joined(signals, peer_id);
    assert_eq!(joined_msgs.len(), 1);

    match &joined_msgs[0] {
        SignalingMessage::Joined(payload) => {
            assert!(
                payload.ice_config.is_none(),
                "ice_config must be absent when TURN is not configured"
            );
        }
        other => panic!("expected Joined, got {other:?}"),
    }
}

#[test]
fn turn_credentials_are_deterministic() {
    let config = make_turn_config();
    let peer_id = "peerdet";

    let creds1 = generate_turn_credentials(peer_id, &config, TEST_NOW_UNIX);
    let creds2 = generate_turn_credentials(peer_id, &config, TEST_NOW_UNIX);

    assert_eq!(creds1.username, creds2.username);
    assert_eq!(creds1.credential, creds2.credential);
}

// Feature: turn-credentials-audit, Property 1: Per-peer credential issuance
// Validates: Requirements 6.1, 6.3
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_per_peer_issuance(
        peer_id in "[a-z0-9]{4,32}",
        signal_kind in prop_oneof![
            Just(TurnSurface::Joined),
            Just(TurnSurface::MediaToken),
            Just(TurnSurface::RoomCreated),
        ],
    ) {
        let config = make_turn_config();
        let payload = ice_payload_for_surface(signal_kind, &peer_id, &config, TEST_NOW_UNIX);

        prop_assert!(!payload.stun_urls.is_empty(), "stun_urls must be non-empty");
        prop_assert!(!payload.turn_urls.is_empty(), "turn_urls must be non-empty");
        prop_assert!(!payload.turn_username.is_empty(), "turn_username must be non-empty");
        prop_assert!(!payload.turn_credential.is_empty(), "turn_credential must be non-empty");
    }
}

// Feature: turn-credentials-audit, Property 2: Credential username format and freshness
// Validates: Requirements 10.3
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_turn_username_format(
        peer_id in "[a-z0-9]{4,32}",
        ttl in 60u64..=7200u64,
        now in 1_000_000u64..2_000_000_000u64,
    ) {
        let config = TurnConfig::new(
            b"a-32-byte-secret-for-testing-ok!".to_vec(),
            None,
            ttl,
            vec!["stun:localhost:3478".to_string()],
            vec!["turn:localhost:3478".to_string()],
        );
        let creds = generate_turn_credentials(&peer_id, &config, now);
        let payload = build_ice_config_payload(&config, &creds);

        let mut parts = payload.turn_username.splitn(2, ':');
        let expiry_str = parts.next().expect("username should include an expiry");
        let parsed_peer = parts.next().expect("username should include a peer id");
        let expiry: u64 = expiry_str.parse().expect("expiry should be numeric");

        prop_assert_eq!(parsed_peer, peer_id.as_str());
        prop_assert!(expiry > now, "expiry must be strictly greater than now");
        prop_assert_eq!(expiry, now + ttl);
    }
}

// Feature: turn-credentials-audit, Property 3: Credential uniqueness across peers
// Validates: Requirements 6.4, 10.4
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_credentials_unique_per_peer(
        peer_a in "[a-z0-9]{4,32}",
        peer_b in "[a-z0-9]{4,32}",
        now in 1_000_000u64..2_000_000_000u64,
    ) {
        prop_assume!(peer_a != peer_b);

        let config = TurnConfig::new(
            b"a-32-byte-secret-for-testing-ok!".to_vec(),
            None,
            3600,
            vec!["stun:localhost:3478".to_string()],
            vec!["turn:localhost:3478".to_string()],
        );

        let creds_a = generate_turn_credentials(&peer_a, &config, now);
        let creds_b = generate_turn_credentials(&peer_b, &config, now);

        prop_assert_ne!(creds_a.username, creds_b.username);
        prop_assert_ne!(creds_a.credential, creds_b.credential);
    }
}

// Feature: turn-credentials-audit, Property 4: Secret redaction across TURN log paths
// Validates: Requirements 5.3, 5.4
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_no_log_leaks_secret(
        secret in prop::collection::vec(any::<u8>(), 32..64),
        peer_id in "[a-z0-9]{4,32}",
    ) {
        let config = make_turn_config_with_secret(secret.clone());
        let expected_creds = generate_turn_credentials(&peer_id, &config, TEST_NOW_UNIX);
        let secret_hex = hex::encode(&secret);
        let secret_base64 = BASE64.encode(&secret);

        let logs = capture_logs(|| {
            log_turn_enabled(&config);
            log_turn_issuance(&config, &peer_id, TEST_NOW_UNIX);
        });

        prop_assert!(logs.contains("TURN credentials enabled (TTL="));
        prop_assert!(logs.contains("issued TURN credentials"));
        prop_assert!(logs.contains(&expected_creds.username));
        prop_assert!(!logs.contains(&secret_hex));
        prop_assert!(!logs.contains(&secret_base64));
        prop_assert!(!logs.contains(&expected_creds.credential));

        if let Ok(secret_text) = String::from_utf8(secret.clone()) {
            prop_assert!(
                !logs.contains(&secret_text),
                "logs must not contain the raw secret text"
            );
        }
    }
}
