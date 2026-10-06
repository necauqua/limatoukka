use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use futures::StreamExt;
use rustis::{client::Client as ValkeyClient, commands::PubSubCommands, resp::RefBulkString};
use tokio::sync::broadcast;

use crate::{injector_getter, services::Service};

#[async_trait]
pub trait IpcService: Service {
    async fn publish(&self, channel: &str, message: &[u8]) -> Result<()>;

    async fn listen(&self, channel: &str) -> Result<Option<Vec<u8>>>;
}

injector_getter!(IpcService::ipc);

/// Delivers the messages only inside this process.
#[derive(Default)]
pub struct IpcServiceInMemory {
    channels: DashMap<String, broadcast::Sender<Vec<u8>>>,
}

impl IpcServiceInMemory {
    fn channel(&self, channel: &str) -> broadcast::Sender<Vec<u8>> {
        self.channels
            .entry(channel.to_owned())
            .or_insert_with(|| broadcast::Sender::new(16))
            .clone()
    }
}

#[async_trait]
impl IpcService for IpcServiceInMemory {
    async fn publish(&self, channel: &str, message: &[u8]) -> Result<()> {
        // no receivers is not an error, same as with valkey
        _ = self.channel(channel).send(message.to_vec());
        Ok(())
    }

    async fn listen(&self, channel: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.channel(channel).subscribe().recv().await.ok())
    }
}

pub struct IpcServiceValkey {
    client: ValkeyClient,
}

impl IpcServiceValkey {
    pub fn new(client: ValkeyClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl IpcService for IpcServiceValkey {
    async fn publish(&self, channel: &str, message: &[u8]) -> Result<()> {
        self.client
            .publish(channel, RefBulkString::new(message))
            .await?;
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
            .map(|msg| msg.payload().to_vec()))
    }
}
