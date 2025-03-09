use std::{
    fmt::{self, Display},
    ops::Deref,
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::Result;
use rustis::{
    client::BatchPreparedCommand,
    commands::{GenericCommands, ListCommands, SetCondition, SetExpiration, StringCommands},
};
use tokio::{process::Command, time::sleep};
use tracing::Instrument;

use crate::{
    config::Config,
    services::{
        messaging::{Message, MessagingClient},
        noita::NoitaHandle,
        status_wall::StatusWall,
        xdo::XDoClient,
    },
};

use super::parsing::CommandType;

pub type Valkey = rustis::client::Client;

#[derive(Clone)]
pub struct AppContext {
    pub config: Arc<Config>,
    pub messaging: MessagingClient,
    pub storage: Arc<Valkey>,
    pub xdo: XDoClient,
    pub noita: NoitaHandle,
    pub status_wall: StatusWall,
}

#[derive(Clone)]
pub struct MessageContext {
    app_ctx: AppContext,
    pub message: Arc<Message>,
}

#[derive(Debug, Clone)]
pub struct CommandDescriptor {
    pub name: String,
    pub tpe: CommandType,
    pub group: usize,
    pub idx: usize,
}

impl Display for CommandDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.tpe {
            CommandType::Uwu => write!(f, "{}~", self.name),
            CommandType::Crusade => write!(f, "+{}", self.name),
        }
    }
}

#[derive(Clone)]
pub struct CommandContext {
    msg_ctx: MessageContext,
    pub command: CommandDescriptor,
}

// deref hack lol
impl Deref for MessageContext {
    type Target = AppContext;

    fn deref(&self) -> &Self::Target {
        &self.app_ctx
    }
}

impl Deref for CommandContext {
    type Target = MessageContext;

    fn deref(&self) -> &Self::Target {
        &self.msg_ctx
    }
}

impl AppContext {
    /// Returns true once (atomically) in the given period - per key.
    pub async fn gate(&self, key: &str, period: Duration) -> Result<bool> {
        let gate: Option<String> = self
            .storage
            .set_get_with_options(
                format!("gate:{key}"),
                "1",
                SetCondition::NX,
                SetExpiration::Px(period.as_millis() as u64),
                false,
            )
            .await?;
        if gate.is_some() {
            tracing::trace!(?period, key, "gated");
            Ok(false)
        } else {
            Ok(true)
        }
    }

    pub async fn send(&self, message: String) -> Result<()> {
        tracing::debug!(text = message, "sending");
        self.messaging.send(message).await?;
        Ok(())
    }

    pub async fn send_buffered(
        &self,
        key: &str,
        period: Duration,
        message: String,
        separator: &'static str,
    ) -> Result<()> {
        let state_key = format!("send_buffered:{key}");

        if self.storage.rpush(&state_key, message).await? == 1 {
            tracing::debug!(key, "new buffer");
            self.schedule(period, move |ctx| async move {
                let mut tx = ctx.storage.create_transaction();
                tx.lrange::<_, _, Vec<String>>(&state_key, 0, -1).queue();
                tx.del(&state_key).forget();
                let messages: Vec<String> = tx.execute::<Vec<String>>().await?;

                ctx.send(messages.join(separator)).await
            });
        } else {
            tracing::debug!(key, "adding to existing buffer");
        }

        Ok(())
    }

    /// Schedules a future to run after the given timeout.
    pub fn schedule<F>(&self, timeout: Duration, f: impl FnOnce(Self) -> F + Send + 'static)
    where
        F: Future<Output = Result<()>> + Send + 'static,
    {
        let ctx = self.clone();
        tokio::spawn(
            async move {
                sleep(timeout).await;
                if let Err(error) = f(ctx).await {
                    tracing::error!(?error);
                }
            }
            .instrument(tracing::debug_span!("set_timeout", ?timeout)),
        );
    }

    pub async fn next_run(&self) -> Result<()> {
        sleep(Duration::from_millis(500)).await;

        self.xdo.key("Enter").await?;

        let entry = self.status_wall.push_top(String::new()).await;
        for i in (1..=10).rev() {
            self.status_wall
                .set(
                    entry,
                    format!("<span style=\"color:orange\">Starting new game in {i}</span>"),
                )
                .await;
            sleep(Duration::from_secs(1)).await;
        }
        self.status_wall.pop(entry).await;

        // eh I couldnt be bothered lol
        Command::new("setsid")
            .args(["just", "stop", "run"])
            .env_remove("RUST_LOG")
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;

        Ok(())
    }
}

impl MessageContext {
    pub fn new(state: AppContext, message: Arc<Message>) -> Self {
        Self {
            app_ctx: state,
            message,
        }
    }

    pub fn sender_key(&self, key: &str) -> String {
        format!("{key}:{}", self.message.sender.id)
    }

    /// Returns true once (atomically) in the given period - per key and per sender.
    pub async fn sender_gate(&self, key: &str, period: Duration) -> Result<bool> {
        self.gate(&self.sender_key(key), period).await
    }

    pub async fn reply(&self, message: String) -> Result<()> {
        tracing::debug!(reply = message, "replying");
        self.messaging.reply(&self.message.id, message).await?;
        Ok(())
    }
}

impl CommandContext {
    pub fn new(msg_ctx: MessageContext, desc: CommandDescriptor) -> Self {
        Self {
            msg_ctx,
            command: desc,
        }
    }

    /// Returns true once (atomically) in the given period - per key and per command.
    pub async fn command_gate(&self, period: Duration) -> Result<bool> {
        self.gate(&self.command.name, period).await
    }

    /// Returns true once (atomically) in the given period - per sender and per command.
    pub async fn sender_command_gate(&self, period: Duration) -> Result<bool> {
        self.sender_gate(&self.command.name, period).await
    }
}
