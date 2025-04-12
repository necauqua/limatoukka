use std::{
    fmt::{self, Display, Write},
    ops::Deref,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use maud::html;
use rustis::{
    client::BatchPreparedCommand,
    commands::{GenericCommands, ListCommands, SetCondition, SetExpiration, StringCommands},
};
use tokio::{process::Command, task::JoinHandle, time::sleep};
use tracing::Instrument;

use crate::{
    config::Config,
    services::{
        holds::HoldState,
        messaging::{Message, MessagingClient},
        noita::NoitaHandle,
        status_wall::StatusWall,
        xdo::XDoClient,
    },
};

use super::{CommandRegistration, parsing::CommandType};

pub type Valkey = rustis::client::Client;

#[derive(Clone)]
pub struct AppContext {
    pub config: Arc<Config>,
    pub messaging: MessagingClient,
    pub storage: Arc<Valkey>,
    pub xdo: XDoClient,
    pub noita: NoitaHandle,
    pub status_wall: StatusWall,
    pub holds: HoldState,
}

#[derive(Clone)]
pub struct MessageContext {
    app_ctx: AppContext,
    repeats: Arc<AtomicU32>,
    pub message: Arc<Message>,
}

#[derive(Debug, Clone)]
pub struct CommandToken {
    pub name: Arc<str>,
    pub tpe: CommandType,
    pub group: usize,
    pub idx: usize,
}

#[derive(Debug, Clone)]
pub struct CommandDescriptor {
    pub registration: &'static CommandRegistration,
    pub token: CommandToken,
}

impl Display for CommandDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.token.tpe.write_command(f, self.registration.name)?;
        if f.alternate() {
            write!(f, "({},{})", self.token.group, self.token.idx)?;
        }
        Ok(())
    }
}

impl Display for CommandToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.tpe.write_command(f, &self.name)?;
        if f.alternate() {
            write!(f, "({},{})", self.group, self.idx)?;
        }
        Ok(())
    }
}

#[derive(Default, Clone, Copy)]
pub struct Nesting {
    pub depth: u32,
    pub macro_depth: u32,
}

impl Display for Nesting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.macro_depth != 0 {
            f.write_char('@')?;
            if self.macro_depth != 1 {
                write!(f, "{}", self.macro_depth)?;
            }
        }
        for _ in 0..self.depth {
            f.write_char('|')?;
        }
        Ok(())
    }
}

impl Nesting {
    pub fn nest(&self) -> Self {
        Self {
            depth: self.depth + 1,
            macro_depth: self.macro_depth,
        }
    }

    pub fn nest_macro(&self) -> Self {
        Self {
            depth: self.depth + 1,
            macro_depth: self.macro_depth + 1,
        }
    }
}

#[derive(Clone)]
pub struct CommandContext {
    msg_ctx: MessageContext,
    pub command: CommandDescriptor,
    pub nesting: Nesting,
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
    pub fn schedule<F>(
        &self,
        timeout: Duration,
        f: impl FnOnce(Self) -> F + Send + 'static,
    ) -> JoinHandle<()>
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
        )
    }

    // eh I couldnt be bothered lol
    async fn just(script: &str) -> Result<()> {
        Command::new("setsid")
            .args(["just", script])
            .env_remove("RUST_LOG")
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        Ok(())
    }

    pub async fn restart() -> Result<()> {
        Self::just("restart").await
    }

    pub async fn fix_obs_capture() -> Result<()> {
        Self::just("obs-reset-display").await
    }

    pub async fn fix_obs_sound() -> Result<()> {
        Self::just("sound-setup").await
    }

    pub async fn reset() -> Result<()> {
        Self::just("reset-restart").await
    }

    pub async fn next_run(&self) -> Result<()> {
        self.holds.send_interrupt().await;

        sleep(Duration::from_millis(500)).await;

        self.xdo.key("Enter").await?;

        let entry = self.status_wall.allocate().await;
        for i in (1..=10).rev() {
            let status = html! { span style="color:orange" { "Starting new game in " (i) } }.0;
            self.status_wall.set(entry, status).await;
            sleep(Duration::from_secs(1)).await;
        }
        self.status_wall.pop(entry).await;
        Self::restart().await?;
        Ok(())
    }
}

impl MessageContext {
    pub fn new(state: AppContext, message: Arc<Message>) -> Self {
        Self {
            app_ctx: state,
            repeats: Default::default(),
            message,
        }
    }

    pub fn inc_repeats(&self) -> u32 {
        self.repeats.fetch_add(1, Ordering::Relaxed)
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
    pub fn new(msg_ctx: MessageContext, command: CommandDescriptor, nesting: Nesting) -> Self {
        Self {
            msg_ctx,
            command,
            nesting,
        }
    }
}
