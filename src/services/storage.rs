use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use rustis::{
    client::Client as ValkeyClient,
    commands::{GenericCommands, StringCommands},
};
use serde::{Serialize, de::DeserializeOwned};

#[async_trait]
pub trait StorageService: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<String>>;
    async fn set(&self, key: &str, value: &str) -> Result<()>;
    async fn del(&self, key: &str) -> Result<bool>;
}

impl dyn StorageService {
    pub async fn has(&self, key: &str) -> Result<bool> {
        Ok(self.get(key).await?.is_some())
    }

    pub async fn save<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.set(key, &serde_json::to_string(value)?).await
    }

    pub async fn load<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let Some(value) = self.get(key).await? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(&value)?))
    }
}

#[derive(Default, Debug)]
pub struct InMemoryStorageService {
    store: DashMap<String, String>,
}

#[async_trait]
impl StorageService for InMemoryStorageService {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.store.get(key).map(|v| v.clone()))
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        self.store.insert(key.to_string(), value.to_string());
        Ok(())
    }

    async fn del(&self, key: &str) -> Result<bool> {
        Ok(self.store.remove(key).is_some())
    }
}

pub struct StorageServiceRedis {
    client: ValkeyClient,
}

impl StorageServiceRedis {
    pub fn new(client: ValkeyClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl StorageService for StorageServiceRedis {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.client.get(format!("storage:{key}")).await?)
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        Ok(self.client.set(format!("storage:{key}"), value).await?)
    }

    async fn del(&self, key: &str) -> Result<bool> {
        Ok(self.client.del(format!("storage:{key}")).await? != 0)
    }
}
