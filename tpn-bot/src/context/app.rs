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
use maud::html;
use rustis::commands::{
    GenericCommands, PubSubCommands, SetCondition, SetExpiration, StringCommands,
};
use tokio::{
    process::{Child, Command},
    sync::Notify,
    time::sleep,
};

use crate::{config::Config, services::Services};

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

    pub fn wait(&self) -> impl Future<Output = InterruptKind> + use<> {
        let inner = self.inner.clone();
        async move {
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
    pub async fn just(script: &str, extra_args: &[&str]) -> Result<()> {
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

    pub async fn cringe_aws_tts_through_shell(msg: &str) -> Result<Child> {
        Ok(Command::new("setsid")
            .args(["just", "cringe-aws-tts-through-shell", msg])
            .env_remove("RUST_LOG")
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?)
    }

    async fn get_gamemode(&self) -> Result<&'static str> {
        let nightmare: Option<String> = self.storage().get("flags:nightmare").await?;
        Ok(match nightmare {
            Some(_) => "2",
            None => "0",
        })
    }

    pub async fn restart(&self) -> Result<()> {
        Self::just("restart", &[self.get_gamemode().await?]).await
    }

    pub async fn reset(&self) -> Result<()> {
        Self::just("reset-restart", &[self.get_gamemode().await?]).await
    }

    pub async fn next_run(&self) -> Result<()> {
        self.storage()
            .del(["balance:blesses", "balance:curses", "best-inventory"])
            .await?;

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
