//! Admin-only HTTP endpoints for bug report management.
//!
//! All endpoints require `Authorization: Bearer <ADMIN_TOKEN>`. If `ADMIN_TOKEN`
//! is not configured in `AppState`, every request returns 503.
//!
//! Routes (mounted at `/admin`):
//!   GET    /admin/bug-report/stats              — rate limiter snapshot + ban list
//!   POST   /admin/bug-report/bans/{user_id}     — ban a user from bug reports
//!   DELETE /admin/bug-report/bans/{user_id}     — unban a user

use std::time::Instant;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::error::ErrorResponse;

type AdminResult<T> = Result<Json<T>, (StatusCode, Json<ErrorResponse>)>;

fn err(status: StatusCode, msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    (status, Json(ErrorResponse { error: msg.to_string() }))
}

/// Validate the Bearer token against `state.admin_token`. Returns Err if missing/wrong/unconfigured.
fn require_admin(headers: &HeaderMap, state: &AppState) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let Some(ref expected) = state.admin_token else {
        return Err(err(StatusCode::SERVICE_UNAVAILABLE, "admin endpoints not configured"));
    };
    let provided = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match provided {
        Some(token) if token == expected => Ok(()),
        _ => Err(err(StatusCode::UNAUTHORIZED, "unauthorized")),
    }
}

// ---------------------------------------------------------------------------
// GET /admin/bug-report/stats
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct BugReportStats {
    pub banned_users: Vec<BannedUserEntry>,
    pub active_ip_counts: Vec<IpCount>,
    pub active_user_counts: Vec<UserCount>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct BannedUserEntry {
    pub user_id: Uuid,
    pub banned_at: chrono::DateTime<chrono::Utc>,
    pub reason: String,
}

#[derive(Serialize)]
pub struct IpCount {
    pub ip: String,
    pub count: usize,
    pub at_limit: bool,
}

#[derive(Serialize)]
pub struct UserCount {
    pub user_id: Uuid,
    pub count: usize,
    pub at_limit: bool,
}

pub async fn get_bug_report_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AdminResult<BugReportStats> {
    require_admin(&headers, &state)?;

    let now = Instant::now();
    let bug_limit = state.bug_report_rate_limiter.max_requests();

    // Rate limiter snapshots
    let mut ip_counts: Vec<IpCount> = state
        .bug_report_rate_limiter
        .snapshot_ip_counts(now)
        .into_iter()
        .map(|(ip, count)| IpCount {
            ip: ip.to_string(),
            at_limit: count >= bug_limit,
            count,
        })
        .collect();
    ip_counts.sort_by(|a, b| b.count.cmp(&a.count));

    let mut user_counts: Vec<UserCount> = state
        .bug_report_rate_limiter
        .snapshot_user_counts(now)
        .into_iter()
        .map(|(user_id, count)| UserCount {
            user_id,
            at_limit: count >= bug_limit,
            count,
        })
        .collect();
    user_counts.sort_by(|a, b| b.count.cmp(&a.count));

    // Banned users from DB
    let banned_users: Vec<BannedUserEntry> = sqlx::query_as(
        "SELECT user_id, banned_at, reason FROM bug_report_bans ORDER BY banned_at DESC",
    )
    .fetch_all(&state.db_pool)
    .await
    .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    Ok(Json(BugReportStats { banned_users, active_ip_counts: ip_counts, active_user_counts: user_counts }))
}

// ---------------------------------------------------------------------------
// POST /admin/bug-report/bans/{user_id}
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct BanRequest {
    pub reason: Option<String>,
}

#[derive(Serialize)]
pub struct BanResponse {
    pub user_id: Uuid,
    pub banned: bool,
}

pub async fn ban_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
    Json(body): Json<BanRequest>,
) -> AdminResult<BanResponse> {
    require_admin(&headers, &state)?;

    let reason = body.reason.unwrap_or_default();

    sqlx::query(
        "INSERT INTO bug_report_bans (user_id, reason) VALUES ($1, $2) ON CONFLICT (user_id) DO UPDATE SET reason = EXCLUDED.reason, banned_at = NOW()",
    )
    .bind(user_id)
    .bind(&reason)
    .execute(&state.db_pool)
    .await
    .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    tracing::warn!(user_id = %user_id, reason = %reason, "admin banned user from bug reports");

    Ok(Json(BanResponse { user_id, banned: true }))
}

// ---------------------------------------------------------------------------
// DELETE /admin/bug-report/bans/{user_id}
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct UnbanResponse {
    pub user_id: Uuid,
    pub banned: bool,
}

pub async fn unban_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
) -> AdminResult<UnbanResponse> {
    require_admin(&headers, &state)?;

    sqlx::query("DELETE FROM bug_report_bans WHERE user_id = $1")
        .bind(user_id)
        .execute(&state.db_pool)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    tracing::info!(user_id = %user_id, "admin unbanned user from bug reports");

    Ok(Json(UnbanResponse { user_id, banned: false }))
}
