use std::{
    fmt::{self, Display},
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
        twitch::Twitch,
        xdo::XDoClient,
    },
};

use super::{CommandRegistration, parsing::CommandType};

pub type Valkey = rustis::client::Client;

struct AppContextInner {
    messaging: MessagingClient,
    config: Config,
    storage: Valkey,
    xdo: XDoClient,
    noita: NoitaHandle,
    status_wall: StatusWall,
    holds: HoldState,
    twitch: Twitch,
}

#[derive(Clone)]
pub struct AppContext {
    inner: Arc<AppContextInner>,
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

#[derive(Clone)]
pub struct EvalContext {
    msg_ctx: MessageContext,
    pub owner: Arc<str>,
    pub depth: u32,
    pub macro_depth: u32,
    pub in_global_macro: bool,
}

impl EvalContext {
    pub fn new(msg_ctx: MessageContext) -> Self {
        Self {
            owner: (&*msg_ctx.message.sender.id).into(),
            msg_ctx,
            depth: 0,
            macro_depth: 0,
            in_global_macro: false,
        }
    }

    pub fn nesting_str(&self) -> String {
        let mut s = String::with_capacity(self.depth as _);
        for _ in 0..self.depth {
            s.push('|');
        }
        s
    }

    pub fn nest(&self) -> Self {
        Self {
            msg_ctx: self.msg_ctx.clone(),
            owner: self.owner.clone(),
            depth: self.depth + 1,
            macro_depth: self.macro_depth,
            in_global_macro: self.in_global_macro,
        }
    }

    pub fn nest_macro(&self, owner: &str, is_global: bool) -> Self {
        Self {
            msg_ctx: self.msg_ctx.clone(),
            owner: owner.into(),
            depth: self.depth + 1,
            macro_depth: self.macro_depth + 1,
            in_global_macro: self.in_global_macro || is_global,
        }
    }
}

#[derive(Clone)]
pub struct CommandContext {
    eval_ctx: EvalContext,
    pub command: CommandDescriptor,
}

// deref hack lol
impl Deref for MessageContext {
    type Target = AppContext;

    fn deref(&self) -> &Self::Target {
        &self.app_ctx
    }
}

impl Deref for EvalContext {
    type Target = MessageContext;

    fn deref(&self) -> &Self::Target {
        &self.msg_ctx
    }
}

impl Deref for CommandContext {
    type Target = EvalContext;

    fn deref(&self) -> &Self::Target {
        &self.eval_ctx
    }
}

impl AppContext {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        messaging: MessagingClient,
        config: Config,
        storage: Valkey,
        xdo: XDoClient,
        noita: NoitaHandle,
        status_wall: StatusWall,
        holds: HoldState,
        twitch: Twitch,
    ) -> Self {
        Self {
            inner: Arc::new(AppContextInner {
                messaging,
                config,
                storage,
                xdo,
                noita,
                status_wall,
                holds,
                twitch,
            }),
        }
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn storage(&self) -> &Valkey {
        &self.inner.storage
    }

    pub fn xdo(&self) -> &XDoClient {
        &self.inner.xdo
    }

    pub fn noita(&self) -> &NoitaHandle {
        &self.inner.noita
    }

    pub fn status_wall(&self) -> &StatusWall {
        &self.inner.status_wall
    }

    pub fn holds(&self) -> &HoldState {
        &self.inner.holds
    }

    pub fn twitch(&self) -> &Twitch {
        &self.inner.twitch
    }

    /// Returns true once (atomically) in the given period - per key.
    pub async fn gate(&self, key: &str, period: Duration) -> Result<bool> {
        let gate: Option<String> = self
            .storage()
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
        self.inner.messaging.send(message).await?;
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

    pub fn reset(&self) -> impl Future<Output = Result<()>> + use<> {
        self.noita().reset_inventory();
        Self::just("reset-restart")
    }

    pub async fn next_run(&self) -> Result<()> {
        self.holds().send_interrupt().await;

        sleep(Duration::from_millis(500)).await;

        self.xdo().key("Enter").await?;

        let no_restarts: Option<String> = self.storage().get("flags:no-restarts").await?;
        if no_restarts.is_some() {
            return Ok(());
        }

        let entry = self.status_wall().allocate().await;
        for i in (1..=10).rev() {
            entry
                .set_top(html! { span style="color:orange" { "Starting new game in " (i) } })
                .await;
            sleep(Duration::from_secs(1)).await;
        }
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
        self.inner.messaging.reply(&self.message, message).await?;
        Ok(())
    }

    pub async fn reply_buffered(&self, message: String) -> Result<()> {
        let state_key = format!("reply_buffered:{}", self.message.sender.id);

        if self.storage().rpush(&state_key, &message).await? != 1 {
            tracing::debug!(message, "adding to existing reply buffer");
            return Ok(());
        }

        tracing::debug!(message, "new reply buffer");
        let ctx = self.clone();
        self.schedule(Duration::from_millis(100), move |_| async move {
            let mut tx = ctx.storage().create_transaction();
            tx.lrange::<_, _, Vec<String>>(&state_key, 0, -1).queue();
            tx.del(&state_key).forget();

            let messages: Vec<String> = tx.execute().await?;
            ctx.reply(messages.join("; ")).await
        });

        Ok(())
    }
}

impl CommandContext {
    pub fn new(eval_ctx: EvalContext, command: CommandDescriptor) -> Self {
        Self { eval_ctx, command }
    }
}
