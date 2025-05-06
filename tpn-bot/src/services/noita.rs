use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bitflags::bitflags;
use noita_engine_reader::{
    Noita, Seed,
    discovery::KnownBuild,
    memory::{MemoryStorage, PadBool, RawPtr},
    types::components::{ItemActionComponent, ItemComponent},
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
use tracing::Span;

pub struct NoitaHandle {
    noita: Mutex<Option<Noita>>,
    seed: Mutex<Option<Seed>>,
    inventory_open: AtomicBool,
    on_inventory_open: Notify,
    on_inventory_close: Notify,
    on_player_death: Notify,
    found_items: Arc<Sender<ItemFound>>,
    reset_items: AtomicBool,
}

#[derive(Debug, EnumCount, Clone, Copy)]
#[repr(u8)]
pub enum ItemFound {
    TreeTablet,
    OtherTablet,
    EvilEye,
    EarthStone,
    TouchOfGold,
    Taikasauva,
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

impl Default for NoitaHandle {
    fn default() -> Self {
        Self {
            noita: Default::default(),
            seed: Default::default(),
            inventory_open: Default::default(),
            on_inventory_open: Default::default(),
            on_inventory_close: Default::default(),
            on_player_death: Default::default(),
            found_items: Arc::new(Sender::new(ItemFound::COUNT)),
            reset_items: Default::default(),
        }
    }
}

impl NoitaHandle {
    pub fn is_inventory_open(&self) -> bool {
        self.inventory_open.load(Ordering::Relaxed)
    }

    pub async fn get_seed(&self) -> Option<Seed> {
        *self.seed.lock().await
    }

    pub fn wait_for_inventory_open(&self) -> Notified {
        self.on_inventory_open.notified()
    }

    pub fn wait_for_inventory_close(&self) -> Notified {
        self.on_inventory_close.notified()
    }

    pub fn wait_for_player_death(&self) -> Notified {
        self.on_player_death.notified()
    }

    pub fn subscribe_to_found_items(&self) -> Receiver<ItemFound> {
        self.found_items.subscribe()
    }

    pub fn reset_inventory(&self) {
        self.reset_items.store(true, Ordering::Relaxed);
    }

    pub async fn poll_state_updates(&self) {
        let mut prev_dead = None;
        let mut prev_inventory = None;
        let mut prev_seed = None;
        let mut best_inv = None;

        let mut last_inv_update = Instant::now();

        loop {
            sleep(Duration::from_millis(30)).await;

            let state = self
                .with(|n| Ok((is_dead(n)?, is_inventory_open(n)?, get_seed(n)?)))
                .await
                .ok();

            let dead = state.map(|(d, _, _)| d);
            if dead != prev_dead {
                let dead_bool = dead.unwrap_or_default();
                if dead_bool {
                    tracing::debug!("died");
                    best_inv = None;
                    self.on_player_death.notify_waiters();
                }
                prev_dead = dead;
            }
            if self.reset_items.swap(false, Ordering::Relaxed) {
                best_inv = None;
            }

            let inventory = state.map(|(_, i, _)| i);
            if inventory != prev_inventory {
                tracing::debug!(open = ?inventory, "inventory change");
                let inventory_bool = inventory.unwrap_or_default();
                self.inventory_open.store(inventory_bool, Ordering::Relaxed);
                if inventory_bool {
                    self.on_inventory_open.notify_waiters()
                } else {
                    self.on_inventory_close.notify_waiters()
                }
                prev_inventory = inventory;
            }

            let seed = state.and_then(|(_, _, s)| s);
            if seed != prev_seed {
                if let Some(seed) = seed {
                    Span::current().record("run.seed", seed.to_string());
                }
                *self.seed.lock().await = seed;
                prev_seed = seed;
            }

            if last_inv_update.elapsed() < Duration::from_secs(1) {
                continue;
            }
            last_inv_update = Instant::now();

            let inv = match self.with(Inventory::read).await {
                Ok(inv) => inv,
                Err(error) => {
                    tracing::warn!(?error, "failed to read player inventory");
                    continue;
                }
            };
            let Some(best_inv) = &mut best_inv else {
                best_inv = Some(inv);
                continue;
            };

            let diff = inv.difference(*best_inv);

            if diff.contains(Inventory::BEST_TABLET) {
                _ = self.found_items.send(ItemFound::TreeTablet);
            } else if diff.contains(Inventory::TABLET) {
                _ = self.found_items.send(ItemFound::OtherTablet);
            }
            if diff.contains(Inventory::EVIL_EYE) {
                _ = self.found_items.send(ItemFound::EvilEye);
            }
            if diff.contains(Inventory::EARTH_STONE) {
                _ = self.found_items.send(ItemFound::EarthStone);
            }
            if diff.contains(Inventory::TAIKASAUVA) {
                _ = self.found_items.send(ItemFound::Taikasauva);
            }
            if diff.contains(Inventory::TOUCH_OF_GOLD) {
                _ = self.found_items.send(ItemFound::TouchOfGold);
            }

            *best_inv |= inv;
        }
    }

    // todo this should be part of noita-engine-reader lol
    pub async fn with<T, F>(&self, mut f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnMut(&mut Noita) -> Result<T> + Send + 'static,
    {
        let mut noita = self.noita.lock().await;
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

bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct Inventory: u64 {
        const TABLET = 1 << 0;
        const BEST_TABLET = 1 << 1;
        const EVIL_EYE = 1 << 2;
        const EARTH_STONE = 1 << 3;
        const TOUCH_OF_GOLD = 1 << 4;
        const TAIKASAUVA = 1 << 5;
    }
}

impl Inventory {
    fn read(noita: &mut Noita) -> Result<Self> {
        let (entity, polied) = noita.get_player()?.context("no player")?;
        if polied {
            return Ok(Self::empty());
        }

        let p = noita.proc().clone();

        let mut inv_quick = None;
        let mut inv_full = None;
        for child in entity.children.read(&p)?.read(&p)? {
            let child = child.read(&p)?;
            match &*child.name.read(&p)? {
                "inventory_quick" => {
                    inv_quick = Some(child);
                }
                "inventory_full" => {
                    inv_full = Some(child);
                }
                _ => {}
            }
        }

        let inv_quick = inv_quick.context("no inventory")?;
        let inv_full = inv_full.context("no inventory")?;
        let store = noita.component_store::<ItemComponent>()?;
        let action_store = noita.component_store::<ItemActionComponent>()?;
        let mut inv = Self::empty();

        for child in inv_quick.children.read(&p)?.read(&p)? {
            let child = child.read(&p)?;
            let Some(item_comp) = store.get(&child)? else {
                continue;
            };
            let name = item_comp.item_name.read(&p)?;
            if name.starts_with("$booktitle") {
                if name == "$booktitle_tree" {
                    inv |= Inventory::BEST_TABLET;
                }
                inv |= Inventory::TABLET;
            } else {
                inv |= match &*name {
                    "$item_evil_eye" => Inventory::EVIL_EYE,
                    "$item_stonestone" => Inventory::EARTH_STONE,
                    _ => Inventory::empty(),
                };
            }
        }
        // apparently this can happen
        if inv_full.children.is_null() {
            return Ok(inv);
        }
        for child in inv_full.children.read(&p)?.read(&p)? {
            let child = child.read(&p)?;
            let Some(item_action_comp) = action_store.get(&child)? else {
                continue;
            };
            let action_id = item_action_comp.action_id.read(&p)?;
            inv |= match &*action_id {
                "TOUCH_GOLD" => Inventory::TOUCH_OF_GOLD,
                "SUMMON_WANDGHOST" => Inventory::TAIKASAUVA,
                _ => Inventory::empty(),
            };
        }
        Ok(inv)
    }
}
