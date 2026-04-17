use anyhow::{Context, anyhow};
use aws_config::BehaviorVersion;
use aws_sdk_ec2::Client;
use aws_sdk_ec2::config::Region;
use aws_sdk_ec2::types::InstanceStateName;
use tokio::sync::OnceCell;

pub struct Ec2InstanceController {
    client: OnceCell<Client>,
    instance_id: String,
    region: Option<String>,
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
    pub fn new(instance_id: String) -> Self {
        Self {
            client: OnceCell::new(),
            instance_id,
            region: std::env::var("LIVEKIT_EC2_REGION").ok(),
        }
    }

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

    pub async fn start_instance(&self) -> Result<(), anyhow::Error> {
        self.client()
            .await
            .start_instances()
            .instance_ids(self.instance_id.clone())
            .send()
            .await
            .with_context(|| format!("failed to start EC2 instance {}", self.instance_id))?;

        Ok(())
    }

    pub async fn stop_instance(&self) -> Result<(), anyhow::Error> {
        self.client()
            .await
            .stop_instances()
            .instance_ids(self.instance_id.clone())
            .send()
            .await
            .with_context(|| format!("failed to stop EC2 instance {}", self.instance_id))?;

        Ok(())
    }

    pub async fn describe_state(&self) -> Result<Ec2InstanceState, anyhow::Error> {
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
