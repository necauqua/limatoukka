use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use dashmap::{DashMap, mapref::entry::Entry};
use rustis::{
    client::Client as ValkeyClient,
    commands::{GenericCommands, SetCondition, SetExpiration, StringCommands},
};

use crate::{
    commands::{CommandResult, runner::CommandError},
    injector_getter,
    services::Service,
};

#[async_trait]
pub trait GateService: Service {
    async fn gate(&self, user: &str, key: &str, period: Duration) -> Result<bool>;

    async fn ungate(&self, user: &str, key: &str) -> Result<()>;

    async fn ungate_all(&self, user: &str) -> Result<()>;
}

injector_getter!(GateService::gates { GateServiceNoop });

impl dyn GateService {
    pub async fn command_gates(
        &self,
        user: &str,
        key: &str,
        global: Option<Duration>,
        sender: Option<Duration>,
    ) -> CommandResult {
        if let Some(global) = &global
            && !self.gate("global", key, *global).await?
        {
            return Err(CommandError::GlobalTimeout(*global));
        }
        if let Some(sender) = &sender
            && !self.gate(user, key, *sender).await?
        {
            return Err(CommandError::SenderTimeout(*sender));
        }
        Ok(())
    }

    pub async fn command_ungate(
        &self,
        user: &str,
        key: &str,
        global: Option<Duration>,
        sender: Option<Duration>,
    ) -> Result<()> {
        if global.is_some() {
            self.ungate("global", key).await?;
        }
        if sender.is_some() {
            self.ungate(user, key).await?;
        }
        Ok(())
    }
}

pub struct GateServiceValkey {
    client: ValkeyClient,
}

impl GateServiceValkey {
    pub fn new(client: ValkeyClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl GateService for GateServiceValkey {
    async fn gate(&self, user: &str, key: &str, period: Duration) -> Result<bool> {
        let gate: Option<String> = self
            .client
            .set_get_with_options(
                format!("gate:{user}:{key}"),
                "1",
                SetCondition::NX,
                SetExpiration::Px(period.as_millis() as u64),
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

/// Keeps the gates in memory.
#[derive(Default)]
pub struct GateServiceInMemory {
    /// `user:key` → the time when the gate opens again
    gates: DashMap<String, Instant>,
}

#[async_trait]
impl GateService for GateServiceInMemory {
    async fn gate(&self, user: &str, key: &str, period: Duration) -> Result<bool> {
        let now = Instant::now();
        match self.gates.entry(format!("{user}:{key}")) {
            Entry::Occupied(o) if *o.get() > now => {
                tracing::trace!(?period, key, "gated");
                Ok(false)
            }
            Entry::Occupied(mut o) => {
                o.insert(now + period);
                Ok(true)
            }
            Entry::Vacant(v) => {
                v.insert(now + period);
                Ok(true)
            }
        }
    }

    async fn ungate(&self, user: &str, key: &str) -> Result<()> {
        self.gates.remove(&format!("{user}:{key}"));
        Ok(())
    }

    async fn ungate_all(&self, user: &str) -> Result<()> {
        let prefix = format!("{user}:");
        self.gates.retain(|k, _| !k.starts_with(&prefix));
        Ok(())
    }
}

pub struct GateServiceNoop;

#[async_trait]
impl GateService for GateServiceNoop {
    async fn gate(&self, user: &str, key: &str, period: Duration) -> Result<bool> {
        tracing::info!(user, key, ?period, "gate");
        Ok(true)
    }

    async fn ungate(&self, user: &str, key: &str) -> Result<()> {
        tracing::info!(user, key, "ungate");
        Ok(())
    }

    async fn ungate_all(&self, user: &str) -> Result<()> {
        tracing::info!(user, "ungate_all");
        Ok(())
    }
}
