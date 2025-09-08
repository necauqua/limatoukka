use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use rustis::commands::PubSubCommands;
use tokio::sync::Notify;

use crate::{
    config::Config,
    services::{
        Injector,
        gates::GateService,
        messaging::{MessagingService, MessagingServiceMock},
        noita::NoitaHandle,
        storage_old::Storage,
    },
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
    caster_id: Option<String>,
    bot_id: Option<String>,
}

#[derive(Clone)]
pub struct AppContext {
    inner: Arc<Inner>,
    messaging: Arc<dyn MessagingService>,
    injector: Injector,
}

impl AppContext {
    pub fn new(injector: Injector) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Default::default(),
                caster_id: None,
                bot_id: None,
            }),
            messaging: injector
                .get_opt::<dyn MessagingService>()
                .unwrap_or_else(|| Arc::new(MessagingServiceMock)),
            injector,
        }
    }

    pub fn with_caster_id(mut self, caster_id: String) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("setting caster id after cloning app context")
            .caster_id = Some(caster_id);
        self
    }

    pub fn with_bot_id(mut self, bot_id: String) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("setting bot id after cloning app context")
            .bot_id = Some(bot_id);
        self
    }

    pub fn config(&self) -> Arc<Config> {
        self.service::<Config>()
    }

    pub fn caster_id(&self) -> Option<&str> {
        self.inner.caster_id.as_deref()
    }

    pub fn bot_id(&self) -> Option<&str> {
        self.inner.bot_id.as_deref()
    }

    #[track_caller]
    pub fn service<T: ?Sized + Send + Sync + 'static>(&self) -> Arc<T> {
        self.injector.get::<T>()
    }

    pub fn service_opt<T: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.injector.get_opt::<T>()
    }

    /// Returns true once (atomically) in the given period - per key.
    pub async fn gate(&self, key: &str, period: Duration) -> Result<bool> {
        self.service::<dyn GateService>()
            .gate(key, "global", period)
            .await
    }

    pub async fn send(&self, message: String) -> Result<()> {
        self.messaging.send(message).await?;
        Ok(())
    }

    pub fn messaging(&self) -> &dyn MessagingService {
        &*self.messaging
    }

    #[track_caller]
    pub fn storage(&self) -> Arc<Storage> {
        self.service::<Storage>()
    }

    #[track_caller]
    pub fn noita(&self) -> Arc<NoitaHandle> {
        self.service::<NoitaHandle>()
    }
}
