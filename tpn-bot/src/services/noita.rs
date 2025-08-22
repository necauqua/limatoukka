use std::{
    collections::{HashMap, HashSet},
    io,
    sync::{
        LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bitflags::bitflags;
use noita_engine_reader::{
    Noita, PlayerState,
    discovery::KnownBuild,
    memory::{MemoryStorage, PadBool, ProcessRef, RawPtr},
    types::components::{DamageModelComponent, ItemActionComponent, ItemComponent},
};
use rustis::commands::StringCommands;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use thiserror::Error;
use tokio::{
    sync::{
        Mutex,
        broadcast::{Receiver, Sender},
    },
    time::sleep,
};
use tracing::instrument;

use crate::context::app::AppContext;

pub struct NoitaHandle {
    noita: Mutex<Option<Noita>>,
    inventory_open: AtomicBool,
    events: Sender<NoitaEvent>,
}

#[derive(Debug, Clone)]
pub enum NoitaEvent {
    InventoryOpened,
    InventoryClosed,
    PlayerDeath,
    Polymorphed,
    LowOxygen,
    ItemFound(ItemFound),
    PillarCompleted(String),
    NewSpellCast(String, String),
    OtherPermanentFlag(String),
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum ItemFound {
    TreeTablet,
    OtherTablet,
    EvilEye,
    EarthStone,
    TouchOfGold,
    Taikasauva,
}

impl Default for NoitaHandle {
    fn default() -> Self {
        Self {
            noita: Default::default(),
            inventory_open: Default::default(),
            events: Sender::new(16),
        }
    }
}

impl NoitaHandle {
    pub fn is_inventory_open(&self) -> bool {
        self.inventory_open.load(Ordering::Relaxed)
    }

    pub fn subscribe(&self) -> Receiver<NoitaEvent> {
        self.events.subscribe()
    }

    pub async fn poll_state_updates(ctx: AppContext) {
        let mut inventory_open = Changeable::new(None);
        let mut low_oxygen = Changeable::new(None);
        let mut polied = Changeable::new(None);
        let mut dead = Changeable::new(None);

        let best_inv = ctx
            .storage()
            .get("best-inventory")
            .await
            .unwrap_or_default();
        let mut best_inv = Inventory::from_bits_truncate(best_inv);

        let mut last_flags = ctx.noita().read_flags().await.ok();

        let mut last_inv_update = Instant::now();
        let mut inv_errored = false;

        loop {
            sleep(Duration::from_millis(30)).await;

            let state = ctx.noita().with(NoitaState::read).await.ok();
            if state.is_none() {
                // prevent busy looping when most likely noita is simply not running
                sleep(Duration::from_secs(5)).await;
            }

            if inventory_open.changed(state.as_ref().map(|n| n.inventory_open)) {
                let inventory_open = inventory_open.value.unwrap_or_default();

                ctx.noita()
                    .inventory_open
                    .store(inventory_open, Ordering::Relaxed);

                _ = ctx.noita().events.send(match inventory_open {
                    true => NoitaEvent::InventoryOpened,
                    false => NoitaEvent::InventoryClosed,
                });
            }

            if low_oxygen.was_set(state.as_ref().map(|n| n.low_oxygen)) {
                _ = ctx.noita().events.send(NoitaEvent::LowOxygen);
            }
            if polied.was_set(state.as_ref().map(|n| n.polied)) {
                _ = ctx.noita().events.send(NoitaEvent::Polymorphed);
            }
            if dead.was_set(state.as_ref().map(|s| s.dead)) {
                tracing::info!("died");

                best_inv = Inventory::empty();
                last_inv_update = Instant::now(); // avoid races with inventory reading by resetting its timer
                _ = ctx.noita().events.send(NoitaEvent::PlayerDeath);
            }

            if last_inv_update.elapsed() < Duration::from_secs(1) {
                continue;
            }
            last_inv_update = Instant::now();

            let inv = match ctx.noita().with(Inventory::read).await {
                Ok(inv) => inv,
                Err(e) if e.is::<NoNoita>() => {
                    sleep(Duration::from_secs(5)).await;
                    continue;
                }
                Err(error) => {
                    if !inv_errored {
                        tracing::warn!(?error, "failed to read player inventory");
                        inv_errored = true;
                    }
                    continue;
                }
            };
            inv_errored = false;

            let diff = inv.difference(best_inv);

            if !diff.is_empty() {
                best_inv |= inv;

                use ItemFound as I;
                use NoitaEvent as E;

                if diff.contains(Inventory::BEST_TABLET) {
                    _ = ctx.noita().events.send(E::ItemFound(I::TreeTablet));
                } else if diff.contains(Inventory::TABLET) {
                    _ = ctx.noita().events.send(E::ItemFound(I::OtherTablet));
                }
                if diff.contains(Inventory::EVIL_EYE) {
                    _ = ctx.noita().events.send(E::ItemFound(I::EvilEye));
                }
                if diff.contains(Inventory::EARTH_STONE) {
                    _ = ctx.noita().events.send(E::ItemFound(I::EarthStone));
                }
                if diff.contains(Inventory::TAIKASAUVA) {
                    _ = ctx.noita().events.send(E::ItemFound(I::Taikasauva));
                }
                if diff.contains(Inventory::TOUCH_OF_GOLD) {
                    _ = ctx.noita().events.send(E::ItemFound(I::TouchOfGold));
                }

                if let Err(e) = ctx.storage().set("best-inventory", best_inv.bits()).await {
                    tracing::error!(error=?e, "failed to save best-inventory");
                }
            }

            let Some(last_flags_ref) = last_flags.as_ref() else {
                // no flags read yet, skip
                last_flags = ctx.noita().read_flags().await.ok();
                continue;
            };

            let current_flags = ctx.noita().read_flags().await.unwrap_or_default();
            let new_flags = current_flags
                .difference(last_flags_ref)
                .cloned()
                .collect::<Vec<_>>();

            if !new_flags.is_empty() {
                last_flags = Some(current_flags);
                for flag in new_flags {
                    _ = ctx.noita().events.send(
                        if let Some(pillar) = PILLAR_FLAG_NAMES.get(&flag) {
                            NoitaEvent::PillarCompleted(pillar.clone())
                        } else if let Some(action) = ACTION_NAMES.get(&flag) {
                            NoitaEvent::NewSpellCast(flag, action.clone())
                        } else {
                            NoitaEvent::OtherPermanentFlag(flag)
                        },
                    );
                }
            }
        }
    }

    // todo this should be part of noita-engine-reader lol
    #[instrument(name = "noita-call", level = "trace", skip_all)]
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
        let e = match f(noita.as_mut().ok_or(NoNoita)?) {
            Ok(r) => return Ok(r),
            Err(e) => e,
        };
        let elapsed = measure.elapsed();
        if elapsed.as_millis() > 100 {
            tracing::warn!("slow noita call, took {elapsed:?}");
        }

        // if the process died we re-lookup (3 is libc::ESRCH, has no ErrorKind variant)
        if e.downcast_ref::<io::Error>().and_then(|e| e.raw_os_error()) == Some(3) {
            *noita = find_noita().await?;

            let measure = Instant::now();
            let res = f(noita.as_mut().ok_or(NoNoita)?);
            let elapsed = measure.elapsed();
            if elapsed.as_millis() > 100 {
                tracing::warn!("slow noita call, took {elapsed:?}");
            }
            res
        } else {
            Err(e)
        }
    }

    pub async fn has_flag(&self, flag: &str) -> Result<bool> {
        Ok(self.read_flags().await?.contains(flag))
    }

    pub async fn read_flags(&self) -> Result<HashSet<String>> {
        let set = self
            .with(|n| Ok(n.read_persistent_flag_manager()?.read_flags(n.proc())?))
            .await?
            .into_iter()
            .filter(|f| f != "_init_rendering_in_progress") // this one is temporarily set by engine on startup and we sometimes catch it
            .collect();

        Ok(set)
    }
}

#[derive(Debug, Error)]
#[error("noita.exe not found")]
struct NoNoita;

async fn find_noita() -> Result<Option<Noita>> {
    // actually does take tens of milliseconds, so we offload it
    tokio::task::spawn_blocking(|| {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_exe(UpdateKind::OnlyIfNotSet)
                .with_environ(UpdateKind::OnlyIfNotSet),
        );

        let Some(process) = system
            .processes_by_exact_name("noita.exe".as_ref())
            .find(|p| p.thread_kind().is_none() && !p.environ().contains(&"NO_TPN_BOT=1".into()))
        else {
            return Ok(None);
        };

        let proc = ProcessRef::connect(process.pid().as_u32())?;
        Ok(Some(Noita::new(proc, KnownBuild::last().map())))
    })
    .await?
}

struct NoitaState {
    inventory_open: bool,
    low_oxygen: bool,
    polied: bool,
    dead: bool,
}

fn is_polied_or_low_oxygen(noita: &mut Noita) -> io::Result<Option<(bool, bool)>> {
    match noita.get_player()? {
        Some((_, PlayerState::Polymorphed)) => Ok(Some((true, false))),
        Some((entity, PlayerState::Normal)) => Ok(noita
            .component_store::<DamageModelComponent>()?
            .get(&entity)?
            .map(|c| {
                (
                    false,
                    c.air_needed.as_bool() && c.air_in_lungs <= c.air_in_lungs_max / 2.0,
                )
            })),
        _ => Ok(None),
    }
}

impl NoitaState {
    fn read(noita: &mut Noita) -> Result<Self> {
        // -> IS_INVENTORY_OPEN (from GameIsInventoryOpen lua fn)
        let inventory_open = RawPtr::of(0x01222510)
            .read::<PadBool<3>>(noita.proc())?
            .get()
            .as_bool();

        let (polied, drowning) = is_polied_or_low_oxygen(noita)?.unwrap_or_default();

        // -> CONFIG_PLAYER_STATS.stats.dead
        let dead = RawPtr::of(0x01208784)
            .read::<PadBool<3>>(noita.proc())?
            .get()
            .as_bool();
        Ok(Self {
            inventory_open,
            low_oxygen: drowning,
            polied,
            dead,
        })
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
        let Some((entity, PlayerState::Normal)) = noita.get_player()? else {
            return Ok(Self::empty());
        };

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

pub struct Changeable<T> {
    pub value: T,
}

impl<T: PartialEq> Changeable<T> {
    pub fn new(value: T) -> Self {
        Self { value }
    }

    pub fn changed(&mut self, next: T) -> bool {
        let changed = self.value != next;
        self.value = next;
        changed
    }
}

impl Changeable<Option<bool>> {
    pub fn was_set(&mut self, next: Option<bool>) -> bool {
        self.changed(next) && next == Some(true)
    }
}

pub static PILLAR_FLAGS: LazyLock<HashSet<String>> =
    LazyLock::new(|| PILLAR_FLAG_NAMES.keys().cloned().collect());

// scripts/biomes/mountain_tree.lua
pub static PILLAR_FLAG_NAMES: LazyLock<HashMap<String, String>> =
    LazyLock::new(|| serde_yml::from_str(include_str!("../../data/pillar-names.yml")).unwrap());

pub static ACTION_FLAGS: LazyLock<HashSet<String>> =
    LazyLock::new(|| ACTION_NAMES.keys().cloned().collect());

pub static ACTION_NAMES: LazyLock<HashMap<String, String>> =
    LazyLock::new(|| serde_yml::from_str(include_str!("../../data/action-names.yml")).unwrap());
