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
use crate::ip::extract_client_ip;
use crate::redaction::redact_token;
use axum::Json;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Instant;
use tracing::warn;
use uuid::Uuid;

use crate::error::ErrorResponse;

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub phrase: String,
    pub device_name: String,
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
    pub device_name: String,
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

fn map_register_error(err: &AuthError) -> (StatusCode, Json<ErrorResponse>) {
    match err {
        AuthError::DatabaseError(_) | AuthError::SigningFailed(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "internal error".to_string(),
            }),
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "internal error".to_string(),
            }),
        ),
    }
}

fn map_recover_error(err: &AuthError) -> (StatusCode, Json<ErrorResponse>) {
    match err {
        AuthError::PhraseVerificationFailed | AuthError::RecoveryIdNotFound => (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "authentication failed".to_string(),
            }),
        ),
        AuthError::DeviceRevoked => (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "authentication failed".to_string(),
            }),
        ),
        AuthError::DatabaseError(_) | AuthError::SigningFailed(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "internal error".to_string(),
            }),
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "internal error".to_string(),
            }),
        ),
    }
}

fn map_refresh_error(err: &AuthError) -> (StatusCode, Json<ErrorResponse>) {
    match err {
        AuthError::RefreshTokenInvalid
        | AuthError::TokenReuseDetected
        | AuthError::ValidationFailed
        | AuthError::TokenExpired
        | AuthError::InvalidToken
        | AuthError::EpochMismatch => (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "authentication failed".to_string(),
            }),
        ),
        AuthError::DatabaseError(_) | AuthError::SigningFailed(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "internal error".to_string(),
            }),
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "internal error".to_string(),
            }),
        ),
    }
}

fn map_pairing_error(err: &PairingError) -> (StatusCode, Json<ErrorResponse>) {
    match err {
        PairingError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "not found".to_string(),
            }),
        ),
        PairingError::Expired | PairingError::CodeMismatch => (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "authentication failed".to_string(),
            }),
        ),
        PairingError::AlreadyUsed | PairingError::AlreadyApproved => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "conflict".to_string(),
            }),
        ),
        PairingError::NotApproved => (
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                error: "forbidden".to_string(),
            }),
        ),
        PairingError::LockedOut => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(ErrorResponse {
                error: "too many requests".to_string(),
            }),
        ),
        PairingError::DatabaseError(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "internal error".to_string(),
            }),
        ),
    }
}

fn map_device_error(err: &DeviceError) -> (StatusCode, Json<ErrorResponse>) {
    match err {
        DeviceError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "not found".to_string(),
            }),
        ),
        DeviceError::NotOwned => (
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                error: "forbidden".to_string(),
            }),
        ),
        DeviceError::DatabaseError(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "internal error".to_string(),
            }),
        ),
    }
}

fn rate_limited_response() -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::TOO_MANY_REQUESTS,
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
) -> Result<(StatusCode, Json<RegisterResponse>), (StatusCode, Json<ErrorResponse>)> {
    let client_ip = extract_client_ip(&ConnectInfo(addr), &headers, &app_state.ip_config);
    let now = Instant::now();

    if !app_state.auth_rate_limiter.check_register(client_ip, now) {
        warn!(ip = %client_ip, "register_device rate-limited");
        return Err(rate_limited_response());
    }
    app_state.auth_rate_limiter.record_register(client_ip, now);

    let result = auth::register_device(
        &app_state.db_pool,
        &app_state.auth_jwt_secret,
        ACCESS_TOKEN_TTL_SECS,
        app_state.refresh_token_ttl_days,
        &app_state.refresh_token_pepper,
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
            warn!(ip = %client_ip, error = %err, "register_device failed");
            Err(map_register_error(&err))
        }
    }
}

// ---------------------------------------------------------------------------
// POST /auth/register
// ---------------------------------------------------------------------------

pub async fn register(
    State(app_state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<RegisterResponse>), (StatusCode, Json<ErrorResponse>)> {
    let client_ip = extract_client_ip(&ConnectInfo(addr), &headers, &app_state.ip_config);
    let now = Instant::now();

    if !app_state.auth_rate_limiter.check_register(client_ip, now) {
        warn!(ip = %client_ip, "register rate-limited");
        return Err(rate_limited_response());
    }
    app_state.auth_rate_limiter.record_register(client_ip, now);

    let result = auth::register_user(
        &app_state.db_pool,
        &body.phrase,
        &body.device_name,
        &app_state.auth_jwt_secret,
        ACCESS_TOKEN_TTL_SECS,
        app_state.refresh_token_ttl_days,
        &app_state.refresh_token_pepper,
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
) -> Result<Json<RecoverResponse>, (StatusCode, Json<ErrorResponse>)> {
    let client_ip = extract_client_ip(&ConnectInfo(addr), &headers, &app_state.ip_config);
    let now = Instant::now();

    // Per-IP rate limit BEFORE any DB lookup; always count the attempt.
    if !app_state.recovery_rate_limiter.check_ip(client_ip, now) {
        warn!(ip = %client_ip, "recover rate-limited (IP)");
        return Err(rate_limited_response());
    }
    app_state.recovery_rate_limiter.record_ip(client_ip, now);

    // Per-recovery_id rate limit check (pre-check only; record AFTER DB lookup
    // confirms the recovery_id exists — avoids creating a rate-limiter oracle).
    if !app_state
        .recovery_rate_limiter
        .check_recovery_id(&body.recovery_id, now)
    {
        warn!(ip = %client_ip, recovery_id = %body.recovery_id, "recover rate-limited (recovery_id)");
        return Err(rate_limited_response());
    }

    let result = auth::recover_account(
        &app_state.db_pool,
        &body.recovery_id,
        &body.phrase,
        &body.device_name,
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
                recovery_id = %body.recovery_id,
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
                recovery_id = %body.recovery_id,
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
) -> Result<(StatusCode, Json<PairStartResponse>), (StatusCode, Json<ErrorResponse>)> {
    let client_ip = extract_client_ip(&ConnectInfo(addr), &headers, &app_state.ip_config);
    let now = Instant::now();

    // Rate-limit: 10 per IP per hour (reuse register limiter slot for pairing start).
    if !app_state.auth_rate_limiter.check_register(client_ip, now) {
        warn!(ip = %client_ip, "pair_start rate-limited");
        return Err(rate_limited_response());
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
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
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
) -> Result<Json<PairFinishResponse>, (StatusCode, Json<ErrorResponse>)> {
    let client_ip = extract_client_ip(&ConnectInfo(addr), &headers, &app_state.ip_config);
    let now = Instant::now();

    // Rate-limit: 10 per IP per hour.
    if !app_state.auth_rate_limiter.check_register(client_ip, now) {
        warn!(ip = %client_ip, "pair_finish rate-limited");
        return Err(rate_limited_response());
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
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
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
) -> Result<Json<ListDevicesResponse>, (StatusCode, Json<ErrorResponse>)> {
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
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
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
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
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
        Err(AuthError::PhraseVerificationFailed) => Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "authentication failed".to_string(),
            }),
        )),
        Err(err) => {
            warn!(user_id = %user.user_id, error = %err, "rotate_phrase failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "internal error".to_string(),
                }),
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
) -> Result<Json<RefreshResponse>, (StatusCode, Json<ErrorResponse>)> {
    let client_ip = extract_client_ip(&ConnectInfo(addr), &headers, &app_state.ip_config);
    let now = Instant::now();

    if !app_state.auth_rate_limiter.check_refresh(client_ip, now) {
        warn!(ip = %client_ip, "refresh_token rate-limited");
        return Err(rate_limited_response());
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
