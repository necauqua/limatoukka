use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use rustis::{
    client::{BatchPreparedCommand, Client as ValkeyClient},
    commands::{GenericCommands, SetExpiration, StringCommands},
};

use crate::{injector_getter, services::Service};

#[derive(Debug)]
pub enum BanishStatus {
    Good,
    Temporary(Duration),
    Banished,
}

impl BanishStatus {
    pub fn is_banished(&self) -> bool {
        !matches!(self, BanishStatus::Good)
    }
}

#[async_trait]
pub trait BanishService: Service {
    async fn banish(&self, user_id: &str, duration: Option<Duration>) -> Result<bool>;

    async fn unbanish(&self, user_id: &str) -> Result<bool>;

    async fn status(&self, user_id: &str) -> Result<BanishStatus>;
}

injector_getter!(BanishService::banishes { BanishServiceNoop });

pub struct BanishServiceValkey {
    client: ValkeyClient,
}

impl BanishServiceValkey {
    pub fn new(client: ValkeyClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl BanishService for BanishServiceValkey {
    async fn banish(&self, user_id: &str, duration: Option<Duration>) -> Result<bool> {
        let key = format!("kick:begone:{user_id}");

        let mut t = self.client.create_transaction();
        t.exists(&key).queue();

        if let Some(duration) = duration {
            t.set_with_options(key, 1, None, SetExpiration::Px(duration.as_millis() as _))
                .forget();
        } else {
            t.set(key, 1).forget();
        }
        let res: usize = t.execute().await?;

        Ok(res == 0)
    }

    async fn unbanish(&self, user_id: &str) -> Result<bool> {
        let removed = self.client.del(format!("kick:begone:{user_id}")).await?;
        Ok(removed != 0)
    }

    async fn status(&self, user_id: &str) -> Result<BanishStatus> {
        let res = match self
            .client
            .pexpiretime(format!("kick:begone:{user_id}"))
            .await?
        {
            -2 => BanishStatus::Good,
            -1 => BanishStatus::Banished,
            time => {
                let now = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_millis();
                let left = Duration::from_millis((time - now as i64).unsigned_abs());
                BanishStatus::Temporary(left)
            }
        };
        Ok(res)
    }
}

/// Keeps the banishes in memory.
#[derive(Default)]
pub struct BanishServiceInMemory {
    /// user id -> the end of a temporary banish, `None` for a permanent one
    banishes: DashMap<String, Option<Instant>>,
}

#[async_trait]
impl BanishService for BanishServiceInMemory {
    async fn banish(&self, user_id: &str, duration: Option<Duration>) -> Result<bool> {
        let existed = self.status(user_id).await?.is_banished();
        self.banishes
            .insert(user_id.to_owned(), duration.map(|d| Instant::now() + d));
        Ok(!existed)
    }

    async fn unbanish(&self, user_id: &str) -> Result<bool> {
        let existed = self.status(user_id).await?.is_banished();
        self.banishes.remove(user_id);
        Ok(existed)
    }

    async fn status(&self, user_id: &str) -> Result<BanishStatus> {
        let Some(until) = self.banishes.get(user_id).map(|e| *e) else {
            return Ok(BanishStatus::Good);
        };
        let Some(until) = until else {
            return Ok(BanishStatus::Banished);
        };
        let now = Instant::now();
        if until <= now {
            self.banishes.remove(user_id);
            return Ok(BanishStatus::Good);
        }
        Ok(BanishStatus::Temporary(until - now))
    }
}

pub struct BanishServiceNoop;

#[async_trait]
impl BanishService for BanishServiceNoop {
    async fn banish(&self, _user_id: &str, _duration: Option<Duration>) -> Result<bool> {
        Ok(true)
    }

    async fn unbanish(&self, _user_id: &str) -> Result<bool> {
        Ok(true)
    }

    async fn status(&self, _user_id: &str) -> Result<BanishStatus> {
        Ok(BanishStatus::Good)
    }
}
