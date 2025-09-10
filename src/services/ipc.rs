use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use rustis::{client::Client as ValkeyClient, commands::PubSubCommands};

use crate::injector_getter;

#[async_trait]
pub trait IpcService: Send + Sync {
    async fn publish(&self, channel: &str, message: &[u8]) -> Result<()>;
    async fn listen(&self, channel: &str) -> Result<Option<Vec<u8>>>;
}

injector_getter!(IpcService::ipc);

pub struct IpcServiceRedis {
    client: ValkeyClient,
}

impl IpcServiceRedis {
    pub fn new(client: ValkeyClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl IpcService for IpcServiceRedis {
    async fn publish(&self, channel: &str, message: &[u8]) -> Result<()> {
        self.client.publish(channel, message).await?;
        Ok(())
    }

    async fn listen(&self, channel: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .client
            .subscribe(channel)
            .await?
            .next()
            .await
            .and_then(|r| r.ok())
            .map(|msg| msg.payload))
    }
}
