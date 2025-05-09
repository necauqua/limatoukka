use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use maud::html;
use rustis::commands::{GenericCommands, SetCondition, SetExpiration, StringCommands};
use tokio::{
    process::Command,
    sync::oneshot::{self, Sender},
    task::JoinHandle,
    time::sleep,
};
use tracing::Instrument;

use crate::{
    commands::runner::CommandInterrupt,
    config::Config,
    services::{
        messaging::MessagingClient, noita::NoitaHandle, status_wall::StatusWall, storage::Storage,
        twitch::Twitch, xdo::XDoClient,
    },
};

struct Inner {
    config: Config,
    messaging: MessagingClient,
    storage: Storage,
    xdo: XDoClient,
    noita: NoitaHandle,
    status_wall: StatusWall,
    twitch: Twitch,

    state: AppState,
}

#[derive(Default)]
struct AppStateInner {
    interrupts: Vec<Sender<()>>,
    last_interrupt: Option<Instant>,
}

#[derive(Default)]
pub struct AppState {
    inner: Mutex<AppStateInner>,
}

impl AppStateInner {
    fn break_holds(&mut self) {
        for tx in self.interrupts.drain(..) {
            _ = tx.send(());
        }
    }

    fn interrupt_holds(&mut self) {
        self.interrupts.clear();
        self.last_interrupt = Some(Instant::now());
    }

    fn interruptible<F>(
        &mut self,
        f: F,
    ) -> impl Future<Output = Result<(), CommandInterrupt>> + use<F>
    where
        F: Future<Output = ()>,
    {
        let skip = self
            .last_interrupt
            .is_some_and(|i| i.elapsed() < Duration::from_millis(50));
        let (tx, rx) = oneshot::channel::<()>();
        self.interrupts.push(tx);
        async move {
            if skip {
                return Err(CommandInterrupt);
            }
            tokio::select! {
                r = rx => r.map_err(|_| CommandInterrupt),
                _ = f => Ok(())
            }
        }
    }
}

#[derive(Clone)]
pub struct AppContext {
    inner: Arc<Inner>,
}

impl AppContext {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        messaging: MessagingClient,
        config: Config,
        storage: Storage,
        xdo: XDoClient,
        noita: NoitaHandle,
        status_wall: StatusWall,
        twitch: Twitch,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                messaging,
                config,
                storage,
                xdo,
                noita,
                status_wall,
                twitch,
                state: Default::default(),
            }),
        }
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn messaging(&self) -> &MessagingClient {
        &self.inner.messaging
    }

    pub fn storage(&self) -> &Storage {
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

    pub fn twitch(&self) -> &Twitch {
        &self.inner.twitch
    }

    pub fn state(&self) -> &AppState {
        &self.inner.state
    }

    pub fn init(&self) {
        let handle = self.clone();
        tokio::spawn(
            handle
                .status_wall()
                .start(&handle.config().browser_source_bind),
        );

        tokio::spawn(async move {
            // fix any stuck holds
            _ = tokio::join!(
                handle.xdo().keyup("w"),
                handle.xdo().keyup("a"),
                handle.xdo().keyup("s"),
                handle.xdo().keyup("d"),
                handle.xdo().mouseup(1),
            );
            NoitaHandle::poll_state_updates(handle).await;
        });
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

    pub async fn break_holds(&self) {
        self.inner.state.inner.lock().unwrap().break_holds();
    }

    pub async fn interrupt_holds(&self) {
        self.inner.state.inner.lock().unwrap().interrupt_holds();
    }

    pub async fn interruptible<F>(&self, f: F) -> Result<(), CommandInterrupt>
    where
        F: Future<Output = ()>,
    {
        let fut = { self.inner.state.inner.lock().unwrap().interruptible(f) };
        fut.await
    }

    // eh I couldnt be bothered lol
    async fn just(script: &str) -> Result<()> {
        let res = Command::new("setsid")
            .args(["just", script])
            .env_remove("RUST_LOG")
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?
            .wait_with_output()
            .await?;
        if !res.status.success() {
            bail!(
                "just command failed: {}",
                String::from_utf8_lossy(&res.stderr)
            )
        }
        Ok(())
    }

    pub async fn cringe_scp_large_reply(msg: &str) -> Result<()> {
        let res = Command::new("setsid")
            .args(["just", "cringe-scp-large-reply", msg])
            .env_remove("RUST_LOG")
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?
            .wait_with_output()
            .await?;
        if !res.status.success() {
            bail!(
                "just command failed: {}",
                String::from_utf8_lossy(&res.stderr)
            )
        }
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
        self.interrupt_holds().await;

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

        self.storage()
            .del(["balance:blesses", "balance:curses"])
            .await?;

        Self::restart().await?;
        Ok(())
    }
}
