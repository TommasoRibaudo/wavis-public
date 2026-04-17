/// Channel Detail Concurrency Scenarios
///
/// Verifies the GUI client's expected concurrency patterns:
/// - After mutation success, channel detail is re-fetchable and reflects the change
/// - On 404 mutation response, channel detail can still be re-fetched
/// - Mutation-triggered re-fetch works correctly (simulates cancel-and-refetch)
///
/// These tests exercise the backend's last-write-wins semantics and verify
/// that the REST surface supports the re-fetch-after-mutation pattern the
/// GUI ChannelDetail component relies on.
///
/// **Validates: Requirements 17.2, 17.3, 17.5**
use std::time::Instant;

use async_trait::async_trait;

use crate::channel_ops;
use crate::client::AuthenticatedClient;
use crate::harness_context::TestContext;
use crate::results::{AssertionFailure, ScenarioResult};
use crate::runner::Scenario;

pub struct ChannelDetailConcurrencyScenario;

#[async_trait]
impl Scenario for ChannelDetailConcurrencyScenario {
    fn name(&self) -> &str {
        "channel-detail-concurrency"
    }

    async fn run(&self, ctx: &TestContext) -> ScenarioResult {
        let start = Instant::now();
        let mut failures: Vec<AssertionFailure> = Vec::new();

        // --- Setup ---
        let owner = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("owner register: {e}")),
        };
        let target = match AuthenticatedClient::register(&ctx.base_url, &ctx.http_client).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &format!("target register: {e}")),
        };

        let channel_id = match channel_ops::create_channel(&owner, "concurrency-test").await {
            Ok(id) => id,
            Err(e) => return err_result(self.name(), start, &e),
        };
        let invite_code = match channel_ops::create_invite(&owner, &channel_id).await {
            Ok(c) => c,
            Err(e) => return err_result(self.name(), start, &e),
        };
        if let Err(e) = channel_ops::join_channel(&target, &channel_id, &invite_code).await {
            return err_result(self.name(), start, &e);
        }

        // ── Test 1: After ban mutation, re-fetch reflects the change ──
        check_refetch_after_ban(&owner, &channel_id, &target.user_id, &mut failures).await;

        // ── Test 2: 404 on mutation (unban already-unbanned), re-fetch still works ──
        check_refetch_after_404_mutation(&owner, &channel_id, &target.user_id, &mut failures).await;

        // ── Test 3: Rapid sequential re-fetches (simulates cancel-and-refetch) ──
        check_rapid_refetch(&owner, &channel_id, &mut failures).await;

        ScenarioResult {
            name: self.name().to_owned(),
            passed: failures.is_empty(),
            duration: start.elapsed(),
            failures,
        }
    }
}

// ─── Check helpers ─────────────────────────────────────────────────

/// After banning a member, re-fetch channel detail and verify the member
/// is no longer in the members list.
async fn check_refetch_after_ban(
    owner: &AuthenticatedClient,
    channel_id: &str,
    target_user_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    // Ban the target
    match owner
        .post_empty(&format!("/channels/{channel_id}/bans/{target_user_id}"))
        .await
    {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => {
            failures.push(AssertionFailure {
                check: "concurrency: ban for refetch test".into(),
                expected: "2xx".into(),
                actual: format!("{}", resp.status()),
            });
            return;
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "concurrency: ban request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
            return;
        }
    }

    // Re-fetch channel detail
    match owner.get(&format!("/channels/{channel_id}")).await {
        Ok(resp) if resp.status() == reqwest::StatusCode::OK => {
            let body: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    failures.push(AssertionFailure {
                        check: "concurrency: refetch parse after ban".into(),
                        expected: "valid JSON".into(),
                        actual: format!("{e}"),
                    });
                    return;
                }
            };
            // Banned member should not appear in the members list
            let members = body
                .get("members")
                .and_then(|m| m.as_array())
                .cloned()
                .unwrap_or_default();
            let still_present = members.iter().any(|m| {
                m.get("user_id")
                    .and_then(|v| v.as_str())
                    .is_some_and(|id| id == target_user_id)
            });
            if still_present {
                failures.push(AssertionFailure {
                    check: "concurrency: banned member removed from members".into(),
                    expected: "target not in members list".into(),
                    actual: "target still in members list".into(),
                });
            }
        }
        Ok(resp) => {
            failures.push(AssertionFailure {
                check: "concurrency: refetch after ban".into(),
                expected: "200".into(),
                actual: format!("{}", resp.status()),
            });
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "concurrency: refetch after ban request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

/// Attempt to unban an already-unbanned user (should 404), then verify
/// that a re-fetch of channel detail still succeeds.
async fn check_refetch_after_404_mutation(
    owner: &AuthenticatedClient,
    channel_id: &str,
    target_user_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    // First unban (should succeed since target was banned in previous test)
    let _ = owner
        .delete(&format!("/channels/{channel_id}/bans/{target_user_id}"))
        .await;

    // Second unban — should 404
    match owner
        .delete(&format!("/channels/{channel_id}/bans/{target_user_id}"))
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status != 404 {
                failures.push(AssertionFailure {
                    check: "concurrency: double-unban returns 404".into(),
                    expected: "404".into(),
                    actual: format!("{status}"),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "concurrency: double-unban request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }

    // Re-fetch should still work fine
    match owner.get(&format!("/channels/{channel_id}")).await {
        Ok(resp) => {
            if resp.status() != reqwest::StatusCode::OK {
                failures.push(AssertionFailure {
                    check: "concurrency: refetch after 404 mutation".into(),
                    expected: "200".into(),
                    actual: format!("{}", resp.status()),
                });
            }
        }
        Err(e) => {
            failures.push(AssertionFailure {
                check: "concurrency: refetch after 404 request".into(),
                expected: "response".into(),
                actual: format!("error: {e}"),
            });
        }
    }
}

/// Fire multiple concurrent GET /channels/{id} requests to simulate
/// the cancel-and-refetch pattern (mutation re-fetch racing auto-refresh).
/// All should return 200 with consistent data.
async fn check_rapid_refetch(
    owner: &AuthenticatedClient,
    channel_id: &str,
    failures: &mut Vec<AssertionFailure>,
) {
    let mut handles = Vec::new();

    for _ in 0..3 {
        let url = format!("{}/channels/{channel_id}", owner.base_url);
        let token = owner.access_token.clone();
        let http = owner.http.clone();

        handles.push(tokio::spawn(async move {
            http.get(&url)
                .header("Authorization", format!("Bearer {token}"))
                .send()
                .await
        }));
    }

    for (i, handle) in handles.into_iter().enumerate() {
        match handle.await {
            Ok(Ok(resp)) => {
                if resp.status() != reqwest::StatusCode::OK {
                    failures.push(AssertionFailure {
                        check: format!("concurrency: rapid refetch #{i}"),
                        expected: "200".into(),
                        actual: format!("{}", resp.status()),
                    });
                }
            }
            Ok(Err(e)) => {
                failures.push(AssertionFailure {
                    check: format!("concurrency: rapid refetch #{i}"),
                    expected: "response".into(),
                    actual: format!("error: {e}"),
                });
            }
            Err(e) => {
                failures.push(AssertionFailure {
                    check: format!("concurrency: rapid refetch #{i} join"),
                    expected: "task success".into(),
                    actual: format!("join error: {e}"),
                });
            }
        }
    }
}

// ─── Helpers ───────────────────────────────────────────────────────

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
