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
    Noita, PlayerState,
    discovery::KnownBuild,
    memory::{MemoryStorage, PadBool, ProcessRef, RawPtr},
    types::components::{DamageModelComponent, ItemActionComponent, ItemComponent},
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

            if low_oxygen.was_set(state.as_ref().map(|n| n.low_oxygen)) {
                _ = ctx.noita().events.send(NoitaEvent::LowOxygen);
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

pub static ACTION_FLAGS: LazyLock<HashSet<String>> =
    LazyLock::new(|| ACTION_NAMES.keys().cloned().collect());

// scripts/biomes/mountain_tree.lua
pub static ACTION_NAMES: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    [
        ("action_bomb", "Bomb"),
        ("action_light_bullet", "Spark bolt"),
        ("action_light_bullet_trigger", "Spark bolt with trigger"),
        (
            "action_light_bullet_trigger_2",
            "Spark bolt with double trigger",
        ),
        ("action_light_bullet_timer", "Spark bolt with timer"),
        ("action_bullet", "Magic arrow"),
        ("action_bullet_trigger", "Magic arrow with trigger"),
        ("action_bullet_timer", "Magic arrow with timer"),
        ("action_heavy_bullet", "Magic bolt"),
        ("action_heavy_bullet_trigger", "Magic bolt with trigger"),
        ("action_heavy_bullet_timer", "Magic bolt with timer"),
        ("action_air_bullet", "Burst of air"),
        ("action_slow_bullet", "Energy orb"),
        ("action_slow_bullet_trigger", "Energy orb with a trigger"),
        ("action_slow_bullet_timer", "Energy orb with a timer"),
        ("action_hook", "Hookbolt"),
        ("action_black_hole", "Black hole"),
        (
            "action_black_hole_death_trigger",
            "Black Hole with Death Trigger",
        ),
        ("action_white_hole", "White hole"),
        ("action_black_hole_big", "Giga black hole"),
        ("action_white_hole_big", "Giga white hole"),
        ("action_black_hole_giga", "Omega Black Hole"),
        ("action_white_hole_giga", "Omega white hole"),
        ("action_tentacle_portal", "Eldritch portal"),
        ("action_spitter", "Spitter bolt"),
        ("action_spitter_timer", "Spitter bolt with timer"),
        ("action_spitter_tier_2", "Large spitter bolt"),
        (
            "action_spitter_tier_2_timer",
            "Large spitter bolt with timer",
        ),
        ("action_spitter_tier_3", "Giant spitter bolt"),
        (
            "action_spitter_tier_3_timer",
            "Giant spitter bolt with timer",
        ),
        ("action_bubbleshot", "Bubble spark"),
        ("action_bubbleshot_trigger", "Bubble spark with trigger"),
        ("action_disc_bullet", "Disc projectile"),
        ("action_disc_bullet_big", "Giga disc projectile"),
        ("action_disc_bullet_bigger", "Summon Omega Sawblade"),
        ("action_bouncy_orb", "Energy sphere"),
        ("action_bouncy_orb_timer", "Energy sphere with timer"),
        ("action_rubber_ball", "Bouncing burst"),
        ("action_arrow", "Arrow"),
        ("action_pollen", "Pollen"),
        ("action_lance", "Glowing lance"),
        ("action_lance_holy", "Holy Lance"),
        ("action_rocket", "Magic missile"),
        ("action_rocket_tier_2", "Large magic missile"),
        ("action_rocket_tier_3", "Giant magic missile"),
        ("action_grenade", "Firebolt"),
        ("action_grenade_trigger", "Firebolt with trigger"),
        ("action_grenade_tier_2", "Large firebolt"),
        ("action_grenade_tier_3", "Giant firebolt"),
        ("action_grenade_anti", "Odd Firebolt"),
        ("action_grenade_large", "Dropper bolt"),
        ("action_mine", "Unstable crystal"),
        ("action_mine_death_trigger", "Unstable crystal with trigger"),
        ("action_pipe_bomb", "Dormant crystal"),
        (
            "action_pipe_bomb_death_trigger",
            "Dormant crystal with trigger",
        ),
        ("action_fish", "Summon fish"),
        ("action_exploding_deer", "Summon deercoy"),
        ("action_exploding_ducks", "Flock of Ducks"),
        ("action_worm_shot", "Worm Launcher"),
        ("action_bomb_detonator", "Explosive Detonator"),
        ("action_laser", "Concentrated light"),
        ("action_megalaser", "Intense concentrated light"),
        ("action_lightning", "Lightning bolt"),
        ("action_ball_lightning", "Ball Lightning"),
        ("action_laser_emitter", "Plasma beam"),
        ("action_laser_emitter_four", "Plasma Beam Cross"),
        ("action_laser_emitter_cutter", "Plasma Cutter"),
        ("action_digger", "Digging bolt"),
        ("action_powerdigger", "Digging blast"),
        ("action_chainsaw", "Chainsaw"),
        ("action_luminous_drill", "Luminous drill"),
        ("action_laser_luminous_drill", "Luminous drill with timer"),
        ("action_tentacle", "Summon Tentacle"),
        ("action_tentacle_timer", "Summon Tentacle with timer"),
        ("action_heal_bullet", "Healing bolt"),
        ("action_antiheal", "Deadly heal"),
        ("action_spiral_shot", "Spiral shot"),
        ("action_magic_shield", "Magic guard"),
        ("action_big_magic_shield", "Big magic guard"),
        ("action_chain_bolt", "Chain bolt"),
        ("action_fireball", "Fireball"),
        ("action_meteor", "Meteor"),
        ("action_flamethrower", "Flamethrower"),
        ("action_iceball", "Iceball"),
        ("action_slimeball", "Slimeball"),
        ("action_darkflame", "Path of dark flame"),
        ("action_missile", "Summon missile"),
        ("action_funky_spell", "???"),
        ("action_pebble", "Summon rock spirit"),
        ("action_dynamite", "Dynamite"),
        ("action_glitter_bomb", "Glitter bomb"),
        ("action_buckshot", "Triplicate bolt"),
        ("action_freezing_gaze", "Freezing gaze"),
        ("action_glowing_bolt", "Pinpoint of light"),
        ("action_spore_pod", "Prickly Spore Pod"),
        ("action_glue_shot", "Glue Ball"),
        ("action_bomb_holy", "Holy Bomb"),
        ("action_bomb_holy_giga", "Giga Holy Bomb"),
        ("action_propane_tank", "Propane tank"),
        ("action_bomb_cart", "Bomb cart"),
        ("action_cursed_orb", "Cursed sphere"),
        ("action_expanding_orb", "Expanding Sphere"),
        ("action_crumbling_earth", "Earthquake"),
        ("action_summon_rock", "Rock"),
        ("action_summon_egg", "Summon egg"),
        ("action_summon_hollow_egg", "Summon hollow egg"),
        ("action_tntbox", "Summon Explosive Box"),
        ("action_tntbox_big", "Summon Large Explosive Box"),
        ("action_swarm_fly", "Summon fly swarm"),
        ("action_swarm_firebug", "Summon Firebug swarm"),
        ("action_swarm_wasp", "Summon Wasp swarm"),
        ("action_friend_fly", "Summon Friendly fly"),
        ("action_acidshot", "Acid ball"),
        ("action_thunderball", "Thunder charge"),
        ("action_firebomb", "Firebomb"),
        ("action_soilball", "Chunk of soil"),
        ("action_death_cross", "Death cross"),
        ("action_death_cross_big", "Giga death cross"),
        ("action_infestation", "Infestation"),
        ("action_wall_horizontal", "Horizontal barrier"),
        ("action_wall_vertical", "Vertical barrier"),
        ("action_wall_square", "Square barrier"),
        ("action_temporary_wall", "Summon Wall"),
        ("action_temporary_platform", "Summon Platform"),
        ("action_purple_explosion_field", "Glittering field"),
        ("action_delayed_spell", "Delayed spellcast"),
        ("action_long_distance_cast", "Long-distance cast"),
        ("action_teleport_cast", "Teleporting cast"),
        ("action_super_teleport_cast", "Warp cast"),
        ("action_caster_cast", "Inner spell"),
        ("action_mist_radioactive", "Toxic mist"),
        ("action_mist_alcohol", "mist of spirits"),
        ("action_mist_slime", "Slime mist"),
        ("action_mist_blood", "Blood mist"),
        ("action_circle_fire", "Circle of fire"),
        ("action_circle_acid", "Circle of acid"),
        ("action_circle_oil", "Circle of oil"),
        ("action_circle_water", "Circle of water"),
        ("action_material_water", "Water"),
        ("action_material_oil", "Oil"),
        ("action_material_blood", "Blood"),
        ("action_material_acid", "Acid"),
        ("action_material_cement", "Cement"),
        ("action_teleport_projectile", "Teleport bolt"),
        ("action_teleport_projectile_short", "Small Teleport Bolt"),
        ("action_teleport_projectile_static", "Return"),
        ("action_swapper_projectile", "Swapper"),
        (
            "action_teleport_projectile_closer",
            "Homebringer Teleport Bolt",
        ),
        ("action_nuke", "Nuke"),
        ("action_nuke_giga", "Giga Nuke"),
        ("action_firework", "Fireworks!"),
        ("action_summon_wandghost", "Summon Taikasauva"),
        ("action_touch_gold", "Touch of Gold"),
        ("action_touch_water", "Touch of Water"),
        ("action_touch_oil", "Touch of Oil"),
        ("action_touch_alcohol", "Touch of Spirits"),
        ("action_touch_piss", "Touch of Gold?"),
        ("action_touch_grass", "Touch of Grass"),
        ("action_touch_blood", "Touch of Blood"),
        ("action_touch_smoke", "Touch of Smoke"),
        ("action_destruction", "Destruction"),
        ("action_mass_polymorph", "Muodonmuutos"),
        ("action_burst_2", "Double spell"),
        ("action_burst_3", "Triple spell"),
        ("action_burst_4", "Quadruple spell"),
        ("action_burst_8", "Octuple spell"),
        ("action_burst_x", "Myriad Spell"),
        ("action_scatter_2", "Double scatter spell"),
        ("action_scatter_3", "Triple scatter spell"),
        ("action_scatter_4", "Quadruple scatter spell"),
        ("action_i_shape", "Formation - behind your back"),
        ("action_y_shape", "Formation - bifurcated"),
        ("action_t_shape", "Formation - above and below"),
        ("action_w_shape", "Formation - trifurcated"),
        ("action_circle_shape", "Formation - hexagon"),
        ("action_pentagram_shape", "Formation - pentagon"),
        ("action_i_shot", "Iplicate Spell"),
        ("action_y_shot", "Yplicate Spell"),
        ("action_t_shot", "Tiplicate Spell"),
        ("action_w_shot", "Wuplicate Spell"),
        ("action_quad_shot", "Quplicate Spell"),
        ("action_penta_shot", "Peplicate Spell"),
        ("action_hexa_shot", "Heplicate Spell"),
        ("action_spread_reduce", "Reduce spread"),
        ("action_heavy_spread", "Heavy spread"),
        ("action_recharge", "Reduce recharge time"),
        ("action_lifetime", "Increase lifetime"),
        ("action_lifetime_down", "Reduce lifetime"),
        ("action_nolla", "Nolla"),
        ("action_slow_but_steady", "Slow But Steady"),
        ("action_explosion_remove", "Remove Explosion"),
        ("action_explosion_tiny", "Concentrated Explosion"),
        ("action_laser_emitter_wider", "Plasma Beam Enhancer"),
        ("action_mana_reduce", "Add mana"),
        ("action_blood_magic", "Blood magic"),
        ("action_money_magic", "Gold to Power"),
        ("action_blood_to_power", "Blood to Power"),
        ("action_duplicate", "Spell duplication"),
        ("action_quantum_split", "Quantum Split"),
        ("action_gravity", "Gravity"),
        ("action_gravity_anti", "Anti-gravity"),
        ("action_sinewave", "Slithering path"),
        ("action_chaotic_arc", "Chaotic path"),
        ("action_pingpong_path", "Ping-pong path"),
        ("action_avoiding_arc", "Avoiding arc"),
        ("action_floating_arc", "Floating arc"),
        ("action_fly_downwards", "Fly downwards"),
        ("action_fly_upwards", "Fly upwards"),
        ("action_horizontal_arc", "Horizontal path"),
        ("action_line_arc", "Linear arc"),
        ("action_orbit_shot", "Orbiting Arc"),
        ("action_spiraling_shot", "Spiral Arc"),
        ("action_phasing_arc", "Phasing Arc"),
        ("action_true_orbit", "True Orbit"),
        ("action_bounce", "Bounce"),
        ("action_remove_bounce", "Remove Bounce"),
        ("action_homing", "Homing"),
        ("action_anti_homing", "Anti Homing"),
        ("action_homing_wand", "Wand Homing"),
        ("action_homing_short", "Short-range Homing"),
        ("action_homing_rotate", "Rotate towards foes"),
        ("action_homing_shooter", "Boomerang"),
        ("action_autoaim", "Auto-Aim"),
        ("action_homing_accelerating", "Accelerative Homing"),
        ("action_homing_cursor", "Aiming Arc"),
        ("action_homing_area", "Projectile Area Teleport"),
        ("action_piercing_shot", "Piercing shot"),
        ("action_clipping_shot", "Drilling shot"),
        ("action_damage", "Damage Plus"),
        ("action_damage_random", "Random damage"),
        ("action_bloodlust", "Bloodlust"),
        ("action_damage_forever", "Mana To Damage"),
        ("action_critical_hit", "Critical Plus"),
        ("action_area_damage", "Damage field"),
        ("action_spells_to_power", "Spells to Power"),
        ("action_essence_to_power", "Essence to Power"),
        ("action_zero_damage", "Null shot"),
        ("action_heavy_shot", "Heavy Shot"),
        ("action_light_shot", "Light shot"),
        ("action_knockback", "Knockback"),
        ("action_recoil", "Recoil"),
        ("action_recoil_damper", "Recoil Damper"),
        ("action_speed", "Speed Up"),
        ("action_accelerating_shot", "Accelerating shot"),
        ("action_decelerating_shot", "Decelerating shot"),
        ("action_explosive_projectile", "Explosive projectile"),
        ("action_clustermod", "Clusterbolt"),
        ("action_water_to_poison", "Water to poison"),
        ("action_blood_to_acid", "Blood to acid"),
        ("action_lava_to_blood", "Lava to blood"),
        ("action_liquid_to_explosion", "Liquid Detonation"),
        ("action_toxic_to_acid", "Toxic sludge to acid"),
        ("action_static_to_sand", "Ground to sand"),
        ("action_transmutation", "Chaotic transmutation"),
        ("action_random_explosion", "Chaos magic"),
        ("action_necromancy", "Necromancy"),
        ("action_light", "Light"),
        ("action_explosion", "Explosion"),
        ("action_explosion_light", "Magical Explosion"),
        ("action_fire_blast", "Explosion of brimstone"),
        ("action_poison_blast", "Explosion of poison"),
        ("action_alcohol_blast", "Explosion of spirits"),
        ("action_thunder_blast", "Explosion of thunder"),
        ("action_berserk_field", "Circle of fervour"),
        ("action_polymorph_field", "Circle of transmogrification"),
        (
            "action_chaos_polymorph_field",
            "Circle of unstable metamorphosis",
        ),
        ("action_electrocution_field", "Circle of thunder"),
        ("action_freeze_field", "Circle of stillness"),
        ("action_regeneration_field", "Circle of vigour"),
        ("action_teleportation_field", "Circle of displacement"),
        ("action_levitation_field", "Circle of buoyancy"),
        ("action_shield_field", "Circle of shielding"),
        (
            "action_projectile_transmutation_field",
            "Projectile transmutation field",
        ),
        (
            "action_projectile_thunder_field",
            "Projectile thunder field",
        ),
        (
            "action_projectile_gravity_field",
            "Projectile gravity field",
        ),
        ("action_vacuum_powder", "Powder Vacuum Field"),
        ("action_vacuum_liquid", "Liquid Vacuum Field"),
        ("action_vacuum_entities", "Vacuum Field"),
        ("action_sea_lava", "Sea of lava"),
        ("action_sea_alcohol", "Sea of alcohol"),
        ("action_sea_oil", "Sea of oil"),
        ("action_sea_water", "Sea of water"),
        ("action_sea_swamp", "Summon Swamp"),
        ("action_sea_acid", "Sea of acid"),
        ("action_sea_acid_gas", "Sea of flammable gas"),
        ("action_sea_mimic", "Sea of Mimicium"),
        ("action_cloud_water", "Rain cloud"),
        ("action_cloud_oil", "Oil cloud"),
        ("action_cloud_blood", "Blood cloud"),
        ("action_cloud_acid", "Acid cloud"),
        ("action_cloud_thunder", "Thundercloud"),
        ("action_electric_charge", "Electric charge"),
        ("action_matter_eater", "Matter eater"),
        ("action_freeze", "Freeze charge"),
        ("action_hitfx_burning_critical_hit", "Critical on burning"),
        (
            "action_hitfx_critical_water",
            "Critical on wet (water) enemies",
        ),
        ("action_hitfx_critical_oil", "Critical on oiled enemies"),
        ("action_hitfx_critical_blood", "Critical on bloody enemies"),
        ("action_hitfx_toxic_charm", "Charm on toxic sludge"),
        ("action_hitfx_explosion_slime", "Explosion on slimy enemies"),
        (
            "action_hitfx_explosion_slime_giga",
            "Giant explosion on slimy enemies",
        ),
        (
            "action_hitfx_explosion_alcohol",
            "Explosion on drunk enemies",
        ),
        (
            "action_hitfx_explosion_alcohol_giga",
            "Giant explosion on drunk enemies",
        ),
        ("action_hitfx_petrify", "Petrify"),
        ("action_rocket_downwards", "Downwards bolt bundle"),
        ("action_rocket_octagon", "Octagonal bolt bundle"),
        ("action_fizzle", "Fizzle"),
        ("action_bounce_explosion", "Explosive bounce"),
        ("action_bounce_spark", "Bubbly bounce"),
        ("action_bounce_laser", "Concentrated light bounce"),
        ("action_bounce_laser_emitter", "Plasma Beam Bounce"),
        ("action_bounce_larpa", "Larpa Bounce"),
        ("action_bounce_small_explosion", "Sparkly bounce"),
        ("action_bounce_lightning", "Lightning bounce"),
        ("action_bounce_hole", "Vacuum bounce"),
        ("action_fireball_ray", "Fireball thrower"),
        ("action_lightning_ray", "Lightning thrower"),
        ("action_tentacle_ray", "Tentacler"),
        ("action_laser_emitter_ray", "Plasma Beam Thrower"),
        ("action_fireball_ray_line", "Two-way fireball thrower"),
        ("action_fireball_ray_enemy", "Personal fireball thrower"),
        ("action_lightning_ray_enemy", "Personal lightning caster"),
        ("action_tentacle_ray_enemy", "Personal tentacler"),
        ("action_gravity_field_enemy", "Personal gravity field"),
        ("action_curse", "Venomous Curse"),
        (
            "action_curse_wither_projectile",
            "Weakening Curse - Projectiles",
        ),
        (
            "action_curse_wither_explosion",
            "Weakening Curse - Explosives",
        ),
        ("action_curse_wither_melee", "Weakening Curse - Melee"),
        (
            "action_curse_wither_electricity",
            "Weakening Curse - Electricity",
        ),
        ("action_orbit_discs", "Sawblade Orbit"),
        ("action_orbit_fireballs", "Fireball Orbit"),
        ("action_orbit_nukes", "Nuke Orbit"),
        ("action_orbit_lasers", "Plasma Beam Orbit"),
        ("action_orbit_larpa", "Orbit Larpa"),
        ("action_chain_shot", "Chain Spell"),
        ("action_arc_electric", "Electric Arc"),
        ("action_arc_fire", "Fire Arc"),
        ("action_arc_gunpowder", "Gunpowder Arc"),
        ("action_arc_poison", "Poison Arc"),
        ("action_crumbling_earth_projectile", "Earthquake shot"),
        ("action_x_ray", "All-seeing eye"),
        ("action_unstable_gunpowder", "Firecrackers"),
        ("action_acid_trail", "Acid trail"),
        ("action_poison_trail", "Poison trail"),
        ("action_oil_trail", "Oil trail"),
        ("action_water_trail", "Water trail"),
        ("action_gunpowder_trail", "Gunpowder trail"),
        ("action_fire_trail", "Fire trail"),
        ("action_burn_trail", "Burning trail"),
        ("action_torch", "Torch"),
        ("action_torch_electric", "Electric Torch"),
        ("action_energy_shield", "Energy shield"),
        ("action_energy_shield_sector", "Energy shield sector"),
        ("action_energy_shield_shot", "Projectile energy shield"),
        ("action_tiny_ghost", "Summon Tiny Ghost"),
        ("action_ocarina_a", "Ocarina - note A"),
        ("action_ocarina_b", "Ocarina - note B"),
        ("action_ocarina_c", "Ocarina - note C"),
        ("action_ocarina_d", "Ocarina - note D"),
        ("action_ocarina_e", "Ocarina - note E"),
        ("action_ocarina_f", "Ocarina - note F"),
        ("action_ocarina_gsharp", "Ocarina - note G#"),
        ("action_ocarina_a2", "Ocarina - note A2"),
        ("action_kantele_a", "Kantele - note A"),
        ("action_kantele_d", "Kantele - note D"),
        ("action_kantele_dis", "Kantele - note D#"),
        ("action_kantele_e", "Kantele - note E"),
        ("action_kantele_g", "Kantele - note G"),
        ("action_random_spell", "Random spell"),
        ("action_random_projectile", "Random projectile spell"),
        ("action_random_modifier", "Random modifier spell"),
        (
            "action_random_static_projectile",
            "Random static projectile spell",
        ),
        ("action_draw_random", "Copy random spell"),
        ("action_draw_random_x3", "Copy random spell thrice"),
        ("action_draw_3_random", "Copy three random spells"),
        ("action_all_nukes", "Spells to nukes"),
        ("action_all_discs", "Spells to giga sawblades"),
        ("action_all_rockets", "Spells to magic missiles"),
        ("action_all_deathcrosses", "Spells to death crosses"),
        ("action_all_blackholes", "Spells to black holes"),
        ("action_all_acid", "Spells to acid"),
        ("action_all_spells", "The end of everything"),
        ("action_summon_portal", "Summon portal"),
        ("action_add_trigger", "Add trigger"),
        ("action_add_timer", "Add timer"),
        ("action_add_death_trigger", "Add expiration trigger"),
        ("action_larpa_chaos", "Chaos larpa"),
        ("action_larpa_downwards", "Downwards larpa"),
        ("action_larpa_upwards", "Upwards larpa"),
        ("action_larpa_chaos_2", "Copy trail"),
        ("action_larpa_death", "Larpa Explosion"),
        ("action_alpha", "Alpha"),
        ("action_gamma", "Gamma"),
        ("action_tau", "Tau"),
        ("action_omega", "Omega"),
        ("action_mu", "Mu"),
        ("action_phi", "Phi"),
        ("action_sigma", "Sigma"),
        ("action_zeta", "Zeta"),
        ("action_divide_2", "Divide by 2"),
        ("action_divide_3", "Divide by 3"),
        ("action_divide_4", "Divide by 4"),
        ("action_divide_10", "Divide by 10"),
        ("action_meteor_rain", "Meteorisade"),
        ("action_worm_rain", "Matosade"),
        ("action_reset", "Wand Refresh"),
        ("action_if_enemy", "Requirement - Enemies"),
        ("action_if_projectile", "Requirement - Projectile Spells"),
        ("action_if_hp", "Requirement - Low Health"),
        ("action_if_half", "Requirement - Every Other"),
        ("action_if_end", "Requirement - Endpoint"),
        ("action_if_else", "Requirement - Otherwise"),
        ("action_colour_red", "Red Glimmer"),
        ("action_colour_orange", "Orange Glimmer"),
        ("action_colour_green", "Green Glimmer"),
        ("action_colour_yellow", "Yellow Glimmer"),
        ("action_colour_purple", "Purple Glimmer"),
        ("action_colour_blue", "Blue Glimmer"),
        ("action_colour_rainbow", "Rainbow Glimmer"),
        ("action_colour_invis", "Invisible Spell"),
        ("action_rainbow_trail", "Rainbow trail"),
        ("action_cessation", "Cessation"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect()
});
