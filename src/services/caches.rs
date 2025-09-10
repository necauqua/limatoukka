use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
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

        self.set(
            cache,
            ttl,
            key,
            &serde_json::to_string(&value).map_err(|e| anyhow!(e))?,
        )
        .await?;

        Ok(value)
    }
}

pub struct CacheServiceRedis {
    client: ValkeyClient,
}

impl CacheServiceRedis {
    pub fn new(client: ValkeyClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl CacheService for CacheServiceRedis {
    async fn set(&self, cache: &str, ttl: Duration, key: &str, value: &str) -> Result<bool> {
        Ok(self
            .client
            .set_with_options(
                format!("caches:{cache}:{key}"),
                value,
                SetCondition::NX,
                SetExpiration::Px(ttl.as_millis() as _),
                false,
            )
            .await?)
    }

    async fn get(&self, cache: &str, key: &str) -> Result<Option<String>> {
        let result: Option<String> = self.client.get(format!("caches:{cache}:{key}")).await?;
        Ok(result)
    }
}
