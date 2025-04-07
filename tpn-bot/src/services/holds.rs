use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::{Mutex, oneshot::Sender};

#[derive(Default)]
struct HoldsInner {
    next_idx: usize,
    current: HashMap<usize, Sender<()>>,
}

impl HoldsInner {
    fn cancel_all(&mut self) {
        for (_, tx) in self.current.drain() {
            _ = tx.send(());
        }
    }

    fn sleep(&mut self, duration: Duration) -> impl Future<Output = ()> + use<> {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let idx = self.next_idx;
        self.next_idx += 1;
        self.current.insert(idx, tx);
        async move {
            tokio::select! { biased;
                _ = rx => {}
                _ = tokio::time::sleep(duration) => {}
            }
        }
    }
}

impl Drop for HoldsInner {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

#[derive(Clone, Default)]
pub struct Holds {
    inner: Arc<Mutex<HoldsInner>>,
}

impl Holds {
    pub async fn cancel_all(&self) {
        self.inner.lock().await.cancel_all();
    }

    pub async fn sleep(&self, duration: Duration) {
        let fut = { self.inner.lock().await.sleep(duration) };
        fut.await;
    }
}
