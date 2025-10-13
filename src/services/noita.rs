use std::{
    collections::{HashMap, HashSet},
    io,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bitflags::bitflags;
use noita_engine_reader::{
    Noita, PlayerState,
    discovery::KnownBuild,
    memory::{MemoryStorage, ProcessRef},
    types::{
        Bitset512, Vec2,
        components::{DamageModelComponent, ItemActionComponent, ItemComponent, UIIconComponent},
    },
};
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

use crate::{
    context::app::AppContext,
    injector_getter,
    services::{Service, storage::StorageServiceExt},
};

use async_trait::async_trait;

#[async_trait]
pub trait NoitaService: Service {
    async fn get_flags(&self) -> NoitaResult<HashSet<String>>;

    async fn get_win_state(&self) -> NoitaResult<WinState>;

    async fn get_seed(&self) -> NoitaResult<Option<String>>;

    async fn get_death_count(&self) -> NoitaResult<u32>;

    async fn get_streak(&self) -> NoitaResult<u32>;

    async fn get_kick_count(&self) -> NoitaResult<u32>;

    async fn get_perk_counts(&self) -> NoitaResult<Vec<(String, u32)>>;

    async fn get_damage_multipliers(&self) -> Result<Vec<(&'static str, f32)>>;

    async fn get_player_pos(&self) -> NoitaResult<PlayerPos>;

    async fn get_entity_tag_data(&self) -> NoitaResult<EntityTagData>;

    async fn get_player_distance_to(&self, entity_tag: String) -> NoitaResult<Option<f32>>;
}

injector_getter!(NoitaService::noita);

pub struct NoitaHandle {
    noita: Mutex<Option<Noita>>,
    events: Sender<NoitaEvent>,
}

#[derive(Debug, Clone)]
pub enum NoitaEvent {
    PlayerDeath,
    Polymorphed,
    LowOxygen,
    WormSummoned,
    PortalEntered,
    ItemFound(ItemFound),
    PillarCompleted(String),
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
            events: Sender::new(16),
        }
    }
}

impl NoitaHandle {
    pub fn subscribe(&self) -> Receiver<NoitaEvent> {
        self.events.subscribe()
    }

    pub async fn poll_state_updates(self: Arc<Self>, ctx: AppContext) {
        let mut low_oxygen = Changeable::new(None);
        let mut polied = Changeable::new(None);
        let mut dead = Changeable::new(None);

        let best_inv = ctx
            .storage()
            .get("best-inventory")
            .await
            .ok()
            .flatten()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or_default();

        let mut best_inv = Inventory::from_bits_truncate(best_inv);

        let mut last_flags = self.get_flags().await.ok();

        let mut last_inv_update = Instant::now();
        let mut inv_errored = false;

        loop {
            sleep(Duration::from_millis(30)).await;

            let state = self.with(NoitaState::read).await.ok();
            if state.is_none() {
                // prevent busy looping when most likely noita is simply not running
                sleep(Duration::from_secs(5)).await;
            }

            if low_oxygen.was_set(state.as_ref().map(|n| n.low_oxygen)) {
                _ = self.events.send(NoitaEvent::LowOxygen);
            }
            if polied.was_set(state.as_ref().map(|n| n.polied)) {
                _ = self.events.send(NoitaEvent::Polymorphed);
            }
            if dead.was_set(state.as_ref().map(|s| s.dead)) {
                tracing::info!("died");

                best_inv = Inventory::empty();
                last_inv_update = Instant::now(); // avoid races with inventory reading by resetting its timer
                _ = self.events.send(NoitaEvent::PlayerDeath);
            }

            if last_inv_update.elapsed() < Duration::from_secs(1) {
                continue;
            }
            last_inv_update = Instant::now();

            let inv = match self.with(Inventory::read).await {
                Ok(inv) => inv,
                Err(NoitaError::NoNoita) => {
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
                    _ = self.events.send(E::ItemFound(I::TreeTablet));
                } else if diff.contains(Inventory::TABLET) {
                    _ = self.events.send(E::ItemFound(I::OtherTablet));
                }
                if diff.contains(Inventory::EVIL_EYE) {
                    _ = self.events.send(E::ItemFound(I::EvilEye));
                }
                if diff.contains(Inventory::EARTH_STONE) {
                    _ = self.events.send(E::ItemFound(I::EarthStone));
                }
                if diff.contains(Inventory::TAIKASAUVA) {
                    _ = self.events.send(E::ItemFound(I::Taikasauva));
                }
                if diff.contains(Inventory::TOUCH_OF_GOLD) {
                    _ = self.events.send(E::ItemFound(I::TouchOfGold));
                }
                if diff.contains(Inventory::WORM_SUMMON) {
                    _ = self.events.send(E::WormSummoned);
                }
                if diff.contains(Inventory::PORTAL_ENTERED) {
                    _ = self.events.send(E::PortalEntered);
                }

                if let Err(e) = ctx
                    .storage()
                    .set("best-inventory", &best_inv.bits().to_string())
                    .await
                {
                    tracing::error!(error=?e, "failed to save best-inventory");
                }
            }

            let Some(last_flags_ref) = last_flags.as_ref() else {
                // no flags read yet, skip
                last_flags = self.get_flags().await.ok();
                continue;
            };

            let current_flags = self.get_flags().await.unwrap_or_default();
            let new_flags = current_flags
                .difference(last_flags_ref)
                .cloned()
                .collect::<Vec<_>>();

            if !new_flags.is_empty() {
                last_flags = Some(current_flags);
                for flag in new_flags {
                    _ = self
                        .events
                        .send(if let Some(pillar) = PILLAR_FLAG_NAMES.get(&flag) {
                            NoitaEvent::PillarCompleted(pillar.clone())
                        } else {
                            NoitaEvent::OtherPermanentFlag(flag)
                        });
                }
            }
        }
    }

    // todo this should be part of noita-engine-reader lol
    #[instrument(name = "noita-call", level = "trace", skip_all)]
    async fn with<T, F>(&self, mut f: F) -> NoitaResult<T>
    where
        T: Send + 'static,
        F: FnMut(&mut Noita) -> NoitaResult<T> + Send + 'static,
    {
        let mut noita = self.noita.lock().await;
        if noita.is_none() {
            *noita = find_noita().await?;
        }

        let measure = Instant::now();
        let e = match f(noita.as_mut().ok_or(NoitaError::NoNoita)?) {
            Ok(r) => return Ok(r),
            Err(e) => e,
        };
        let elapsed = measure.elapsed();
        if elapsed.as_millis() > 100 {
            tracing::warn!("slow noita call, took {elapsed:?}");
        }

        // if the process died we re-lookup (3 is libc::ESRCH, has no ErrorKind variant)
        match e {
            NoitaError::NoNoita => {}
            NoitaError::Internal(e) => {
                if e.downcast_ref::<io::Error>().and_then(|e| e.raw_os_error()) != Some(3) {
                    return Err(e.into());
                }
            }
            e => return Err(e),
        };
        *noita = find_noita().await?;

        let measure = Instant::now();
        let res = f(noita.as_mut().ok_or(NoitaError::NoNoita)?);
        let elapsed = measure.elapsed();
        if elapsed.as_millis() > 100 {
            tracing::warn!("slow noita call, took {elapsed:?}");
        }
        res
    }
}

#[async_trait]
impl NoitaService for NoitaHandle {
    async fn get_flags(&self) -> NoitaResult<HashSet<String>> {
        let set = self
            .with(|n| Ok(n.read_persistent_flag_manager()?.read_flags(n.proc())?))
            .await?
            .into_iter()
            .filter(|f| f != "_init_rendering_in_progress") // this one is temporarily set by engine on startup and we sometimes catch it
            .collect();

        Ok(set)
    }

    /// This assumes the game is over and player being alive is not actually checked.
    async fn get_win_state(&self) -> NoitaResult<WinState> {
        self.with(|n| {
            let Some(ws) = n.get_world_state()? else {
                return Ok(WinState::Loss);
            };
            let flags = ws.flags.read_storage(n.proc())?;
            if flags.iter().any(|f| f == "ending_game_completed") {
                let cheese = flags
                    .iter()
                    .any(|f| f == "kantele_secret_02" || f == "ocarina_secret_02");
                return Ok(WinState::Win { cheese });
            }
            Ok(WinState::Loss)
        })
        .await
    }

    async fn get_seed(&self) -> NoitaResult<Option<String>> {
        self.with(|n| Ok(n.read_seed()?.map(|s| s.to_string())))
            .await
    }

    async fn get_death_count(&self) -> NoitaResult<u32> {
        self.with(|n| Ok(n.read_stats()?.global.death_count)).await
    }

    async fn get_streak(&self) -> NoitaResult<u32> {
        self.with(|n| Ok(n.read_stats()?.session.streaks)).await
    }

    async fn get_kick_count(&self) -> NoitaResult<u32> {
        self.with(|n| Ok(n.read_config_player_stats()?.stats.kicks))
            .await
    }

    async fn get_perk_counts(&self) -> NoitaResult<Vec<(String, u32)>> {
        let perks = self
            .with(|n| {
                let Some((entity, _)) = n.get_player()? else {
                    return Err(NoitaError::NoPlayer);
                };

                let perk_tag = n.get_entity_tag_index("perk")?;

                // todo cache the translations lol
                let translations = n.translations()?;

                let store = n.component_store::<UIIconComponent>()?;
                let p = n.proc();

                let mut perks = HashMap::<_, u32>::new();

                for child in entity.children.read(p)?.read(p)? {
                    let child = child.read(p)?;
                    if !child.tags[perk_tag] {
                        continue;
                    }
                    let Some(ui_comp) = store.get(&child)? else {
                        continue;
                    };

                    let name = ui_comp.name.read(p)?;

                    let translated = translations.translate(name.trim_start_matches("$"), true);
                    let name = translated.unwrap_or(name);

                    *perks.entry(name).or_default() += 1;
                }
                Ok(perks)
            })
            .await?;

        let mut perks = perks.into_iter().collect::<Vec<_>>();
        perks.sort_unstable_by_key(|(_, c)| -(*c as i32));
        Ok(perks)
    }

    async fn get_damage_multipliers(&self) -> Result<Vec<(&'static str, f32)>> {
        let dmc = self
            .with(|n| {
                let Some((entity, _)) = n.get_player()? else {
                    return Err(NoitaError::NoPlayer);
                };

                let component = n
                    .component_store::<DamageModelComponent>()?
                    .get(&entity)?
                    .context("no damage model component?")?;

                Ok(component)
            })
            .await?;

        let m = dmc.damage_multipliers;

        let mut result = Vec::new();

        macro_rules! fields {
            ($($fields:ident),* $(,)?) => {
                $(result.push((stringify!($fields), m.$fields));)*
            };
        }

        #[rustfmt::skip]
        fields!(
            melee, projectile, explosion, electricity,
            fire, drill, slice, ice, healing, physics_hit,
            radioactive, poison, overeating, curse, holy,
        );

        Ok(result)
    }

    async fn get_player_pos(&self) -> NoitaResult<PlayerPos> {
        let (e, ng_count) = self
            .with(|n| {
                let (e, _) = n.get_player()?.ok_or(NoitaError::NoPlayer)?;
                let ng_count = n.read_ng_plus()?;
                Ok((e, ng_count))
            })
            .await?;

        let Vec2 { x, y } = e.transform.pos;

        let pw = if ng_count == 0 {
            x / 512.0 / 70.0 // NG is 70 chunks
        } else {
            x / 512.0 / 64.0 // and NG+ is 64
        }
        .round() as i32;

        Ok(PlayerPos {
            x,
            y,
            parallel: pw,
            parallel_v: 0,
        })
    }

    async fn get_entity_tag_data(&self) -> NoitaResult<EntityTagData> {
        self.with(|n| {
            let all_tags = n
                .read_entity_tag_manager()
                .context("no entity tag manager")?
                .tags
                .read_storage(n.proc())?;
            let manager = n.read_entity_manager().context("no entity manager")?;
            let entities = manager.entities.read(n.proc())?;

            let mut entity_tags = Vec::with_capacity(entities.len());
            for entity in entities {
                if !entity.is_null() {
                    entity_tags.push(entity.read(n.proc())?.tags);
                }
            }

            Ok(EntityTagData {
                all_tags,
                entity_tags,
            })
        })
        .await
    }

    async fn get_player_distance_to(&self, entity_tag: String) -> NoitaResult<Option<f32>> {
        self.with(move |n| {
            let (e, _) = n.get_player()?.ok_or(NoitaError::NoPlayer)?;

            let Some(tag_idx) = n.get_entity_tag_index(&entity_tag)? else {
                return Ok(None);
            };

            let manager = n.read_entity_manager()?;
            let Some(bucket) = manager.entity_buckets.get(tag_idx as _) else {
                return Ok(None);
            };
            let mut min_dist_sqr = None;
            for ptr in bucket.read(n.proc())?.read(n.proc())? {
                if ptr.is_null() {
                    continue;
                }
                let entity = ptr.read(n.proc())?;
                let pos = entity.transform.pos;
                let dx = pos.x - e.transform.pos.x;
                let dy = pos.y - e.transform.pos.y;
                let dist_sqr = dx * dx + dy * dy;
                if min_dist_sqr.is_none_or(|d| dist_sqr < d) {
                    min_dist_sqr = Some(dist_sqr);
                }
            }
            Ok(min_dist_sqr.map(|d| d.sqrt()))
        })
        .await
    }
}

#[derive(Debug, Error)]
pub enum NoitaError {
    #[error("the game is not running")]
    NoNoita,
    #[error("player entity not found")]
    NoPlayer,
    #[error("internal")]
    Internal(#[from] anyhow::Error),
}

// engine reader talks in io::Error's 🤷
impl From<std::io::Error> for NoitaError {
    fn from(value: io::Error) -> Self {
        NoitaError::Internal(value.into())
    }
}

pub type NoitaResult<T> = std::result::Result<T, NoitaError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinState {
    Loss,
    Win { cheese: bool },
}

pub struct PlayerPos {
    pub x: f32,
    pub y: f32,
    pub parallel: i32,
    pub parallel_v: i32,
}

pub struct EntityTagData {
    pub all_tags: Vec<String>,
    pub entity_tags: Vec<Bitset512>,
}

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
    fn read(noita: &mut Noita) -> NoitaResult<Self> {
        let (polied, drowning) = is_polied_or_low_oxygen(noita)?.unwrap_or_default();
        let dead = noita.read_config_player_stats()?.stats.dead.as_bool();

        Ok(Self {
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
        const WORM_SUMMON = 1 << 6;
        const PORTAL_ENTERED = 1 << 7;
    }
}

impl Inventory {
    fn read(noita: &mut Noita) -> NoitaResult<Self> {
        let Some((entity, PlayerState::Normal)) = noita.get_player()? else {
            return Ok(Self::empty());
        };

        let p = noita.proc().clone();
        let p = &p;

        let mut inv_quick = None;
        let mut inv_full = None;
        for child in entity.children.read(p)?.read(p)? {
            let child = child.read(p)?;
            match &*child.name.read(p)? {
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

        if entity.transform.pos.y > 1200.0 {
            inv |= Inventory::PORTAL_ENTERED;
        }

        for child in inv_quick.children.read(p)?.read(p)? {
            let child = child.read(p)?;
            let Some(item_comp) = store.get(&child)? else {
                continue;
            };
            let name = item_comp.item_name.read(p)?;
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
        for child in inv_full.children.read(p)?.read(p)? {
            let child = child.read(p)?;
            let Some(item_action_comp) = action_store.get(&child)? else {
                continue;
            };
            let action_id = item_action_comp.action_id.read(p)?;
            inv |= match &*action_id {
                "TOUCH_GOLD" => Inventory::TOUCH_OF_GOLD,
                "SUMMON_WANDGHOST" => Inventory::TAIKASAUVA,
                _ => Inventory::empty(),
            };
        }

        let Some(ws) = noita.get_world_state()? else {
            return Ok(inv);
        };
        let worm = ws
            .flags
            .read_storage(p)?
            .iter()
            .any(|f| f == "kantele_secret_02" || f == "ocarina_secret_02");
        if worm {
            inv |= Inventory::WORM_SUMMON;
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
