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
use tokio::{sync::oneshot::Receiver, time::sleep};

use crate::{
    commands::{
        CommandResult,
        runner::{CommandError, Runner},
    },
    services::messaging::{Message, PermissionLevel},
};

use super::app::{AppContext, InterruptKind, InterruptTicket};

struct MessageState {
    interrupt_ticket: InterruptTicket,
    repeats: AtomicU32,
    message: Message,
}

#[derive(Clone)]
pub struct MessageContext {
    parent: AppContext,
    runner: Runner,
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
    pub fn new(parent: AppContext, runner: Runner, message: Message) -> Self {
        Self {
            state: Arc::new(MessageState {
                interrupt_ticket: parent.interrupt_ticket(&message.sender.id),
                repeats: AtomicU32::new(0),
                message,
            }),
            runner,
            parent,
        }
    }

    pub fn message(&self) -> &Message {
        &self.state.message
    }

    pub fn sender(&self) -> &str {
        &self.state.message.sender.id
    }

    pub fn level(&self) -> PermissionLevel {
        self.state.message.sender.level
    }

    pub fn runner(&self) -> &Runner {
        &self.runner
    }

    pub fn interrupted(&self) -> bool {
        self.state.interrupt_ticket.interrupted()
    }

    pub fn local_interrupt(&self) {
        self.state
            .interrupt_ticket
            .interrupt(InterruptKind::Interrupt);
    }

    pub fn wait_for_interrupt(&self) -> impl Future<Output = InterruptKind> + use<> {
        self.state.interrupt_ticket.wait()
    }

    pub fn interrupt_signal(&self) -> Receiver<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let f = self.wait_for_interrupt();
        tokio::spawn(async move {
            f.await;
            let _ = tx.send(());
        });
        rx
    }

    pub fn inc_repeats(&self) -> u32 {
        self.state.repeats.fetch_add(1, Ordering::Relaxed)
    }

    pub fn reset_repeats(&self) {
        self.state.repeats.store(0, Ordering::Relaxed);
    }

    pub async fn reply(&self, message: String) -> Result<()> {
        tracing::debug!(reply = message, "replying");
        self.messaging().reply(self.message(), message).await?;
        Ok(())
    }

    pub async fn fail(&self, message: impl Into<String>) -> CommandResult {
        let message = message.into();
        self.reply(message.clone()).await?;
        Err(CommandError::PreconditionFail(message))
    }

    pub async fn reply_buffered(&self, message: String) -> Result<()> {
        let state_key = format!("reply_buffered:{}", self.message().sender.id);

        if self.storage_old().rpush(&state_key, &message).await? != 1 {
            tracing::debug!(message, "adding to existing reply buffer");
            return Ok(());
        }

        tracing::debug!(message, "new reply buffer");

        sleep(Duration::from_millis(100)).await;

        let messages: Vec<String> = {
            let mut tx = self.storage_old().create_transaction();
            tx.lrange::<_, _, Vec<String>>(&state_key, 0, -1).queue();
            tx.del(&state_key).forget();
            tx.execute().await?
        };

        self.reply(messages.join("; ")).await
    }
}
