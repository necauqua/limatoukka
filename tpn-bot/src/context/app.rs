use std::{
    ops::Deref,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, bail};
use futures::future::pending;
use maud::html;
use rustis::commands::{PubSubCommands, SetCondition, SetExpiration, StringCommands};
use tokio::{
    process::{Child, Command},
    sync::{Notify, oneshot::Receiver},
    time::sleep,
};

use crate::{
    config::Config,
    services::{Injector, Services},
};

#[derive(Default)]
struct AppState {
    interrupts: Vec<Arc<InterruptTicketInner>>,
}

#[derive(Debug, Clone, Copy)]
pub enum InterruptKind {
    Interrupt,
    Break,
}

struct InterruptTicketInner {
    chatter_id: String,
    interrupted: AtomicBool,
    notif_interrupt: Notify,
    notif_break: Notify,
    ctx: AppContext,
}

impl InterruptTicketInner {
    fn interrupt(&self, kind: InterruptKind) {
        match kind {
            InterruptKind::Interrupt => {
                self.interrupted.store(true, Ordering::Relaxed);
                self.notif_interrupt.notify_waiters();
            }
            InterruptKind::Break => {
                self.notif_break.notify_waiters();
            }
        }
    }
}

#[derive(Clone)]
pub struct InterruptTicket {
    inner: Arc<InterruptTicketInner>,
}

impl InterruptTicket {
    pub fn interrupted(&self) -> bool {
        self.inner.interrupted.load(Ordering::Relaxed)
    }

    pub fn interrupt(&self, kind: InterruptKind) {
        self.inner.interrupt(kind);
    }

    pub fn wait(&self) -> impl Future<Output = InterruptKind> + use<> {
        let inner = self.inner.clone();
        async move {
            if inner.interrupted.load(Ordering::Relaxed) {
                return InterruptKind::Interrupt;
            }
            tokio::select! {
                _ = inner.notif_interrupt.notified() => InterruptKind::Interrupt,
                _ = inner.notif_break.notified() => InterruptKind::Break,
            }
        }
    }
}

impl Drop for InterruptTicket {
    fn drop(&mut self) {
        let mut state = self.inner.ctx.inner.state.lock().unwrap();
        if let Some(pos) = state
            .interrupts
            .iter()
            .position(|t| std::ptr::eq(&**t, &*self.inner))
        {
            state.interrupts.swap_remove(pos);
        }
    }
}

impl AppContext {
    pub fn interrupt_ticket(&self, chatter_id: &str) -> InterruptTicket {
        let ticket = Arc::new(InterruptTicketInner {
            chatter_id: chatter_id.to_string(),
            interrupted: AtomicBool::new(false),
            notif_interrupt: Notify::new(),
            notif_break: Notify::new(),
            ctx: self.clone(),
        });
        self.inner
            .state
            .lock()
            .unwrap()
            .interrupts
            .push(ticket.clone());
        InterruptTicket { inner: ticket }
    }

    pub fn interrupt(&self, chatter_id: Option<&str>, kind: InterruptKind) {
        {
            let state = self.inner.state.lock().unwrap();
            if let Some(chatter_id) = chatter_id {
                for t in &state.interrupts {
                    if t.chatter_id == chatter_id {
                        t.interrupt(kind);
                    }
                }
            } else {
                for t in &state.interrupts {
                    t.interrupt(kind);
                }
            }
        }

        let handle = self.clone();
        let chatter_id = chatter_id.map_or_else(|| "<all>".into(), |s| s.to_owned());
        tokio::spawn(async move {
            if let Err(error) = handle.storage().publish("interrupt", chatter_id).await {
                tracing::error!(?error, "failed to publish interrupt: {error:?}");
            }
        });
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
    injector: Injector,
}

impl AppContext {
    pub fn new(config: Config, services: Services, injector: Injector) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Default::default(),
                config,
            }),
            services,
            injector,
        }
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn service<T: ?Sized + Send + Sync + 'static>(&self) -> Arc<T> {
        self.injector.get::<T>()
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
    pub async fn just_detached(script: &str, extra_args: &[&str]) -> Result<()> {
        // we just send it and *dont* wait for like noita.exe to finish
        let _res = Command::new("setsid")
            .args(["just", script])
            .args(extra_args)
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

    pub fn just(script: &str, extra_args: &[&str]) -> Result<Process> {
        Ok(Process {
            child: Command::new("setsid")
                .args(["just", script])
                .args(extra_args)
                .env_remove("RUST_LOG")
                .stderr(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?,
        })
    }

    async fn get_gamemode(&self) -> Result<&'static str> {
        let nightmare: Option<String> = self.storage().get("flags:nightmare").await?;
        Ok(match nightmare {
            Some(_) => "2",
            None => "0",
        })
    }

    async fn get_set_seed(&self) -> Result<String> {
        let seed: Option<String> = self.storage().get("set-seed").await?;
        Ok(seed.unwrap_or_default())
    }

    pub async fn restart(&self) -> Result<()> {
        Self::just_detached(
            "restart",
            &[self.get_gamemode().await?, &self.get_set_seed().await?],
        )
        .await
    }

    pub async fn reset(&self) -> Result<()> {
        Self::just_detached(
            "reset-restart",
            &[self.get_gamemode().await?, &self.get_set_seed().await?],
        )
        .await
    }

    pub async fn next_run(&self) -> Result<()> {
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

        self.interrupt(None, InterruptKind::Interrupt);

        self.restart().await?;
        Ok(())
    }
}

impl Deref for AppContext {
    type Target = Services;

    fn deref(&self) -> &Self::Target {
        &self.services
    }
}

pub struct Process {
    child: Child,
}

impl Process {
    pub async fn get(self) -> Result<Result<String, String>> {
        let output = self.child.wait_with_output().await?;
        if output.status.success() {
            Ok(Ok(String::from_utf8(output.stdout)?))
        } else {
            Ok(Err(String::from_utf8(output.stderr)?))
        }
    }

    pub async fn check(self) -> Result<String> {
        match self.get().await? {
            Ok(success) => Ok(success),
            Err(err) => bail!("just command failed: {err}"),
        }
    }

    async fn do_wait(&mut self) {
        if let Err(e) = self.child.wait().await {
            tracing::error!(?e, "child process errored");
        }
    }

    pub async fn wait(&mut self, stop: Option<Receiver<()>>) -> bool {
        let stop = async {
            if let Some(stop) = stop {
                _ = stop.await;
            } else {
                pending::<()>().await;
            }
        };
        tokio::select! {
            _ = self.do_wait() => true,
            _ = stop => {
                // ugh meh
                let pid = self.child.id().unwrap();
                unsafe { libc::killpg(pid as _, libc::SIGTERM) };
                self.do_wait().await;
                false
            },
        }
    }
}
