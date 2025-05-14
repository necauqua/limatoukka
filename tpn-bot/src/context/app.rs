use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use maud::html;
use rustis::commands::{
    GenericCommands, PubSubCommands, SetCondition, SetExpiration, StringCommands,
};
use tokio::{
    process::Command,
    sync::oneshot::{self, Sender},
    time::sleep,
};

use crate::{
    commands::runner::CommandInterrupt,
    config::Config,
    services::{
        messaging::MessagingClient,
        noita::NoitaHandle,
        status_wall::StatusWall,
        storage::{Storage, StorageRef},
        twitch::Twitch,
        xdo::XDoClient,
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

impl AppState {
    pub fn last_interrupt(&self) -> Option<Instant> {
        self.inner.lock().unwrap().last_interrupt
    }
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
        let (tx, rx) = oneshot::channel::<()>();
        self.interrupts.push(tx);
        async move {
            tokio::select! {
                r = rx => r.map_err(|_| CommandInterrupt),
                _ = f => Ok(())
            }
        }
    }
}

#[derive(Clone)]
pub struct AppContext {
    inner: Option<Arc<Inner>>,
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
            inner: Some(Arc::new(Inner {
                messaging,
                config,
                storage,
                xdo,
                noita,
                status_wall,
                twitch,
                state: Default::default(),
            })),
        }
    }

    pub fn mock() -> Self {
        Self { inner: None }
    }

    pub fn config(&self) -> &Config {
        &self.inner.as_deref().unwrap().config
    }

    pub fn messaging(&self) -> &MessagingClient {
        &self.inner.as_deref().unwrap().messaging
    }

    pub fn storage(&self) -> StorageRef {
        StorageRef::new(&self.inner.as_deref().unwrap().storage)
    }

    pub fn xdo(&self) -> &XDoClient {
        &self.inner.as_deref().unwrap().xdo
    }

    pub fn noita(&self) -> &NoitaHandle {
        &self.inner.as_deref().unwrap().noita
    }

    pub fn status_wall(&self) -> &StatusWall {
        &self.inner.as_deref().unwrap().status_wall
    }

    pub fn twitch(&self) -> &Twitch {
        &self.inner.as_deref().unwrap().twitch
    }

    pub fn state(&self) -> &AppState {
        &self.inner.as_deref().unwrap().state
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
        self.inner
            .as_deref()
            .unwrap()
            .messaging
            .send(message)
            .await?;
        Ok(())
    }

    pub async fn break_holds(&self) {
        self.state().inner.lock().unwrap().break_holds();
    }

    pub async fn interrupt_holds(&self) {
        let handle = self.clone();

        tokio::spawn(async move {
            if let Err(error) = handle.storage().publish("interrupt", "1").await {
                tracing::error!(?error, "failed to publish interrupt: {error:?}");
            }
        });

        self.state().inner.lock().unwrap().interrupt_holds();
    }

    pub async fn interruptible(&self, f: impl Future<Output = ()>) -> Result<(), CommandInterrupt> {
        let fut = { self.state().inner.lock().unwrap().interruptible(f) };
        fut.await
    }

    // eh I couldnt be bothered lol
    async fn just(script: &str) -> Result<()> {
        // we just send it and *dont* wait for like noita.exe to finish
        let _res = Command::new("setsid")
            .args(["just", script])
            .env_remove("RUST_LOG")
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        //     .wait_with_output()
        //     .await?;
        // if !res.status.success() {
        //     bail!(
        //         "just command failed: {}",
        //         String::from_utf8_lossy(&res.stderr)
        //     )
        // }
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

    pub async fn reset() -> Result<()> {
        Self::just("reset-restart").await
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
            .del(["balance:blesses", "balance:curses", "best-inventory"])
            .await?;

        Self::restart().await?;
        Ok(())
    }
}
