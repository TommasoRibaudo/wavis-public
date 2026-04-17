/// Voice Status Scenarios
///
/// Tests the voice status endpoint the GUI ChannelDetail screen depends on:
/// - GET /channels/{channelId}/voice — inactive session returns { active: false }
/// - GET /channels/{channelId}/voice — 403 for non-member
use std::time::Instant;

use async_trait::async_trait;

use crate::channel_ops;
use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct VoiceStatusScenario;

#[async_trait]
impl Scenario for VoiceStatusScenario {
    fn name(&self) -> &str {
        "voice-status"
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut failures: Vec<AssertionFailure> = Vec::new();

        // --- Setup ---
        let owner = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("owner register: {e}")),
        };
        let outsider = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("outsider register: {e}")),
        };

        let channel_id = match channel_ops::create_channel(&owner, "voice-status-test").await {
            Ok(id) => id,
            Err(e) => return err_result(self.name(), start, &e),
        };

        // ── Voice status — inactive ──
        check_voice_inactive(&owner, &channel_id, &mut failures).await;

        // ── Voice status — 403 for outsider ──
        check_voice_forbidden(&outsider, &channel_id, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

async fn check_voice_inactive(
    client: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match client.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels/{id}/voice request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::OK {
        failures.push(AssertionFailure {
            check: "GET /channels/{id}/voice status".into(),
            expected: "200".into(),
            actual: format!("{}", resp.status()),
        });
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "voice status parse".into(),
                expected: "valid JSON".into(),
                actual: format!("{e}"),
            });
            return;
        }
    };

    // GUI expects { active: bool, participant_count?: number, participants?: [] }
    match body.get("active").and_then(|v| v.as_bool()) {
        Some(false) => {} // expected
        Some(true) => {
            failures.push(AssertionFailure {
                check: "voice status active=false for idle channel".into(),
                expected: "false".into(),
                actual: "true".into(),
            });
        }
        None => {
            failures.push(AssertionFailure {
                check: "voice status has active field".into(),
                expected: "active boolean present".into(),
                actual: format!("{body}"),
            });
        }
    }
}

async fn check_voice_forbidden(
    outsider: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let resp = match outsider.get(&format!("/channels/{channel_id}/voice")).await {
        Ok(r) => r,
        Err(e) => {
            failures.push(AssertionFailure {
                check: "GET /channels/{id}/voice outsider".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    };

    if resp.status() != reqwest::StatusCode::FORBIDDEN {
        failures.push(AssertionFailure {
            check: "GET /channels/{id}/voice outsider returns 403".into(),
            expected: "403".into(),
            actual: format!("{}", resp.status()),
        });
    }
}

fn err_result(name: &str, start: Instant, msg: &str) -> ScenarioResult {
    ScenarioResult {
        name: name.to_owned(),
        passed: false,
        duration: start.elapsed(),
        failures: vec![AssertionFailure {
            check: "setup".into(),
            expected: "success".into(),
            actual: msg.to_owned(),
        }],
    }
}
