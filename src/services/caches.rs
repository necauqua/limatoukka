use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use dashmap::DashMap;
use rustis::{
    client::Client as ValkeyClient,
    commands::{SetCondition, SetExpiration, StringCommands},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::injector_getter;

#[async_trait]
pub trait CacheService: Send + Sync {
    async fn set(&self, cache: &str, ttl: Duration, key: &str, value: &str) -> Result<bool>;

    async fn get(&self, cache: &str, key: &str) -> Result<Option<String>>;
}

injector_getter!(CacheService::caches);

impl dyn CacheService {
    /// Note that this is not atomic
    pub async fn get_cached<T, E>(
        &self,
        cache: &str,
        ttl: Duration,
        key: &str,
        compute: impl AsyncFnOnce() -> Result<T, E>,
    ) -> Result<T, E>
    where
        T: Serialize + DeserializeOwned,
        E: From<anyhow::Error>,
    {
        if let Some(value) = self.get(cache, key).await? {
            return Ok(serde_json::from_str(&value).map_err(|e| anyhow!(e))?);
        }

        let value = compute().await?;
        let json = serde_json::to_string(&value).map_err(|e| anyhow!(e))?;

        self.set(cache, ttl, key, &json).await?;

        Ok(value)
    }
}

#[derive(Default)]
pub struct CacheServiceInMemory {
    memory: DashMap<String, (String, Instant)>,
}

#[async_trait]
impl CacheService for CacheServiceInMemory {
    async fn set(&self, cache: &str, ttl: Duration, key: &str, value: &str) -> Result<bool> {
        let full_key = format!("{cache}:{key}");
        let now = Instant::now();
        if self.get(cache, key).await?.is_some() {
            return Ok(false);
        }
        self.memory.insert(full_key, (value.to_string(), now + ttl));
        Ok(true)
    }

    async fn get(&self, cache: &str, key: &str) -> Result<Option<String>> {
        let full_key = format!("{cache}:{key}");
        let Some(entry) = self.memory.get(&full_key) else {
            return Ok(None);
        };
        if Instant::now() < entry.1 {
            return Ok(Some(entry.0.clone()));
        }
        self.memory.remove(&full_key);
        Ok(None)
    }
}

pub struct CacheServiceValkey {
    client: ValkeyClient,
}

impl CacheServiceValkey {
    pub fn new(client: ValkeyClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl CacheService for CacheServiceValkey {
    async fn set(&self, cache: &str, ttl: Duration, key: &str, value: &str) -> Result<bool> {
        Ok(self
            .client
            .set_with_options(
                format!("caches:{cache}:{key}"),
                value,
                SetCondition::NX,
                SetExpiration::Px(ttl.as_millis() as _),
            )
            .await?)
    }

    async fn get(&self, cache: &str, key: &str) -> Result<Option<String>> {
        let result: Option<String> = self.client.get(format!("caches:{cache}:{key}")).await?;
        Ok(result)
    }
}
