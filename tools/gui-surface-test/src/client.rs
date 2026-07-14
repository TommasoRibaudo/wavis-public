use serde::Deserialize;

/// Authenticated HTTP client for surface tests.
///
/// Registers a closed-alpha user, obtains a bearer token, and provides helpers
/// for making authenticated requests to the backend REST API.
pub struct AuthenticatedClient {
    pub http: reqwest::Client,
    pub base_url: String,
    pub access_token: String,
    pub device_id: String,
    pub user_id: String,
}

#[derive(Deserialize)]
struct RegisterResponse {
    device_id: String,
    user_id: String,
    access_token: String,
    #[allow(dead_code)]
    refresh_token: String,
}

impl AuthenticatedClient {
    /// Register a new user and return an authenticated client.
    pub async fn register(base_url: &str, http: &reqwest::Client) -> Result<Self, String> {
        let invite_code = std::env::var("ALPHA_INVITE_CODE")
            .map_err(|_| "ALPHA_INVITE_CODE must be set for surface auth seeding".to_string())?;
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let url = format!("{base_url}/auth/register");
        let resp = http
            .post(&url)
            .json(&serde_json::json!({
                "phrase": format!("surface-password-{suffix}"),
                "username": format!("Surface-{suffix}"),
                "inviteCode": invite_code,
            }))
            .send()
            .await
            .map_err(|e| format!("register request failed: {e}"))?;

        if resp.status() != reqwest::StatusCode::CREATED {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("register failed: {status} {body}"));
        }

        let reg: RegisterResponse = resp
            .json()
            .await
            .map_err(|e| format!("register parse failed: {e}"))?;

        Ok(Self {
            http: http.clone(),
            base_url: base_url.to_owned(),
            access_token: reg.access_token,
            device_id: reg.device_id,
            user_id: reg.user_id,
        })
    }

    /// GET request with bearer auth.
    pub async fn get(&self, path: &str) -> reqwest::Result<reqwest::Response> {
        self.http
            .get(format!("{}{path}", self.base_url))
            .header("Authorization", format!("Bearer {}", self.access_token))
            .send()
            .await
    }

    /// POST request with bearer auth and JSON body.
    pub async fn post(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> reqwest::Result<reqwest::Response> {
        self.http
            .post(format!("{}{path}", self.base_url))
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
    }

    /// POST request with bearer auth and no body.
    pub async fn post_empty(&self, path: &str) -> reqwest::Result<reqwest::Response> {
        self.http
            .post(format!("{}{path}", self.base_url))
            .header("Authorization", format!("Bearer {}", self.access_token))
            .send()
            .await
    }

    /// PUT request with bearer auth and JSON body.
    pub async fn put(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> reqwest::Result<reqwest::Response> {
        self.http
            .put(format!("{}{path}", self.base_url))
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
    }

    /// DELETE request with bearer auth.
    pub async fn delete(&self, path: &str) -> reqwest::Result<reqwest::Response> {
        self.http
            .delete(format!("{}{path}", self.base_url))
            .header("Authorization", format!("Bearer {}", self.access_token))
            .send()
            .await
    }
}
