use std::{
    ops::Deref,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use anyhow::{Ok, Result};
use rustis::{
    client::BatchPreparedCommand,
    commands::{GenericCommands, ListCommands},
};
use tokio::time::sleep;

use crate::services::messaging::Message;

use super::app::AppContext;

struct MessageState {
    message: Message,
    repeats: AtomicU32,
}

#[derive(Clone)]
pub struct MessageContext {
    parent: AppContext,
    state: Arc<MessageState>,
}

// deref hack lol
impl Deref for MessageContext {
    type Target = AppContext;

    fn deref(&self) -> &Self::Target {
        &self.parent
    }
}

impl MessageContext {
    pub fn new(state: AppContext, message: Message) -> Self {
        Self {
            parent: state,
            state: Arc::new(MessageState {
                message,
                repeats: AtomicU32::new(0),
            }),
        }
    }

    pub fn message(&self) -> &Message {
        &self.state.message
    }

    pub fn inc_repeats(&self) -> u32 {
        self.state.repeats.fetch_add(1, Ordering::Relaxed)
    }

    pub fn reset_repeats(&self) {
        self.state.repeats.store(0, Ordering::Relaxed);
    }

    /// Returns true once (atomically) in the given period - per key and per sender.
    pub async fn sender_gate(&self, key: &str, period: Duration) -> Result<bool> {
        self.gate(&format!("{key}:{}", self.message().sender.id), period)
            .await
    }

    pub async fn reply(&self, message: String) -> Result<()> {
        tracing::debug!(reply = message, "replying");
        self.messaging().reply(self.message(), message).await?;
        Ok(())
    }

    pub async fn reply_buffered(&self, message: String) -> Result<()> {
        let state_key = format!("reply_buffered:{}", self.message().sender.id);

        if self.storage().rpush(&state_key, &message).await? != 1 {
            tracing::debug!(message, "adding to existing reply buffer");
            return Ok(());
        }

        tracing::debug!(message, "new reply buffer");

        sleep(Duration::from_millis(100)).await;

        let messages: Vec<String> = {
            let mut tx = self.storage().create_transaction();
            tx.lrange::<_, _, Vec<String>>(&state_key, 0, -1).queue();
            tx.del(&state_key).forget();
            tx.execute().await?
        };

        self.reply(messages.join("; ")).await
    }
}
