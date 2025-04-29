use std::{process::Stdio, sync::Arc, time::Duration};

use anyhow::Result;
use maud::html;
use rustis::commands::{SetCondition, SetExpiration, StringCommands};
use tokio::{process::Command, task::JoinHandle, time::sleep};
use tracing::Instrument;

use crate::{
    config::Config,
    services::{
        holds::HoldState, messaging::MessagingClient, noita::NoitaHandle, status_wall::StatusWall,
        storage::Storage, twitch::Twitch, xdo::XDoClient,
    },
};

struct Inner {
    config: Config,
    messaging: MessagingClient,
    storage: Storage,
    xdo: XDoClient,
    noita: NoitaHandle,
    status_wall: StatusWall,
    holds: HoldState,
    twitch: Twitch,
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
        holds: HoldState,
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
                holds,
                twitch,
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

    pub fn holds(&self) -> &HoldState {
        &self.inner.holds
    }

    pub fn twitch(&self) -> &Twitch {
        &self.inner.twitch
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
            handle.noita().poll_state_updates().await;
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
