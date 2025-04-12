use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use tokio::{
    sync::{Mutex, oneshot::Sender},
    time::timeout,
};

use crate::commands::runner::CommandInterrupt;

#[derive(Default)]
struct Inner {
    next_idx: usize,
    current: HashMap<usize, Sender<()>>,
    last_interrupt: Option<Instant>,
}

impl Inner {
    fn send_break(&mut self) {
        for (_, tx) in self.current.drain() {
            _ = tx.send(());
        }
    }

    fn send_interrupt(&mut self) {
        self.current.clear();
        self.last_interrupt = Some(Instant::now());
    }

    fn sleep(
        &mut self,
        duration: Duration,
    ) -> impl Future<Output = Result<(), CommandInterrupt>> + use<> {
        let skip = self
            .last_interrupt
            .is_some_and(|i| i.elapsed() < Duration::from_millis(50));
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let idx = self.next_idx;
        self.next_idx += 1;
        self.current.insert(idx, tx);
        async move {
            if skip {
                return Err(CommandInterrupt);
            }
            match timeout(duration, rx).await {
                Ok(Ok(_)) | Err(_) => Ok(()),
                Ok(Err(_)) => Err(CommandInterrupt),
            }
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.send_interrupt();
    }
}

#[derive(Clone, Default)]
pub struct HoldState {
    inner: Arc<Mutex<Inner>>,
}

impl HoldState {
    pub async fn send_break(&self) {
        self.inner.lock().await.send_break();
    }

    pub async fn send_interrupt(&self) {
        self.inner.lock().await.send_interrupt();
    }

    pub async fn sleep(&self, duration: Duration) -> Result<(), CommandInterrupt> {
        let fut = { self.inner.lock().await.sleep(duration) };
        fut.await
    }
}
