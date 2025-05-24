use std::{
    collections::{HashMap, HashSet},
    io,
    path::PathBuf,
    sync::{
        LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use bitflags::bitflags;
use noita_engine_reader::{
    Noita,
    discovery::KnownBuild,
    memory::{MemoryStorage, PadBool, ProcessRef, RawPtr},
    types::components::{DamageModelComponent, ItemActionComponent, ItemComponent},
};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio::{
    sync::{
        Mutex,
        broadcast::{Receiver, Sender},
    },
    time::sleep,
};
use tracing::instrument;

use crate::{context::app::AppContext, storage};

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
    StartedDrowning,
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
        let mut drowning = Changeable::new(None);
        let mut polied = Changeable::new(None);
        let mut dead = Changeable::new(None);

        let best_inv = storage!(ctx, get, "best-inventory").unwrap_or_default();
        let mut best_inv = Inventory::from_bits_truncate(best_inv);

        let mut last_flags = ctx.noita().read_flags().await.unwrap_or_default();

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

            if drowning.was_set(state.as_ref().map(|n| n.drowning)) {
                _ = ctx.noita().events.send(NoitaEvent::StartedDrowning);
            }
            if polied.was_set(state.as_ref().map(|n| n.polied)) {
                _ = ctx.noita().events.send(NoitaEvent::Polymorphed);
            }
            if dead.was_set(state.as_ref().map(|s| s.dead)) {
                tracing::info!("died");

                best_inv = Inventory::empty();
                _ = ctx.noita().events.send(NoitaEvent::PlayerDeath);
            }

            if last_inv_update.elapsed() < Duration::from_secs(1) {
                continue;
            }
            last_inv_update = Instant::now();

            let inv = match ctx.noita().with(Inventory::read).await {
                Ok(inv) => inv,
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

                if let Err(e) = storage!(ctx, set, "best-inventory", { best_inv.bits() }) {
                    tracing::error!(error=?e, "failed to save best-inventory");
                }
            }

            let current_flags = ctx.noita().read_flags().await.unwrap_or_default();
            let new_flags = current_flags
                .difference(&last_flags)
                .cloned()
                .collect::<Vec<_>>();

            if !new_flags.is_empty() {
                last_flags = current_flags;
                for flag in new_flags {
                    _ = ctx.noita().events.send(match PILLAR_FLAG_NAMES.get(&flag) {
                        Some(pillar) => NoitaEvent::PillarCompleted(pillar.clone()),
                        None => NoitaEvent::OtherPermanentFlag(flag),
                    });
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
        let e = match f(noita.as_mut().context("noita.exe not found")?) {
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
            let res = f(noita.as_mut().context("noita.exe not found")?);
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
        if flag.contains("/") || flag.contains("..") {
            return Ok(false);
        }
        // could've read PersistentFlagManager from memory, which would prob be a bit faster?.
        // but meh
        let exists = tokio::fs::try_exists(PathBuf::from(FLAG_PATH).join(flag))
            .await
            .is_ok_and(|b| b);
        Ok(exists)
    }

    pub async fn read_flags(&self) -> Result<HashSet<String>> {
        let mut set = HashSet::new();
        // same, meeh
        let mut dir = tokio::fs::read_dir(FLAG_PATH).await?;
        while let Some(f) = dir.next_entry().await? {
            set.insert(
                f.file_name()
                    .into_string()
                    .map_err(|_| anyhow!("bad file name"))?,
            );
        }
        Ok(set)
    }

    pub async fn test(&self) -> Result<()> {
        Ok(())
    }
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
            .find(|p| {
                p.thread_kind().is_none() && p.environ().contains(&"TWITCH_PLAYS_NOITA=1".into())
            })
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
    drowning: bool,
    polied: bool,
    dead: bool,
}

fn is_polied_or_drowning(noita: &mut Noita) -> io::Result<Option<(bool, bool)>> {
    match noita.get_player()? {
        Some((entity, polied)) => Ok(noita
            .component_store::<DamageModelComponent>()?
            .get(&entity)?
            .map(|c| (polied, c.air_needed.as_bool() && c.air_in_lungs <= 0.0))),
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

        let (polied, drowning) = is_polied_or_drowning(noita)?.unwrap_or_default();

        // -> CONFIG_PLAYER_STATS.stats.dead
        let dead = RawPtr::of(0x01208784)
            .read::<PadBool<3>>(noita.proc())?
            .get()
            .as_bool();
        Ok(Self {
            inventory_open,
            drowning,
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

static FLAG_PATH: &str = "../noita/steam-compat-data/pfx/drive_c/users/steamuser/AppData/LocalLow/Nolla_Games_Noita/save00/persistent/flags";

pub static PILLAR_FLAGS: LazyLock<HashSet<String>> =
    LazyLock::new(|| PILLAR_FLAG_NAMES.keys().cloned().collect());

// scripts/biomes/mountain_tree.lua
pub static PILLAR_FLAG_NAMES: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    [
        // first pillar
        ("misc_chest_rain", "Sacrifice Chest"),
        ("misc_util_rain", "Sacrifice Utility Box"),
        ("misc_worm_rain", "Sacrifice Worm Crystal"),
        ("misc_greed_rain", "Sacrifice Greed Curse"),
        ("misc_altar_tablet", "Sacrifice Tablets"),
        ("misc_mimic_potion_rain", "Sacrifice Henkevä potu"),
        ("misc_monk_bots", "Sacrifice Monk Statue"),
        ("misc_sun_effect", "Sacrifice Sun Rock"),
        ("misc_darksun_effect", "Sacrifice Dark Sun Rock"),
        ("secret_tower", "Tower"),
        ("player_status_ghostly", "Ghostly Transformation"),
        ("player_status_ratty", "Ratty Transformation"),
        ("player_status_funky", "Funky Transformation"),
        ("player_status_lukky", "Lukki Transformation"),
        ("player_status_halo", "Halo Transformation"),
        // second pillar
        ("essence_fire", "Essence of Fire"),
        ("essence_water", "Essence of Water"),
        ("essence_laser", "Essence of Earth"),
        ("essence_air", "Essence of Air"),
        ("essence_alcohol", "Essence of Spirits"),
        ("secret_moon", "Void Moon"),
        ("secret_moon2", "Drunk Moon"),
        ("special_mood", "Gourd Moon"),
        ("secret_dmoon", "Blood Moon"),
        ("dead_mood", "Dark Gourd Moon"),
        ("secret_sun_collision", "As Above, So Below"),
        ("secret_darksun_collision", "As Above, So Below (Dark)"),
        // third pillar
        ("progress_ending0", "Normal Ending"),
        ("progress_ending1_toxic", "Mountain Ending (Toxic)"),
        ("progress_ending1_gold", "Mountain Ending (Pure)"),
        ("progress_ending2", "Peaceful Ending"),
        ("progress_newgameplusplus3", "New Game+++"),
        ("progress_nightmare", "Nightmare"),
        // fourth pillar
        ("miniboss_dragon", "Suomuhauki"),
        ("miniboss_limbs", "Kolmisilmän koipi"),
        ("miniboss_meat", "Kolmisilmän sydän"),
        ("miniboss_ghost", "Unohdettu"),
        ("miniboss_pit", "Sauvojen tuntija"),
        ("miniboss_alchemist", "Ylialkemisti"),
        ("miniboss_robot", "Kolmisilmän silmä"),
        ("miniboss_wizard", "Mestarien mestari"),
        ("miniboss_maggot", "Limatoukka"),
        ("miniboss_fish", "Syväolento"),
        ("miniboss_islandspirit", "Tapion vasalli"),
        ("miniboss_threelk", "Tapio's Wrath"),
        ("miniboss_gate_monsters", "Gate Guardian"),
        ("final_secret_orb3", "Toveri"),
        ("miniboss_sky", "Kivi"),
        ("boss_centipede", "Kolmisilmä"),
        // fifth pillar
        ("progress_orb_1", "Orb"),
        ("progress_orb_evil", "Corrupted Orb"),
        ("progress_orb_all", "All Orbs"),
        ("progress_pacifist", "Pacifist"),
        ("progress_nogold", "No Gold"),
        ("progress_clock", "Dedicated to 5 Minutes"),
        ("progress_minit", "1 Minute?!"),
        ("progress_nohit", "Undamaged"),
        ("progress_sun", "Uusi Aurinko"),
        ("progress_darksun", "Pimeä Aurinko"),
        ("progress_sunkill", "Benign Sunshine!"),
        ("secret_supernova", "Supernova"),
        ("secret_greed", "Eternal Wealth"),
        ("final_secret_orb", "Friendship"),
        ("final_secret_orb2", "FRIENDSHIP"),
        ("secret_chest_dark", "Dark Chest"),
        ("secret_chest_light", "Coral Chest"),
        ("card_unlocked_everything", "The End of Everything"),
        ("card_unlocked_divide", "Avarice"),
        ("secret_fruit", "Secret Fruit"),
        ("secret_allessences", "All Essence Win"),
        ("secret_meditation", "Meditation Cube"),
        ("secret_buried_eye", "Buried Eye"),
        ("secret_hourglass", "Hourglass Chamber"),
        ("progress_hut_a", "Experimental Wand (Paint)"),
        ("progress_hut_b", "Experimental Wand (Math)"),
        ("secret_null", "Nullifying Altar"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect()
});

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
