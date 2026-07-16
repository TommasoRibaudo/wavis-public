#[cfg(not(windows))]
use anyhow::Context;
use anyhow::anyhow;
use async_trait::async_trait;
#[cfg(not(windows))]
use aws_config::BehaviorVersion;
#[cfg(not(windows))]
use aws_sdk_ec2::Client;
#[cfg(not(windows))]
use aws_sdk_ec2::config::Region;
#[cfg(not(windows))]
use aws_sdk_ec2::types::InstanceStateName;
#[cfg(not(windows))]
use tokio::sync::OnceCell;

/// Seam over EC2 instance control so `trigger_ec2_stop`'s success/failure
/// branches are testable without a real AWS instance. `Ec2InstanceController`
/// is the only production implementation; tests substitute a fake.
#[async_trait]
pub trait InstanceController: Send + Sync {
    async fn start_instance(&self) -> Result<(), anyhow::Error>;
    async fn stop_instance(&self) -> Result<(), anyhow::Error>;
    async fn describe_state(&self) -> Result<Ec2InstanceState, anyhow::Error>;
}

#[cfg(not(windows))]
pub struct Ec2InstanceController {
    client: OnceCell<Client>,
    instance_id: String,
    region: Option<String>,
}

// On native Windows builds, `AppState::new` never constructs an EC2 controller
// (see app_state.rs — `ec2_controller` is hardcoded `None`, since production
// servers are Linux-only). This variant exists only so the crate compiles
// uniformly across platforms and is exercised directly by a unit test below,
// not by any production path — hence `allow(dead_code)` here specifically.
#[cfg(windows)]
#[allow(dead_code)]
pub struct Ec2InstanceController {
    instance_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ec2InstanceState {
    Running,
    Stopped,
    Starting,
    Stopping,
    Other(String),
}

impl Ec2InstanceController {
    #[cfg(not(windows))]
    pub fn new(instance_id: String) -> Self {
        Self {
            client: OnceCell::new(),
            instance_id,
            region: std::env::var("LIVEKIT_EC2_REGION").ok(),
        }
    }

    #[cfg(windows)]
    #[allow(dead_code)]
    pub fn new(instance_id: String) -> Self {
        Self { instance_id }
    }

    #[cfg(not(windows))]
    async fn client(&self) -> &Client {
        self.client
            .get_or_init(|| async {
                let mut loader = aws_config::defaults(BehaviorVersion::latest());
                if let Some(region) = &self.region {
                    loader = loader.region(Region::new(region.clone()));
                }
                let config = loader.load().await;
                Client::new(&config)
            })
            .await
    }
}

#[async_trait]
impl InstanceController for Ec2InstanceController {
    #[cfg(not(windows))]
    async fn start_instance(&self) -> Result<(), anyhow::Error> {
        self.client()
            .await
            .start_instances()
            .instance_ids(self.instance_id.clone())
            .send()
            .await
            .with_context(|| format!("failed to start EC2 instance {}", self.instance_id))?;

        Ok(())
    }

    #[cfg(windows)]
    async fn start_instance(&self) -> Result<(), anyhow::Error> {
        Err(anyhow!(
            "EC2 control is not available on Windows builds for instance {}",
            self.instance_id
        ))
    }

    #[cfg(not(windows))]
    async fn stop_instance(&self) -> Result<(), anyhow::Error> {
        self.client()
            .await
            .stop_instances()
            .instance_ids(self.instance_id.clone())
            .send()
            .await
            .with_context(|| format!("failed to stop EC2 instance {}", self.instance_id))?;

        Ok(())
    }

    #[cfg(windows)]
    async fn stop_instance(&self) -> Result<(), anyhow::Error> {
        Err(anyhow!(
            "EC2 control is not available on Windows builds for instance {}",
            self.instance_id
        ))
    }

    #[cfg(not(windows))]
    async fn describe_state(&self) -> Result<Ec2InstanceState, anyhow::Error> {
        let output = self
            .client()
            .await
            .describe_instances()
            .instance_ids(self.instance_id.clone())
            .send()
            .await
            .with_context(|| format!("failed to describe EC2 instance {}", self.instance_id))?;

        let instance = output
            .reservations()
            .iter()
            .flat_map(|reservation| reservation.instances())
            .next()
            .ok_or_else(|| anyhow!("EC2 instance {} not found", self.instance_id))?;

        let state_name = instance
            .state()
            .and_then(|state| state.name())
            .ok_or_else(|| anyhow!("EC2 instance {} has no state", self.instance_id))?;

        Ok(match state_name {
            InstanceStateName::Running => Ec2InstanceState::Running,
            InstanceStateName::Stopped => Ec2InstanceState::Stopped,
            InstanceStateName::Pending => Ec2InstanceState::Starting,
            InstanceStateName::Stopping => Ec2InstanceState::Stopping,
            other => Ec2InstanceState::Other(format!("{other:?}")),
        })
    }

    #[cfg(windows)]
    async fn describe_state(&self) -> Result<Ec2InstanceState, anyhow::Error> {
        Err(anyhow!(
            "EC2 control is not available on Windows builds for instance {}",
            self.instance_id
        ))
    }
}

/// Stop the LiveKit EC2 instance and mark SFU health as Unavailable.
///
/// Shared by the 1 AM idle scheduler (`main.rs`) and the last-room-close hook
/// (`ws_session.rs`). Defined here so both call sites use the same logic.
pub async fn trigger_ec2_stop(app_state: &crate::app_state::AppState) {
    if let Some(ref ec2) = app_state.ec2_controller {
        tracing::info!("Stopping LiveKit EC2 instance");
        if let Err(error) = ec2.stop_instance().await {
            tracing::error!(%error, "Failed to stop LiveKit EC2 instance");
            return;
        }

        *app_state.sfu_health_status.write().await =
            crate::voice::sfu_bridge::SfuHealth::Unavailable(
                "EC2 stopped by idle scheduler".to_string(),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abuse::join_rate_limiter::{JoinRateLimiter, JoinRateLimiterConfig};
    use crate::app_state::AppState;
    use crate::auth::auth_rate_limiter::{AuthRateLimiter, AuthRateLimiterConfig};
    use crate::channel::invite::{InviteStore, InviteStoreConfig};
    use crate::diagnostics::bug_report::MockGitHubClient;
    use crate::diagnostics::llm_client::NoOpLlmClient;
    use crate::ip::IpConfig;
    use crate::voice::mock_sfu_bridge::MockSfuBridge;
    use crate::voice::sfu_bridge::{SfuHealth, SfuRoomManager, SfuSignalingProxy};
    use std::sync::Arc;

    /// A fake `InstanceController` whose `stop_instance` result is configurable.
    /// `trigger_ec2_stop` never calls start_instance/describe_state, so those
    /// are stubbed with an arbitrary Ok result to satisfy the trait.
    struct FakeInstanceController {
        stop_result: Result<(), String>,
    }

    #[async_trait]
    impl InstanceController for FakeInstanceController {
        async fn start_instance(&self) -> Result<(), anyhow::Error> {
            Ok(())
        }

        async fn stop_instance(&self) -> Result<(), anyhow::Error> {
            self.stop_result.clone().map_err(|e| anyhow!(e))
        }

        async fn describe_state(&self) -> Result<Ec2InstanceState, anyhow::Error> {
            Ok(Ec2InstanceState::Running)
        }
    }

    /// Build a minimal AppState for trigger_ec2_stop tests. Never touches the
    /// DB, so a lazy (never-connecting) pool is enough.
    fn build_test_app_state(ec2_controller: Option<Arc<dyn InstanceController>>) -> AppState {
        let mock = Arc::new(MockSfuBridge::new());
        let mut app_state = AppState::new(
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
        app_state.ec2_controller = ec2_controller;
        app_state
    }

    #[tokio::test]
    async fn trigger_ec2_stop_ok_marks_sfu_unavailable() {
        let controller = FakeInstanceController {
            stop_result: Ok(()),
        };
        let app_state =
            build_test_app_state(Some(Arc::new(controller) as Arc<dyn InstanceController>));
        *app_state.sfu_health_status.write().await = SfuHealth::Available;

        trigger_ec2_stop(&app_state).await;

        assert_eq!(
            *app_state.sfu_health_status.read().await,
            SfuHealth::Unavailable("EC2 stopped by idle scheduler".to_string())
        );
    }

    #[tokio::test]
    async fn trigger_ec2_stop_err_leaves_health_unchanged() {
        let controller = FakeInstanceController {
            stop_result: Err("AWS API unreachable".to_string()),
        };
        let app_state =
            build_test_app_state(Some(Arc::new(controller) as Arc<dyn InstanceController>));
        *app_state.sfu_health_status.write().await = SfuHealth::Available;

        trigger_ec2_stop(&app_state).await;

        assert_eq!(
            *app_state.sfu_health_status.read().await,
            SfuHealth::Available,
            "a stop_instance failure must leave SFU health untouched"
        );
    }

    #[tokio::test]
    async fn trigger_ec2_stop_with_no_controller_is_noop() {
        let app_state = build_test_app_state(None);
        *app_state.sfu_health_status.write().await = SfuHealth::Available;

        trigger_ec2_stop(&app_state).await;

        assert_eq!(
            *app_state.sfu_health_status.read().await,
            SfuHealth::Available,
            "with no EC2 controller configured, trigger_ec2_stop must be a no-op"
        );
    }

    /// The real, Windows-native `Ec2InstanceController` always fails, so it
    /// exercises the same error path without needing a fake — the seam exists
    /// for the Ok path above, not because this type can't be tested directly.
    /// Windows-only: on other platforms `Ec2InstanceController` makes real AWS
    /// SDK calls, which would make this test network-dependent.
    #[cfg(windows)]
    #[tokio::test]
    async fn real_windows_controller_stop_instance_always_errs() {
        let controller = Ec2InstanceController::new("i-test".to_string());
        assert!(controller.stop_instance().await.is_err());
    }
}
