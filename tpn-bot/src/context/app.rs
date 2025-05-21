use std::{
    ops::Deref,
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

use crate::{commands::runner::CommandInterrupt, config::Config, services::Services};

#[derive(Default)]
struct AppState {
    interrupts: Vec<Sender<()>>,
    last_interrupt: Option<Instant>,
}

impl AppContext {
    pub fn last_interrupt(&self) -> Option<Instant> {
        self.inner.state.lock().unwrap().last_interrupt
    }

    pub fn break_holds(&self) {
        for tx in self.inner.state.lock().unwrap().interrupts.drain(..) {
            _ = tx.send(());
        }
    }

    pub fn interrupt_holds(&self) {
        {
            let mut state = self.inner.state.lock().unwrap();
            state.interrupts.clear();
            state.last_interrupt = Some(Instant::now());
        }

        let handle = self.clone();
        tokio::spawn(async move {
            if let Err(error) = handle.storage().publish("interrupt", "1").await {
                tracing::error!(?error, "failed to publish interrupt: {error:?}");
            }
        });
    }

    pub fn interruptible<F>(
        &self,
        f: F,
    ) -> impl Future<Output = Result<(), CommandInterrupt>> + use<F>
    where
        F: Future<Output = ()>,
    {
        let (tx, rx) = oneshot::channel::<()>();
        self.inner.state.lock().unwrap().interrupts.push(tx);
        async move {
            tokio::select! {
                r = rx => r.map_err(|_| CommandInterrupt),
                _ = f => Ok(())
            }
        }
    }
}

struct Inner {
    state: Mutex<AppState>,
    config: Config,
}

#[derive(Clone)]
pub struct AppContext {
    inner: Arc<Inner>,
    services: Services,
}

impl AppContext {
    pub fn new(config: Config, services: Services) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Default::default(),
                config,
            }),
            services,
        }
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
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
        self.messaging().send(message).await?;
        Ok(())
    }

    // eh I couldnt be bothered lol
    pub async fn just(script: &str) -> Result<()> {
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

    pub async fn just_bool(script: &str) -> Result<bool> {
        let res = Command::new("setsid")
            .args(["just", script])
            .env_remove("RUST_LOG")
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?
            .wait_with_output()
            .await?;
        if !res.status.success() {
            bail!(
                "just command failed: {}",
                String::from_utf8_lossy(&res.stderr)
            )
        }
        Ok(res.stdout.trim_ascii() == b"true")
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

    pub async fn reset() -> Result<()> {
        Self::just("reset-restart").await
    }

    pub async fn next_run(&self) -> Result<()> {
        self.interrupt_holds();

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

impl Deref for AppContext {
    type Target = Services;

    fn deref(&self) -> &Self::Target {
        &self.services
    }
}
