use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use tokio::sync::{Mutex, oneshot::Sender};

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
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let idx = self.next_idx;
        self.next_idx += 1;
        self.current.insert(idx, tx);
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

    pub async fn interruptible<F>(&self, f: F) -> Result<(), CommandInterrupt>
    where
        F: Future<Output = ()>,
    {
        let fut = { self.inner.lock().await.interruptible(f) };
        fut.await
    }
}
