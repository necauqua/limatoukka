//! A logind "delay" inhibitor lock: logind waits (at most `InhibitDelayMaxSec`,
//! 5 s by default) before it shuts the system down, so that the bot can
//! deinitialize (e.g. pause the rewards) while the network and Valkey still
//! run.

use anyhow::Result;
use futures::StreamExt;
use zbus::{Connection, OwnedFd, proxy};

#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Manager {
    fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> zbus::Result<OwnedFd>;

    #[zbus(signal)]
    fn prepare_for_shutdown(&self, start: bool) -> zbus::Result<()>;
}

pub struct ShutdownInhibitor {
    /// The lock is held while this fd is open.
    lock: Option<OwnedFd>,
    signals: PrepareForShutdownStream,
}

impl ShutdownInhibitor {
    pub async fn new() -> Result<Self> {
        let connection = Connection::system().await?;
        let manager = ManagerProxy::new(&connection).await?;
        // subscribe first, so that no shutdown after the lock is missed
        let signals = manager.receive_prepare_for_shutdown().await?;
        let lock = manager
            .inhibit("shutdown", "limatoukka", "Deinitialize the bot", "delay")
            .await?;
        Ok(Self {
            lock: Some(lock),
            signals,
        })
    }

    /// Completes when the system starts to shut down.
    /// Never completes when the connection to the system bus is lost.
    pub async fn wait(&mut self) {
        while let Some(signal) = self.signals.next().await {
            match signal.args() {
                Ok(args) if args.start => return,
                Ok(_) => {}
                Err(error) => tracing::warn!(?error, "bad PrepareForShutdown signal"),
            }
        }
        tracing::warn!("lost the logind PrepareForShutdown signals");
        std::future::pending().await
    }

    /// Lets the shutdown continue.
    pub fn release(&mut self) {
        self.lock.take();
    }
}

/// [`ShutdownInhibitor::wait`], or never when there is no inhibitor.
pub async fn shutdown_requested(inhibitor: &mut Option<ShutdownInhibitor>) {
    match inhibitor {
        Some(inhibitor) => inhibitor.wait().await,
        None => std::future::pending().await,
    }
}
