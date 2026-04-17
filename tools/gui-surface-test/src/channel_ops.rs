use serde::Deserialize;

use crate::client::AuthenticatedClient;

#[derive(Deserialize)]
pub struct CreateChannelResponse {
    pub channel_id: String,
    #[allow(dead_code)]
    pub name: String,
}

#[derive(Deserialize)]
pub struct InviteResponse {
    pub code: String,
    #[allow(dead_code)]
    pub channel_id: String,
}

/// Create a channel owned by `owner`. Returns the channel_id.
pub async fn create_channel(owner: &AuthenticatedClient, name: &str) -> Result<String, String> {
    let resp = owner
        .post("/channels", &serde_json::json!({ "name": name }))
        .await
        .map_err(|e| format!("create channel request failed: {e}"))?;

    if resp.status() != reqwest::StatusCode::CREATED {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("create channel failed: {status} {body}"));
    }

    let ch: CreateChannelResponse = resp
        .json()
        .await
        .map_err(|e| format!("create channel parse failed: {e}"))?;

    Ok(ch.channel_id)
}

/// Create an invite for a channel. Returns the invite code.
pub async fn create_invite(
    client: &AuthenticatedClient,
    channel_id: &str,
) -> Result<String, String> {
    let resp = client
        .post(
            &format!("/channels/{channel_id}/invites"),
            &serde_json::json!({}),
        )
        .await
        .map_err(|e| format!("create invite request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("create invite failed: {status} {body}"));
    }

    let inv: InviteResponse = resp
        .json()
        .await
        .map_err(|e| format!("create invite parse failed: {e}"))?;

    Ok(inv.code)
}

/// Join a channel using an invite code.
pub async fn join_channel(
    client: &AuthenticatedClient,
    _channel_id: &str,
    invite_code: &str,
) -> Result<(), String> {
    let resp = client
        .post(
            "/channels/join",
            &serde_json::json!({ "code": invite_code }),
        )
        .await
        .map_err(|e| format!("join channel request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("join channel failed: {status} {body}"));
    }

    Ok(())
}

/// Promote a member to admin. Requires owner privileges.
pub async fn promote_to_admin(
    owner: &AuthenticatedClient,
    channel_id: &str,
    user_id: &str,
) -> Result<(), String> {
    let resp = owner
        .put(
            &format!("/channels/{channel_id}/members/{user_id}/role"),
            &serde_json::json!({ "role": "admin" }),
        )
        .await
        .map_err(|e| format!("promote request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("promote failed: {status} {body}"));
    }

    Ok(())
}
