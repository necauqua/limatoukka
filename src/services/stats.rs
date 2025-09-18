use anyhow::Result;
use async_trait::async_trait;
use rustis::{
    client::{BatchPreparedCommand, Client as ValkeyClient},
    commands::HashCommands,
};

use crate::injector_getter;

#[async_trait]
pub trait StatsService: Send + Sync {
    /// Implies record_global
    async fn record(&self, user: &str, event: &str) -> Result<()>;

    async fn record_global(&self, event: &str) -> Result<()>;

    async fn get(&self, user: &str, event: &str) -> Result<u64>;

    async fn get_global(&self, event: &str) -> Result<u64>;
}

injector_getter!(StatsService::stats);

pub struct StatsServiceRedis {
    client: ValkeyClient,
}

impl StatsServiceRedis {
    pub fn new(client: ValkeyClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl StatsService for StatsServiceRedis {
    async fn record(&self, user: &str, event: &str) -> Result<()> {
        let mut t = self.client.create_transaction();

        t.hincrby(format!("stats:{user}"), event, 1).forget();
        t.hincrby("stats:global", event, 1).forget();

        t.execute::<[(); 0]>().await?;

        Ok(())
    }

    async fn record_global(&self, event: &str) -> Result<()> {
        self.client.hincrby("stats:global", event, 1).await?;
        Ok(())
    }

    async fn get(&self, user: &str, event: &str) -> Result<u64> {
        Ok(self.client.hget(format!("stats:{user}"), event).await?)
    }

    async fn get_global(&self, event: &str) -> Result<u64> {
        Ok(self.client.hget("stats:global", event).await?)
    }
}

pub struct StatsServiceNoop;

#[async_trait]
impl StatsService for StatsServiceNoop {
    async fn record(&self, user: &str, event: &str) -> Result<()> {
        tracing::info!(user, event, "record");
        Ok(())
    }

    async fn record_global(&self, event: &str) -> Result<()> {
        tracing::info!(event, "record_global");
        Ok(())
    }

    async fn get(&self, user: &str, event: &str) -> Result<u64> {
        tracing::info!(user, event, "get");
        Ok(0)
    }

    async fn get_global(&self, event: &str) -> Result<u64> {
        tracing::info!(event, "get_global");
        Ok(0)
    }
}
