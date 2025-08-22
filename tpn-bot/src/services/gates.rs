use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use rustis::{
    client::Client as ValkeyClient,
    commands::{SetCondition, SetExpiration, StringCommands},
};

#[async_trait]
pub trait GateService: Send + Sync {
    async fn gate(&self, key: &str, period: Duration) -> Result<bool>;
}

pub struct GateServiceNoop;

#[async_trait]
impl GateService for GateServiceNoop {
    async fn gate(&self, _key: &str, _period: Duration) -> Result<bool> {
        Ok(true)
    }
}

pub struct GateServiceRedis {
    client: ValkeyClient,
}

impl GateServiceRedis {
    pub fn new(client: ValkeyClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl GateService for GateServiceRedis {
    async fn gate(&self, key: &str, period: Duration) -> Result<bool> {
        let gate: Option<String> = self
            .client
            .set_get_with_options(
                format!("gate:{key}"),
                "1",
                SetCondition::NX,
                SetExpiration::Px(period.as_millis() as u64),
                false,
            )
            .await?;
        if gate.is_some() {
            tracing::trace!(?period, key, "gated");
            Ok(false)
        } else {
            Ok(true)
        }
    }
}
