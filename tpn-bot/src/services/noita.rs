use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use noita_engine_reader::{
    Noita, Seed,
    discovery::KnownBuild,
    memory::{MemoryStorage, PadBool, RawPtr},
    types::components::ItemComponent,
};
use strum::EnumCount;
use tokio::{
    sync::{
        Mutex, Notify,
        broadcast::{Receiver, Sender},
        futures::Notified,
    },
    time::sleep,
};
use tracing::{Instrument, Span};

#[derive(Clone)]
pub struct NoitaHandle {
    inner: Arc<Inner>,
}

#[derive(EnumCount, Clone, Copy)]
pub enum ItemFound {
    TreeTablet,
    OtherTablet,
    EvilEye,
    EarthStone,
}

struct Inner {
    noita: Mutex<Option<Noita>>,
    seed: Mutex<Option<Seed>>,
    inventory_open: AtomicBool,
    player_dead: AtomicBool,
    on_inventory_open: Notify,
    on_inventory_close: Notify,
    on_player_death: Notify,
    found_items: Arc<Sender<ItemFound>>,
}

fn is_dead(noita: &Noita) -> Result<bool> {
    // -> CONFIG_PLAYER_STATS.stats.dead
    Ok(RawPtr::of(0x01208784)
        .read::<PadBool<3>>(noita.proc())?
        .get()
        .as_bool())
}

fn read_death_count(noita: &Noita) -> Result<u32> {
    Ok(noita.read_stats().map(|stats| stats.global.death_count)?)
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
        let handle = Self {
            inner: Arc::new(Inner {
                noita: Default::default(),
                seed: Default::default(),
                inventory_open: Default::default(),
                player_dead: Default::default(),
                on_inventory_open: Default::default(),
                on_inventory_close: Default::default(),
                on_player_death: Default::default(),
                found_items: Arc::new(Sender::new(ItemFound::COUNT)),
            }),
        };
        tokio::spawn(handle.clone().start_state_polling().in_current_span());
        handle
    }

    pub fn is_inventory_open(&self) -> bool {
        self.inner.inventory_open.load(Ordering::Relaxed)
    }

    pub fn is_player_dead(&self) -> bool {
        self.inner.player_dead.load(Ordering::Relaxed)
    }

    pub async fn get_death_count(&self) -> Option<u32> {
        self.inner
            .noita
            .lock()
            .await
            .as_ref()
            .and_then(|noita| read_death_count(noita).ok())
    }

    pub async fn get_seed(&self) -> Option<Seed> {
        *self.inner.seed.lock().await
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

    pub fn subscribe_to_found_items(&self) -> Receiver<ItemFound> {
        self.inner.found_items.subscribe()
    }

    async fn start_state_polling(self) {
        let mut prev_dead = None;
        let mut prev_inventory = None;
        let mut prev_seed = None;

        let inv_handle = self.clone();
        tokio::spawn(async move {
            let mut prev_inv: Option<Inventory> = None;
            loop {
                sleep(Duration::from_secs(1)).await;

                if inv_handle.inner.player_dead.load(Ordering::Relaxed) {
                    prev_inv = None;
                }

                let Ok(inv) = inv_handle.with(Inventory::read).await else {
                    continue;
                };

                if let Some(prev) = &mut prev_inv {
                    if !prev.has_tablet.any() {
                        match inv.has_tablet {
                            Tablet::Tree => {
                                prev.has_tablet = Tablet::Tree;
                                _ = inv_handle.inner.found_items.send(ItemFound::TreeTablet);
                            }
                            Tablet::Other => {
                                prev.has_tablet = Tablet::Other;
                                _ = inv_handle.inner.found_items.send(ItemFound::OtherTablet);
                            }
                            _ => {}
                        }
                    }
                    if !prev.has_evil_eye && inv.has_evil_eye {
                        prev.has_evil_eye = true;
                        _ = inv_handle.inner.found_items.send(ItemFound::EvilEye);
                    }
                    if !prev.has_earth_stone && inv.has_earth_stone {
                        prev.has_earth_stone = true;
                        _ = inv_handle.inner.found_items.send(ItemFound::EarthStone);
                    }
                } else {
                    prev_inv = Some(inv);
                }
            }
        });

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
                    Span::current().record("run.seed", seed.to_string());
                }
                *self.inner.seed.lock().await = seed;
                prev_seed = seed;
            }

            sleep(Duration::from_millis(30)).await;
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

#[derive(Default, Clone, Copy)]
enum Tablet {
    #[default]
    None,
    Tree,
    Other,
}

impl Tablet {
    fn any(&self) -> bool {
        !matches!(self, Tablet::None)
    }
}

#[derive(Default)]
struct Inventory {
    has_tablet: Tablet,
    has_evil_eye: bool,
    has_earth_stone: bool,
}

impl Inventory {
    fn read(noita: &mut Noita) -> Result<Self> {
        let (entity, polied) = noita.get_player()?.context("no player")?;
        if polied {
            return Ok(Self::default());
        }

        let p = noita.proc().clone();

        let mut inv_quick = None;
        for child in entity.children.read(&p)?.read(&p)? {
            let child = child.read(&p)?;
            if child.name.read(&p)? == "inventory_quick" {
                inv_quick = Some(child);
                break;
            }
        }

        let inv_quick = inv_quick.context("no inventory")?;
        let store = noita.component_store::<ItemComponent>()?;
        let mut inv = Self::default();

        for child in inv_quick.children.read(&p)?.read(&p)? {
            let child = child.read(&p)?;
            let Some(item_comp) = store.get(&child)? else {
                continue;
            };
            let name = item_comp.item_name.read(&p)?;
            if name.starts_with("$booktitle") {
                inv.has_tablet = if name == "$booktitle_tree" {
                    Tablet::Tree
                } else {
                    Tablet::Other
                };
            } else if name == "$item_evil_eye" {
                inv.has_evil_eye = true;
            } else if name == "$item_stonestone" {
                inv.has_earth_stone = true;
            }
        }
        Ok(inv)
    }
}
