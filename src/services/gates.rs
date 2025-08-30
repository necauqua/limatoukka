use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use rustis::{
    client::Client as ValkeyClient,
    commands::{GenericCommands, SetCondition, SetExpiration, StringCommands},
};

#[async_trait]
pub trait GateService: Send + Sync {
    async fn gate(&self, user: &str, key: &str, period: Duration) -> Result<bool>;
    async fn ungate(&self, user: &str, key: &str) -> Result<()>;
    async fn ungate_all(&self, user: &str) -> Result<()>;
}

pub struct GateServiceNoop;

#[async_trait]
impl GateService for GateServiceNoop {
    async fn gate(&self, _user: &str, _key: &str, _period: Duration) -> Result<bool> {
        Ok(true)
    }
    async fn ungate(&self, _user: &str, _key: &str) -> Result<()> {
        Ok(())
    }
    async fn ungate_all(&self, _user: &str) -> Result<()> {
        Ok(())
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
    async fn gate(&self, user: &str, key: &str, period: Duration) -> Result<bool> {
        let gate: Option<String> = self
            .client
            .set_get_with_options(
                format!("gate:{user}:{key}"),
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

    async fn ungate(&self, user: &str, key: &str) -> Result<()> {
        self.client.del(format!("gate:{user}:{key}")).await?;
        Ok(())
    }

    async fn ungate_all(&self, user: &str) -> Result<()> {
        let keys: Vec<String> = self.client.keys(format!("gate:{user}:*")).await?;
        if !keys.is_empty() {
            self.client.del(keys).await?;
        }
        Ok(())
    }
}
