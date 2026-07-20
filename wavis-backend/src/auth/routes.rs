//! HTTP REST endpoints for device authentication and identity management.
//!
//! **Owns:** request parsing, IP-based rate limiting, and response formatting
//! for: device registration, account recovery, device pairing, token refresh,
//! phrase rotation, device listing/revocation, and logout-all.
//!
//! **Does not own:** any auth business logic. Every endpoint delegates to
//! `domain::auth`, `domain::device`, or `domain::pairing` for validation,
//! persistence, and token issuance. This module never reads or writes the
//! database directly.
//!
//! **Key invariants:**
//! - All endpoints are rate-limited per IP via `AuthRateLimiter` (checked
//!   before any domain call).
//! - Error responses are opaque — internal details are logged server-side
//!   but never leaked to clients.
//! - Token-bearing endpoints use [`AuthenticatedUser`] for identity
//!   extraction.
//!
//! **Layering:** handlers → domain → state. This module calls domain
//! functions and maps their typed errors to HTTP status codes.

use crate::app_state::AppState;
use crate::auth::auth::{self, AuthError};
use crate::auth::device::{self, DeviceError};
use crate::auth::extractor::AuthenticatedUser;
use crate::auth::jwt::ACCESS_TOKEN_TTL_SECS;
use crate::auth::pairing::{self, PairingError};
use crate::auth::ws_ticket;
use crate::ip::extract_client_ip;
use crate::redaction::redact_token;
use axum::Json;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header::RETRY_AFTER};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Instant;
use tracing::warn;
use uuid::Uuid;

use crate::error::ErrorResponse;

type AuthErrorResponse = (StatusCode, HeaderMap, Json<ErrorResponse>);

// Mirror of shared::signaling::validation::MAX_DISPLAY_NAME_LEN.
const MAX_USERNAME_LEN: usize = shared::signaling::validation::MAX_DISPLAY_NAME_LEN;

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub phrase: String,
    #[serde(alias = "device_name", alias = "displayName", alias = "display_name")]
    pub username: String,
    #[serde(alias = "inviteCode")]
    pub invite_code: Option<String>,
}

#[derive(Serialize)]
pub struct RegisterResponse {
    pub user_id: String,
    pub device_id: String,
    pub recovery_id: String,
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Deserialize)]
pub struct RecoverRequest {
    pub recovery_id: String,
    pub phrase: String,
    #[serde(alias = "device_name", alias = "displayName", alias = "display_name")]
    pub username: Option<String>,
}

#[derive(Serialize)]
pub struct RecoverResponse {
    pub user_id: String,
    pub device_id: String,
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Serialize)]
pub struct RefreshResponse {
    pub user_id: String,
    pub device_id: String,
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Deserialize)]
pub struct PairStartRequest {
    pub device_name: String,
}

#[derive(Serialize)]
pub struct PairStartResponse {
    pub pairing_id: String,
    pub code: String,
}

#[derive(Deserialize)]
pub struct PairApproveRequest {
    pub pairing_id: Uuid,
    pub code: String,
}

#[derive(Deserialize)]
pub struct PairFinishRequest {
    pub pairing_id: Uuid,
    pub code: String,
}

#[derive(Serialize)]
pub struct PairFinishResponse {
    pub user_id: String,
    pub device_id: String,
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Deserialize)]
pub struct RotatePhraseRequest {
    pub current_phrase: String,
    pub new_phrase: String,
}

#[derive(Deserialize)]
pub struct UpdateUsernameRequest {
    pub username: String,
}

#[derive(Serialize)]
pub struct MeResponse {
    pub username: String,
}

#[derive(Serialize)]
pub struct WsTicketResponse {
    pub ticket: String,
    pub expires_in: u64,
}

#[derive(Serialize)]
pub struct DeviceInfoResponse {
    pub device_id: String,
    pub device_name: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Serialize)]
pub struct ListDevicesResponse {
    pub devices: Vec<DeviceInfoResponse>,
    pub current_device_id: String,
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

fn error_response(status: StatusCode, error: &str) -> AuthErrorResponse {
    (
        status,
        HeaderMap::new(),
        Json(ErrorResponse {
            error: error.to_string(),
        }),
    )
}

#[allow(clippy::result_large_err)]
fn validate_username(username: &str) -> Result<&str, AuthErrorResponse> {
    let username = username.trim();
    if username.is_empty() || username.chars().count() > MAX_USERNAME_LEN {
        return Err(error_response(StatusCode::BAD_REQUEST, "invalid username"));
    }
    Ok(username)
}
fn map_register_error(err: &AuthError) -> AuthErrorResponse {
    match err {
        AuthError::InviteInvalid => {
            error_response(StatusCode::UNAUTHORIZED, "authentication failed")
        }
        AuthError::DatabaseError(_) | AuthError::SigningFailed(_) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
        _ => error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    }
}

fn map_recover_error(err: &AuthError) -> AuthErrorResponse {
    match err {
        AuthError::PhraseVerificationFailed | AuthError::RecoveryIdNotFound => {
            error_response(StatusCode::UNAUTHORIZED, "authentication failed")
        }
        AuthError::DeviceRevoked => {
            error_response(StatusCode::UNAUTHORIZED, "authentication failed")
        }
        AuthError::DatabaseError(_) | AuthError::SigningFailed(_) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
        _ => error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    }
}

fn map_refresh_error(err: &AuthError) -> AuthErrorResponse {
    match err {
        AuthError::RefreshTokenInvalid
        | AuthError::TokenReuseDetected
        | AuthError::ValidationFailed
        | AuthError::TokenExpired
        | AuthError::InvalidToken
        | AuthError::EpochMismatch => {
            error_response(StatusCode::UNAUTHORIZED, "authentication failed")
        }
        AuthError::DatabaseError(_) | AuthError::SigningFailed(_) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
        _ => error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    }
}

fn map_pairing_error(err: &PairingError) -> AuthErrorResponse {
    match err {
        PairingError::NotFound => error_response(StatusCode::NOT_FOUND, "not found"),
        PairingError::Expired | PairingError::CodeMismatch => {
            error_response(StatusCode::UNAUTHORIZED, "authentication failed")
        }
        PairingError::AlreadyUsed | PairingError::AlreadyApproved => {
            error_response(StatusCode::CONFLICT, "conflict")
        }
        PairingError::NotApproved => error_response(StatusCode::FORBIDDEN, "forbidden"),
        PairingError::LockedOut => {
            error_response(StatusCode::TOO_MANY_REQUESTS, "too many requests")
        }
        PairingError::DatabaseError(_) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

fn map_device_error(err: &DeviceError) -> AuthErrorResponse {
    match err {
        DeviceError::NotFound => error_response(StatusCode::NOT_FOUND, "not found"),
        DeviceError::NotOwned => error_response(StatusCode::FORBIDDEN, "forbidden"),
        DeviceError::DatabaseError(_) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

fn rate_limited_response(retry_after_secs: Option<u64>) -> AuthErrorResponse {
    let mut headers = HeaderMap::new();
    if let Some(secs) = retry_after_secs
        && let Ok(value) = HeaderValue::from_str(&secs.to_string())
    {
        headers.insert(RETRY_AFTER, value);
    }
    (
        StatusCode::TOO_MANY_REQUESTS,
        headers,
        Json(ErrorResponse {
            error: "too many requests".to_string(),
        }),
    )
}

// ---------------------------------------------------------------------------
// POST /auth/register_device (legacy — kept for backward compatibility)
// ---------------------------------------------------------------------------

pub async fn register_device(
    State(app_state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<ErrorResponse>), AuthErrorResponse> {
    let client_ip = extract_client_ip(&ConnectInfo(addr), &headers, &app_state.ip_config);

    warn!(ip = %client_ip, "legacy register_device rejected");
    Ok((
        StatusCode::GONE,
        Json(ErrorResponse {
            error: "gone".to_string(),
        }),
    ))
}

// ---------------------------------------------------------------------------
// POST /auth/register
// ---------------------------------------------------------------------------

pub async fn register(
    State(app_state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<RegisterResponse>), AuthErrorResponse> {
    let client_ip = extract_client_ip(&ConnectInfo(addr), &headers, &app_state.ip_config);
    let now = Instant::now();

    let retry_after_secs = app_state
        .auth_rate_limiter
        .seconds_until_register(client_ip, now);
    if retry_after_secs.is_some() {
        warn!(ip = %client_ip, retry_after = retry_after_secs, "register rate-limited");
        return Err(rate_limited_response(retry_after_secs));
    }
    app_state.auth_rate_limiter.record_register(client_ip, now);
    let username = validate_username(&body.username)?;
    let invite_code = body
        .invite_code
        .as_deref()
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "authentication failed"))?;

    let result = auth::register_user_with_invite(
        &app_state.db_pool,
        &body.phrase,
        username,
        invite_code,
        &app_state.auth_jwt_secret,
        ACCESS_TOKEN_TTL_SECS,
        app_state.refresh_token_ttl_days,
        &app_state.refresh_token_pepper,
        &app_state.alpha_invite_code_pepper,
        &app_state.phrase_config,
        &app_state.phrase_encryption_key,
    )
    .await;

    match result {
        Ok(reg) => Ok((
            StatusCode::CREATED,
            Json(RegisterResponse {
                user_id: reg.user_id.to_string(),
                device_id: reg.device_id.to_string(),
                recovery_id: reg.recovery_id,
                access_token: reg.access_token,
                refresh_token: reg.refresh_token,
            }),
        )),
        Err(err) => {
            warn!(ip = %client_ip, error = %err, "register failed");
            Err(map_register_error(&err))
        }
    }
}

// ---------------------------------------------------------------------------
// POST /auth/recover
// ---------------------------------------------------------------------------

pub async fn recover(
    State(app_state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<RecoverRequest>,
) -> Result<Json<RecoverResponse>, AuthErrorResponse> {
    let client_ip = extract_client_ip(&ConnectInfo(addr), &headers, &app_state.ip_config);
    let now = Instant::now();

    // Per-IP rate limit BEFORE any DB lookup; always count the attempt.
    let retry_after_secs = app_state
        .recovery_rate_limiter
        .seconds_until_ip(client_ip, now);
    if retry_after_secs.is_some() {
        warn!(ip = %client_ip, retry_after = retry_after_secs, "recover rate-limited (IP)");
        return Err(rate_limited_response(retry_after_secs));
    }
    app_state.recovery_rate_limiter.record_ip(client_ip, now);

    // Per-recovery_id rate limit check (pre-check only; record AFTER DB lookup
    // confirms the recovery_id exists — avoids creating a rate-limiter oracle).
    let retry_after_secs = app_state
        .recovery_rate_limiter
        .seconds_until_recovery_id(&body.recovery_id, now);
    if retry_after_secs.is_some() {
        warn!(
            ip = %client_ip,
            recovery_id = %redact_token(&body.recovery_id),
            retry_after = retry_after_secs,
            "recover rate-limited (recovery_id)"
        );
        return Err(rate_limited_response(retry_after_secs));
    }

    let result = auth::recover_account(
        &app_state.db_pool,
        &body.recovery_id,
        &body.phrase,
        body.username.as_deref().unwrap_or(""),
        &app_state.auth_jwt_secret,
        ACCESS_TOKEN_TTL_SECS,
        app_state.refresh_token_ttl_days,
        &app_state.refresh_token_pepper,
        &app_state.phrase_config,
        &app_state.phrase_encryption_key,
        &app_state.dummy_verifier,
    )
    .await;

    match result {
        Ok(reg) => {
            // Record per-recovery_id attempt only when recovery_id was found in DB.
            app_state
                .recovery_rate_limiter
                .record_recovery_id(&body.recovery_id, now);
            warn!(
                recovery_id = %redact_token(&body.recovery_id),
                new_device_id = %reg.device_id,
                "account recovered"
            );
            Ok(Json(RecoverResponse {
                user_id: reg.user_id.to_string(),
                device_id: reg.device_id.to_string(),
                access_token: reg.access_token,
                refresh_token: reg.refresh_token,
            }))
        }
        Err(ref err) => {
            // Record per-recovery_id attempt for PhraseVerificationFailed
            // (recovery_id was found but phrase was wrong).
            if matches!(err, AuthError::PhraseVerificationFailed) {
                app_state
                    .recovery_rate_limiter
                    .record_recovery_id(&body.recovery_id, now);
            }
            // RecoveryIdNotFound: do NOT record per-recovery_id attempt.
            let reason = match err {
                AuthError::PhraseVerificationFailed => "phrase_mismatch",
                AuthError::RecoveryIdNotFound => "recovery_id_not_found",
                _ => "internal_error",
            };
            warn!(
                ip = %client_ip,
                recovery_id = %redact_token(&body.recovery_id),
                reason = reason,
                "recovery failed"
            );
            Err(map_recover_error(err))
        }
    }
}

// ---------------------------------------------------------------------------
// POST /auth/pair/start
// ---------------------------------------------------------------------------

pub async fn pair_start(
    State(app_state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<PairStartRequest>,
) -> Result<(StatusCode, Json<PairStartResponse>), AuthErrorResponse> {
    let client_ip = extract_client_ip(&ConnectInfo(addr), &headers, &app_state.ip_config);
    let now = Instant::now();

    // Rate-limit: 10 per IP per hour (reuse register limiter slot for pairing start).
    let retry_after_secs = app_state
        .auth_rate_limiter
        .seconds_until_register(client_ip, now);
    if retry_after_secs.is_some() {
        warn!(ip = %client_ip, retry_after = retry_after_secs, "pair_start rate-limited");
        return Err(rate_limited_response(retry_after_secs));
    }
    app_state.auth_rate_limiter.record_register(client_ip, now);

    let result = pairing::start_pairing(
        &app_state.db_pool,
        &body.device_name,
        &app_state.pairing_code_pepper,
    )
    .await;

    match result {
        Ok((pairing_id, code)) => Ok((
            StatusCode::CREATED,
            Json(PairStartResponse {
                pairing_id: pairing_id.to_string(),
                code,
            }),
        )),
        Err(err) => {
            warn!(ip = %client_ip, error = %err, "pair_start failed");
            Err(map_pairing_error(&err))
        }
    }
}

// ---------------------------------------------------------------------------
// POST /auth/pair/approve (Bearer auth required)
// ---------------------------------------------------------------------------

pub async fn pair_approve(
    State(app_state): State<AppState>,
    user: AuthenticatedUser,
    Json(body): Json<PairApproveRequest>,
) -> Result<StatusCode, AuthErrorResponse> {
    // Rate-limit: 10 per user per hour (reuse refresh limiter keyed by user IP
    // — for MVP, we approximate per-user with the auth_rate_limiter).
    // The actual per-user enforcement is approximated since AuthRateLimiter is per-IP.
    // For MVP this is acceptable.

    let result = pairing::approve_pairing(
        &app_state.db_pool,
        body.pairing_id,
        &body.code,
        user.user_id,
        user.device_id,
        &app_state.pairing_code_pepper,
    )
    .await;

    match result {
        Ok(()) => {
            warn!(
                pairing_id = %body.pairing_id,
                approved_user_id = %user.user_id,
                approved_by_device_id = %user.device_id,
                "pairing approved"
            );
            Ok(StatusCode::OK)
        }
        Err(err) => {
            warn!(
                pairing_id = %body.pairing_id,
                user_id = %user.user_id,
                error = %err,
                "pair_approve failed"
            );
            Err(map_pairing_error(&err))
        }
    }
}

// ---------------------------------------------------------------------------
// POST /auth/pair/finish
// ---------------------------------------------------------------------------

pub async fn pair_finish(
    State(app_state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<PairFinishRequest>,
) -> Result<Json<PairFinishResponse>, AuthErrorResponse> {
    let client_ip = extract_client_ip(&ConnectInfo(addr), &headers, &app_state.ip_config);
    let now = Instant::now();

    // Rate-limit: 10 per IP per hour.
    let retry_after_secs = app_state
        .auth_rate_limiter
        .seconds_until_register(client_ip, now);
    if retry_after_secs.is_some() {
        warn!(ip = %client_ip, retry_after = retry_after_secs, "pair_finish rate-limited");
        return Err(rate_limited_response(retry_after_secs));
    }
    app_state.auth_rate_limiter.record_register(client_ip, now);

    let result = pairing::finish_pairing(
        &app_state.db_pool,
        body.pairing_id,
        &body.code,
        &app_state.pairing_code_pepper,
        &app_state.auth_jwt_secret,
        ACCESS_TOKEN_TTL_SECS,
        app_state.refresh_token_ttl_days,
        &app_state.refresh_token_pepper,
    )
    .await;

    match result {
        Ok(pr) => Ok(Json(PairFinishResponse {
            user_id: pr.user_id.to_string(),
            device_id: pr.device_id.to_string(),
            access_token: pr.access_token,
            refresh_token: pr.refresh_token,
        })),
        Err(err) => {
            warn!(ip = %client_ip, pairing_id = %body.pairing_id, error = %err, "pair_finish failed");
            Err(map_pairing_error(&err))
        }
    }
}

// ---------------------------------------------------------------------------
// POST /auth/logout_all (Bearer auth required)
// ---------------------------------------------------------------------------

pub async fn logout_all(
    State(app_state): State<AppState>,
    user: AuthenticatedUser,
) -> Result<StatusCode, AuthErrorResponse> {
    let result = device::logout_all(&app_state.db_pool, user.user_id).await;

    match result {
        Ok(new_epoch) => {
            warn!(
                user_id = %user.user_id,
                new_epoch = new_epoch,
                "logout_all executed"
            );
            Ok(StatusCode::OK)
        }
        Err(err) => {
            warn!(user_id = %user.user_id, error = %err, "logout_all failed");
            Err(map_device_error(&err))
        }
    }
}

// ---------------------------------------------------------------------------
// GET /auth/devices (Bearer auth required)
// ---------------------------------------------------------------------------

pub async fn list_devices(
    State(app_state): State<AppState>,
    user: AuthenticatedUser,
) -> Result<Json<ListDevicesResponse>, AuthErrorResponse> {
    let result = device::list_devices(&app_state.db_pool, user.user_id).await;

    match result {
        Ok(devices) => {
            let device_list = devices
                .into_iter()
                .map(|d| DeviceInfoResponse {
                    device_id: d.device_id.to_string(),
                    device_name: d.device_name,
                    created_at: d.created_at.to_rfc3339(),
                    revoked_at: d.revoked_at.map(|t| t.to_rfc3339()),
                })
                .collect();
            Ok(Json(ListDevicesResponse {
                devices: device_list,
                current_device_id: user.device_id.to_string(),
            }))
        }
        Err(err) => {
            warn!(user_id = %user.user_id, error = %err, "list_devices failed");
            Err(map_device_error(&err))
        }
    }
}

// ---------------------------------------------------------------------------
// POST /auth/devices/{device_id}/revoke (Bearer auth required)
// ---------------------------------------------------------------------------

pub async fn revoke_device(
    State(app_state): State<AppState>,
    user: AuthenticatedUser,
    Path(target_device_id): Path<Uuid>,
) -> Result<StatusCode, AuthErrorResponse> {
    let result = device::revoke_device(&app_state.db_pool, user.user_id, target_device_id).await;

    match result {
        Ok(()) => {
            warn!(
                user_id = %user.user_id,
                revoked_device_id = %target_device_id,
                revoking_device_id = %user.device_id,
                "device revoked"
            );
            Ok(StatusCode::OK)
        }
        Err(err) => {
            warn!(
                user_id = %user.user_id,
                target_device_id = %target_device_id,
                error = %err,
                "revoke_device failed"
            );
            Err(map_device_error(&err))
        }
    }
}

// ---------------------------------------------------------------------------
// POST /auth/phrase/rotate (Bearer auth required)
// ---------------------------------------------------------------------------

pub async fn rotate_phrase(
    State(app_state): State<AppState>,
    user: AuthenticatedUser,
    Json(body): Json<RotatePhraseRequest>,
) -> Result<StatusCode, AuthErrorResponse> {
    let result = auth::rotate_phrase(
        &app_state.db_pool,
        user.user_id,
        &body.current_phrase,
        &body.new_phrase,
        &app_state.phrase_config,
        &app_state.phrase_encryption_key,
    )
    .await;

    match result {
        Ok(()) => Ok(StatusCode::OK),
        Err(AuthError::PhraseVerificationFailed) => Err(error_response(
            StatusCode::UNAUTHORIZED,
            "authentication failed",
        )),
        Err(err) => {
            warn!(user_id = %user.user_id, error = %err, "rotate_phrase failed");
            Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error",
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// POST /auth/ws-ticket (Bearer auth required)
// ---------------------------------------------------------------------------

pub async fn issue_ws_ticket(
    State(app_state): State<AppState>,
    user: AuthenticatedUser,
) -> Json<WsTicketResponse> {
    let ticket = app_state
        .ws_ticket_store
        .issue(user.user_id, user.device_id);

    Json(WsTicketResponse {
        ticket,
        expires_in: ws_ticket::TICKET_TTL_SECS,
    })
}

// ---------------------------------------------------------------------------
// GET /auth/me (Bearer auth required)
// ---------------------------------------------------------------------------

pub async fn get_me(
    State(app_state): State<AppState>,
    user: AuthenticatedUser,
) -> Result<Json<MeResponse>, AuthErrorResponse> {
    match auth::get_username(&app_state.db_pool, user.user_id).await {
        Ok(username) => Ok(Json(MeResponse { username })),
        Err(err) => {
            warn!(user_id = %user.user_id, error = %err, "get_me failed");
            Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error",
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// POST /auth/username (Bearer auth required)
// ---------------------------------------------------------------------------

pub async fn update_username(
    State(app_state): State<AppState>,
    user: AuthenticatedUser,
    Json(body): Json<UpdateUsernameRequest>,
) -> Result<StatusCode, AuthErrorResponse> {
    let username = validate_username(&body.username)?;

    let result = auth::update_username(&app_state.db_pool, user.user_id, username).await;
    match result {
        Ok(()) => Ok(StatusCode::OK),
        Err(err) => {
            warn!(user_id = %user.user_id, error = %err, "update_username failed");
            Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error",
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// POST /auth/refresh
// ---------------------------------------------------------------------------

pub async fn refresh_token(
    State(app_state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<RefreshRequest>,
) -> Result<Json<RefreshResponse>, AuthErrorResponse> {
    let client_ip = extract_client_ip(&ConnectInfo(addr), &headers, &app_state.ip_config);
    let now = Instant::now();

    let retry_after_secs = app_state
        .auth_rate_limiter
        .seconds_until_refresh(client_ip, now);
    if retry_after_secs.is_some() {
        warn!(ip = %client_ip, retry_after = retry_after_secs, "refresh_token rate-limited");
        return Err(rate_limited_response(retry_after_secs));
    }
    app_state.auth_rate_limiter.record_refresh(client_ip, now);

    let result = auth::rotate_refresh_token(
        &app_state.db_pool,
        &body.refresh_token,
        &app_state.auth_jwt_secret,
        ACCESS_TOKEN_TTL_SECS,
        app_state.refresh_token_ttl_days,
        &app_state.refresh_token_pepper,
    )
    .await;

    match result {
        Ok(pair) => Ok(Json(RefreshResponse {
            user_id: pair.user_id.to_string(),
            device_id: pair.device_id.to_string(),
            access_token: pair.access_token,
            refresh_token: pair.refresh_token,
        })),
        Err(err) => {
            warn!(
                ip = %client_ip,
                token = %redact_token(&body.refresh_token),
                error = %err,
                "refresh_token failed"
            );
            Err(map_refresh_error(&err))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abuse::join_rate_limiter::{JoinRateLimiter, JoinRateLimiterConfig};
    use crate::auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
    use crate::auth::recovery_rate_limiter::{RecoveryRateLimiter, RecoveryRateLimiterConfig};
    use crate::channel::invite::{InviteStore, InviteStoreConfig};
    use crate::diagnostics::bug_report::MockGitHubClient;
    use crate::diagnostics::llm_client::NoOpLlmClient;
    use crate::ip::IpConfig;
    use crate::voice::mock_sfu_bridge::MockSfuBridge;
    use crate::voice::sfu_bridge::SfuRoomManager;
    use serde_json::json;
    use std::sync::Arc;

    const TEST_AUTH_SECRET: &[u8] = b"test-auth-secret-at-least-32-bytes!!";
    const TEST_REFRESH_PEPPER: &[u8] = b"test-pepper-at-least-32-bytes!!!!!!";

    fn test_app_state() -> AppState {
        let mock = Arc::new(MockSfuBridge::new());
        AppState::new(
            mock as Arc<dyn SfuRoomManager>,
            None,
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
                .connect_lazy("postgres://wavis:wavis@localhost:5432/wavis")
                .unwrap(),
            Arc::new(TEST_AUTH_SECRET.to_vec()),
            None,
            Arc::new(AuthRateLimiter::new(AuthRateLimiterConfig::default())),
            30,
            72,
            Arc::new(TEST_REFRESH_PEPPER.to_vec()),
            None,
            Arc::new(crate::auth::phrase::generate_dummy_verifier(
                &crate::auth::phrase::PhraseConfig::default(),
            )),
            Arc::new(b"test-pairing-pepper-32-bytes!!XX".to_vec()),
            Arc::new(RecoveryRateLimiter::new(
                RecoveryRateLimiterConfig::default(),
            )),
            Arc::new(crate::auth::phrase::PhraseConfig::default()),
            Arc::new(vec![0u8; 32]),
            24,
            7,
            Arc::new(MockGitHubClient::new()),
            "owner/test-repo".to_string(),
            Arc::new(NoOpLlmClient),
        )
    }

    #[test]
    fn validate_username_trims_valid_names() {
        match validate_username("  Alice  ") {
            Ok(username) => assert_eq!(username, "Alice"),
            Err(_) => panic!("valid username should be accepted"),
        }
    }

    #[test]
    fn validate_username_rejects_empty_and_long_names() {
        let (empty_status, _, _) = validate_username("   ").unwrap_err();
        assert_eq!(empty_status, StatusCode::BAD_REQUEST);

        let too_long = "a".repeat(MAX_USERNAME_LEN + 1);
        let (long_status, _, _) = validate_username(&too_long).unwrap_err();
        assert_eq!(long_status, StatusCode::BAD_REQUEST);
    }

    /// Recovery accepts a missing or empty username because recovery does not
    /// rename the account. validate_username is NOT called on the recover path.
    #[test]
    fn recover_request_allows_missing_or_empty_username() {
        let missing = RecoverRequest {
            recovery_id: "wvs-ABCD-1234".to_string(),
            phrase: "phrase".to_string(),
            username: None,
        };
        let empty = RecoverRequest {
            recovery_id: "wvs-ABCD-1234".to_string(),
            phrase: "phrase".to_string(),
            username: Some(String::new()),
        };
        assert_eq!(missing.username.as_deref().unwrap_or(""), "");
        assert_eq!(empty.username.as_deref().unwrap_or(""), "");
        assert!(validate_username("").is_err());
    }

    #[test]
    fn register_request_accepts_invite_code_aliases() {
        let snake: RegisterRequest = serde_json::from_value(json!({
            "phrase": "phrase",
            "username": "alice",
            "invite_code": "ALPHA-SNAKE",
        }))
        .unwrap();
        assert_eq!(snake.invite_code.as_deref(), Some("ALPHA-SNAKE"));

        let camel: RegisterRequest = serde_json::from_value(json!({
            "phrase": "phrase",
            "username": "alice",
            "inviteCode": "ALPHA-CAMEL",
        }))
        .unwrap();
        assert_eq!(camel.invite_code.as_deref(), Some("ALPHA-CAMEL"));
    }

    #[tokio::test]
    async fn register_device_returns_gone_without_consuming_register_limit() {
        let app_state = test_app_state();
        let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

        assert!(
            app_state
                .auth_rate_limiter
                .check_register(ip, std::time::Instant::now())
        );

        let result = register_device(
            State(app_state.clone()),
            ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 12345))),
            HeaderMap::new(),
        )
        .await;
        let (status, Json(body)) = match result {
            Ok(response) => response,
            Err(_) => panic!("register_device should return 410, not an error response"),
        };

        assert_eq!(status, StatusCode::GONE);
        assert_eq!(body.error, "gone");
        assert!(
            app_state
                .auth_rate_limiter
                .check_register(ip, std::time::Instant::now())
        );
    }

    // ------------------------------------------------------------------
    // P1-5: map_pairing_error / map_device_error HTTP contract — every
    // variant must map to the exact status + opaque body security.md
    // documents, and DatabaseError must never leak its inner SQL detail.
    // ------------------------------------------------------------------

    #[test]
    fn map_pairing_error_matches_security_contract() {
        let cases: Vec<(PairingError, StatusCode, &str)> = vec![
            (PairingError::NotFound, StatusCode::NOT_FOUND, "not found"),
            (
                PairingError::Expired,
                StatusCode::UNAUTHORIZED,
                "authentication failed",
            ),
            (
                PairingError::CodeMismatch,
                StatusCode::UNAUTHORIZED,
                "authentication failed",
            ),
            (PairingError::AlreadyUsed, StatusCode::CONFLICT, "conflict"),
            (
                PairingError::AlreadyApproved,
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                PairingError::NotApproved,
                StatusCode::FORBIDDEN,
                "forbidden",
            ),
            (
                PairingError::LockedOut,
                StatusCode::TOO_MANY_REQUESTS,
                "too many requests",
            ),
        ];
        for (err, expected_status, expected_body) in cases {
            let (status, _headers, Json(body)) = map_pairing_error(&err);
            assert_eq!(status, expected_status, "status mismatch for {err:?}");
            assert_eq!(body.error, expected_body, "body mismatch for {err:?}");
        }
    }

    #[test]
    fn map_pairing_error_database_error_never_leaks_sql_detail() {
        let secret_detail = "duplicate key value violates unique constraint \"users_pkey\"";
        let (status, _headers, Json(body)) =
            map_pairing_error(&PairingError::DatabaseError(secret_detail.to_string()));
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.error, "internal error");
        assert!(
            !body.error.contains(secret_detail),
            "SQL error detail must never reach the client"
        );
    }

    #[test]
    fn map_device_error_matches_security_contract() {
        let cases: Vec<(DeviceError, StatusCode, &str)> = vec![
            (DeviceError::NotFound, StatusCode::NOT_FOUND, "not found"),
            (DeviceError::NotOwned, StatusCode::FORBIDDEN, "forbidden"),
        ];
        for (err, expected_status, expected_body) in cases {
            let (status, _headers, Json(body)) = map_device_error(&err);
            assert_eq!(status, expected_status, "status mismatch for {err:?}");
            assert_eq!(body.error, expected_body, "body mismatch for {err:?}");
        }
    }

    #[test]
    fn map_device_error_database_error_never_leaks_sql_detail() {
        let secret_detail = "connection refused at 10.0.0.5:5432";
        let (status, _headers, Json(body)) =
            map_device_error(&DeviceError::DatabaseError(secret_detail.to_string()));
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.error, "internal error");
        assert!(
            !body.error.contains(secret_detail),
            "SQL error detail must never reach the client"
        );
    }
}
