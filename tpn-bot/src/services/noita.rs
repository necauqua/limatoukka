use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result};
use noita_engine_reader::{
    Noita, Seed,
    discovery::KnownBuild,
    memory::{PadBool, RawPtr},
};
use tokio::sync::{Mutex, Notify, futures::Notified};
use tracing::{Instrument, Span};

#[derive(Default, Clone)]
pub struct NoitaHandle {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    noita: Mutex<Option<Noita>>,
    seed: Mutex<Option<Seed>>,
    inventory_open: AtomicBool,
    player_dead: AtomicBool,
    on_inventory_open: Notify,
    on_inventory_close: Notify,
    on_player_death: Notify,
}

fn is_dead(noita: &Noita) -> Result<bool> {
    // -> CONFIG_PLAYER_STATS.stats.dead
    Ok(RawPtr::of(0x01208784)
        .read::<PadBool<3>>(noita.proc())?
        .get()
        .as_bool())
}

fn is_inventory_open(noita: &Noita) -> Result<bool> {
    // -> IS_INVENTORY_OPEN (from GameIsInventoryOpen lua fn)
    Ok(RawPtr::of(0x01222510)
        .read::<PadBool<3>>(noita.proc())?
        .get()
        .as_bool())
}

fn get_seed(noita: &Noita) -> Result<Option<Seed>> {
    Ok(noita.read_seed()?)
}

async fn find_noita() -> Result<Option<Noita>> {
    // actually does take tens of milliseconds, so we offload it
    Ok(tokio::task::spawn_blocking(|| Noita::lookup(KnownBuild::last().map())).await??)
}

impl NoitaHandle {
    pub fn new() -> Self {
        let handle = Self::default();
        tokio::spawn(handle.clone().start_state_polling().in_current_span());
        handle
    }

    pub fn is_inventory_open(&self) -> bool {
        self.inner.inventory_open.load(Ordering::Relaxed)
    }

    pub fn is_player_dead(&self) -> bool {
        self.inner.player_dead.load(Ordering::Relaxed)
    }

    pub async fn get_seed(&self) -> Option<Seed> {
        self.inner.seed.lock().await.clone()
    }

    pub fn wait_for_inventory_open(&self) -> Notified {
        self.inner.on_inventory_open.notified()
    }

    pub fn wait_for_inventory_close(&self) -> Notified {
        self.inner.on_inventory_close.notified()
    }

    pub fn wait_for_player_death(&self) -> Notified {
        self.inner.on_player_death.notified()
    }

    async fn start_state_polling(self) {
        let mut prev_dead = None;
        let mut prev_inventory = None;
        let mut prev_seed = None;

        loop {
            let state = self
                .with(|n| Ok((is_dead(n)?, is_inventory_open(n)?, get_seed(n)?)))
                .await
                .ok();

            let dead = state.map(|(d, _, _)| d);
            if dead != prev_dead {
                let dead_bool = dead.unwrap_or_default();
                self.inner.player_dead.store(dead_bool, Ordering::Relaxed);
                if dead_bool {
                    tracing::debug!("died");
                    self.inner.on_player_death.notify_waiters();
                }
                prev_dead = dead;
            }

            let inventory = state.map(|(_, i, _)| i);
            if inventory != prev_inventory {
                tracing::debug!(open = ?inventory, "inventory change");
                let inventory_bool = inventory.unwrap_or_default();
                self.inner
                    .inventory_open
                    .store(inventory_bool, Ordering::Relaxed);
                if inventory_bool {
                    self.inner.on_inventory_open.notify_waiters()
                } else {
                    self.inner.on_inventory_close.notify_waiters()
                }
                prev_inventory = inventory;
            }

            let seed = state.and_then(|(_, _, s)| s);
            if seed != prev_seed {
                if let Some(seed) = seed {
                    Span::current().record("run.seed", &seed.to_string());
                }
                *self.inner.seed.lock().await = seed;
                prev_seed = seed;
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;
        }
    }

    // todo this should be part of noita-engine-reader lol
    pub async fn with<T, F>(&self, mut f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnMut(&mut Noita) -> Result<T> + Send + 'static,
    {
        let mut noita = self.inner.noita.lock().await;
        if noita.is_none() {
            *noita = find_noita().await?;
        }

        let measure = Instant::now();
        let e = match f(noita.as_mut().context("noita.exe not found")?) {
            Ok(r) => return Ok(r),
            Err(e) => e,
        };
        let elapsed = measure.elapsed();
        if elapsed.as_millis() > 100 {
            tracing::warn!(?elapsed, "slow noita call");
        }

        // if the process died we re-lookup (3 is libc::ESRCH, has no ErrorKind variant)
        if e.downcast_ref::<std::io::Error>()
            .and_then(|e| e.raw_os_error())
            == Some(3)
        {
            *noita = find_noita().await?;

            let measure = Instant::now();
            let res = f(noita.as_mut().context("noita.exe not found")?);
            let elapsed = measure.elapsed();
            if elapsed.as_millis() > 100 {
                tracing::warn!(?elapsed, "slow noita call");
            }
            res
        } else {
            Err(e)
        }
    }
}
