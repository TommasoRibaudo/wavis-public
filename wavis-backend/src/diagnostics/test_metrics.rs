#[cfg(feature = "test-metrics")]
use crate::abuse::abuse_metrics::AbuseMetricsSnapshot;
#[cfg(feature = "test-metrics")]
use crate::app_state::AppState;
#[cfg(feature = "test-metrics")]
use axum::extract::State;
#[cfg(feature = "test-metrics")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "test-metrics")]
use std::collections::HashMap;

#[cfg(feature = "test-metrics")]
#[derive(Serialize, Deserialize)]
pub struct RoomSnapshot {
    pub peer_ids: Vec<String>,
    pub participant_count: usize,
    pub room_type: String,
    pub active_shares: Vec<String>,
}

#[cfg(feature = "test-metrics")]
#[derive(Serialize, Deserialize)]
pub struct TestMetricsResponse {
    pub rooms: HashMap<String, RoomSnapshot>,
    pub abuse_metrics: AbuseMetricsSnapshot,
    pub total_rooms: usize,
    pub total_participants: usize,
    pub sfu_available: bool,
}

#[cfg(feature = "test-metrics")]
pub async fn test_metrics_handler(
    State(app_state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<TestMetricsResponse>, axum::http::StatusCode> {
    let expected_token = &app_state.metrics_token;
    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !auth_header.starts_with("Bearer ") || &auth_header[7..] != expected_token {
        return Err(axum::http::StatusCode::UNAUTHORIZED);
    }

    let rooms = app_state.room_state.snapshot_rooms();
    let room_snapshots: HashMap<String, RoomSnapshot> = rooms
        .into_iter()
        .map(|(room_id, peers)| {
            let info = app_state.room_state.get_room_info(&room_id);
            let participant_count = peers.len();
            let snapshot = RoomSnapshot {
                peer_ids: peers,
                participant_count,
                room_type: info
                    .as_ref()
                    .map(|i| format!("{:?}", i.room_type))
                    .unwrap_or_default(),
                active_shares: info
                    .map(|i| i.active_shares.into_iter().collect::<Vec<_>>())
                    .unwrap_or_default(),
            };
            (room_id, snapshot)
        })
        .collect();

    let sfu_available = {
        let health = app_state.sfu_health_status.read().await;
        matches!(*health, crate::voice::sfu_bridge::SfuHealth::Available)
    };

    Ok(axum::Json(TestMetricsResponse {
        total_rooms: room_snapshots.len(),
        total_participants: room_snapshots.values().map(|r| r.participant_count).sum(),
        rooms: room_snapshots,
        abuse_metrics: app_state.abuse_metrics.snapshot(),
        sfu_available,
    }))
}

#[cfg(feature = "test-metrics")]
pub async fn reset_rate_limits_handler(
    State(app_state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<axum::http::StatusCode, axum::http::StatusCode> {
    let expected_token = &app_state.metrics_token;
    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !auth_header.starts_with("Bearer ") || &auth_header[7..] != expected_token {
        return Err(axum::http::StatusCode::UNAUTHORIZED);
    }

    app_state.auth_rate_limiter.clear();
    app_state.channel_rate_limiter.clear();
    app_state.recovery_rate_limiter.clear();

    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[cfg(all(test, feature = "test-metrics"))]
mod tests {
    use super::*;
    use crate::abuse::join_rate_limiter::{JoinRateLimiter, JoinRateLimiterConfig};
    use crate::auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
    use crate::channel::invite::{InviteStore, InviteStoreConfig};
    use crate::diagnostics::bug_report::MockGitHubClient;
    use crate::diagnostics::llm_client::NoOpLlmClient;
    use crate::ip::IpConfig;
    use crate::voice::mock_sfu_bridge::MockSfuBridge;
    use crate::voice::sfu_bridge::{SfuRoomManager, SfuSignalingProxy};
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use std::sync::{Arc, Mutex};
    use tower::util::ServiceExt;

    // Serialize all tests that touch env vars to prevent races.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Build a test app. Must be called while holding `ENV_LOCK`.
    fn build_app_with_token(token: &str) -> (Router, AppState) {
        unsafe {
            std::env::set_var("SFU_JWT_SECRET", "dev-secret-32-bytes-minimum!!!XX");
            std::env::set_var("REQUIRE_INVITE_CODE", "false");
            std::env::set_var("TEST_METRICS_TOKEN", token);
        }

        let mock = Arc::new(MockSfuBridge::new());
        let invite_store = Arc::new(InviteStore::new(InviteStoreConfig::default()));
        let join_rate_limiter = Arc::new(JoinRateLimiter::new(JoinRateLimiterConfig::default()));
        let ip_config = IpConfig {
            trust_proxy_headers: false,
            trusted_proxy_cidrs: vec![],
        };

        let app_state = AppState::new(
            mock.clone() as Arc<dyn SfuRoomManager>,
            Some(mock as Arc<dyn SfuSignalingProxy>),
            "sfu://localhost".to_string(),
            invite_store,
            join_rate_limiter,
            ip_config,
            Arc::new(b"dev-secret-32-bytes-minimum!!!XX".to_vec()),
            None,
            "wavis-backend".to_string(),
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://dummy")
                .unwrap(),
            Arc::new(b"test-auth-secret-at-least-32-bytes!!".to_vec()),
            None,
            Arc::new(AuthRateLimiter::new(AuthRateLimiterConfig::default())),
            30,
            72,
            Arc::new(b"test-pepper-at-least-32-bytes!!!!!!".to_vec()),
            None,
            Arc::new(crate::auth::phrase::generate_dummy_verifier(
                &crate::auth::phrase::PhraseConfig::default(),
            )),
            Arc::new(b"test-pairing-pepper-32-bytes!!XX".to_vec()),
            Arc::new(
                crate::auth::recovery_rate_limiter::RecoveryRateLimiter::new(
                    crate::auth::recovery_rate_limiter::RecoveryRateLimiterConfig::default(),
                ),
            ),
            Arc::new(crate::auth::phrase::PhraseConfig::default()),
            Arc::new(vec![0u8; 32]),
            24,
            7,
            Arc::new(MockGitHubClient::new()),
            "owner/test-repo".to_string(),
            Arc::new(NoOpLlmClient),
        );

        let app = Router::new()
            .route("/test/metrics", get(test_metrics_handler))
            .with_state(app_state.clone());

        (app, app_state)
    }

    // -----------------------------------------------------------------------
    // 1. Valid bearer token â†’ 200
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn valid_bearer_token_returns_200() {
        let token = "valid-token-test1";
        let (app, _state) = {
            let _guard = ENV_LOCK.lock().unwrap();
            build_app_with_token(token)
        };

        let req = Request::builder()
            .uri("/test/metrics")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // -----------------------------------------------------------------------
    // 2. Missing Authorization header â†’ 401
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn missing_authorization_header_returns_401() {
        let (app, _state) = {
            let _guard = ENV_LOCK.lock().unwrap();
            build_app_with_token("missing-header-test2")
        };

        let req = Request::builder()
            .uri("/test/metrics")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // -----------------------------------------------------------------------
    // 3. Wrong bearer token â†’ 401
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn wrong_bearer_token_returns_401() {
        let (app, _state) = {
            let _guard = ENV_LOCK.lock().unwrap();
            build_app_with_token("correct-token-test3")
        };

        let req = Request::builder()
            .uri("/test/metrics")
            .header("authorization", "Bearer wrong-token-test3")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // -----------------------------------------------------------------------
    // 4. Response contains room state and abuse metrics fields
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn response_contains_room_state_and_abuse_metrics() {
        let token = "fields-token-test4";
        let (app, _state) = {
            let _guard = ENV_LOCK.lock().unwrap();
            let (app, state) = build_app_with_token(token);

            // Seed some room state so the snapshot is non-trivial
            state
                .room_state
                .add_peer("peer-1".to_string(), "room-a".to_string());
            state
                .room_state
                .add_peer("peer-2".to_string(), "room-a".to_string());

            // Bump an abuse counter
            state
                .abuse_metrics
                .increment(&state.abuse_metrics.join_invite_rejections);

            (app, state)
        };

        let req = Request::builder()
            .uri("/test/metrics")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        // Top-level fields must be present
        assert!(json.get("rooms").is_some(), "missing 'rooms' field");
        assert!(
            json.get("abuse_metrics").is_some(),
            "missing 'abuse_metrics' field"
        );
        assert!(
            json.get("total_rooms").is_some(),
            "missing 'total_rooms' field"
        );
        assert!(
            json.get("total_participants").is_some(),
            "missing 'total_participants' field"
        );

        // Room state should reflect what we seeded
        assert_eq!(json["total_rooms"], 1);
        assert_eq!(json["total_participants"], 2);
        let room = &json["rooms"]["room-a"];
        assert_eq!(room["participant_count"], 2);

        // Abuse metrics snapshot must include the counter we incremented
        let abuse = &json["abuse_metrics"];
        assert!(abuse.get("join_invite_rejections").is_some());
        assert_eq!(abuse["join_invite_rejections"], 1);
    }

    // -----------------------------------------------------------------------
    // 5. No token provided â†’ 401 (loopback enforcement is at the TCP listener
    //    level in production; the handler itself rejects unauthenticated requests)
    // -----------------------------------------------------------------------
    //
    // The admin listener is bound to 127.0.0.1 in main.rs, which means
    // non-loopback clients cannot reach it at the TCP level. This is stronger
    // than any handler-level IP check and cannot be spoofed behind a reverse
    // proxy. The handler's own responsibility is bearer-token auth, which is
    // tested here: a request with no Authorization header is rejected with 401
    // regardless of where it originates.
    #[tokio::test]
    async fn no_token_returns_401_loopback_enforcement_is_at_listener_level() {
        let (app, _state) = {
            let _guard = ENV_LOCK.lock().unwrap();
            build_app_with_token("loopback-token-test5")
        };

        let req = Request::builder()
            .uri("/test/metrics")
            // No Authorization header â€” simulates a request that somehow reached
            // the handler without a token (e.g. direct in-process call in tests).
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "handler must reject requests with no token; \
             non-loopback access is blocked at the TCP listener level in production"
        );
    }
}
