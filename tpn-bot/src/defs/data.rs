use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::PathBuf,
    sync::LazyLock,
};

use anyhow::{Context, Result, anyhow};
use noita_engine_reader::{
    memory::MemoryStorage,
    types::components::{DamageModelComponent, UIIconComponent},
};

use crate::{
    commands::{command, runner::CommandFailure},
    context::cmd::CommandContext,
    fail,
};

/// Read the current seed
#[command(global_gate = 15s, permission = Vip)]
async fn seed(ctx: CommandContext) -> Result<()> {
    match ctx.noita().get_seed().await {
        Some(seed) => ctx.reply(format!("{seed}")).await,
        None => fail!("no data"),
    }
}

/// Read the current death count
#[command(global_gate = 15s, shortcode=deaths)]
async fn death_count(ctx: CommandContext) -> Result<()> {
    let stats = ctx
        .noita()
        .with(|n| Ok(n.read_stats()?))
        .await
        .map_err(|e| {
            tracing::warn!(error=?e, "failed to read stats");
            CommandFailure::new("failed to read game data - is it running?".into())
        })?;
    ctx.reply(stats.global.death_count.to_string()).await
}

/// Read the currently picked up perks. Look ma, streamer wands at home!
#[command(global_gate = 5s)]
async fn perks(ctx: CommandContext, top_n: Option<u32>) -> Result<()> {
    let perks = ctx
        .noita()
        .with(|n| {
            let (entity, _) = n.get_player()?.context("no player")?;
            let p = n.proc().clone();
            let store = n.component_store::<UIIconComponent>()?;
            let mut perks = HashMap::<_, u32>::new();

            for child in entity.children.read(&p)?.read(&p)? {
                if let Some(ui) = store.get(&child.read(&p)?)? {
                    *perks.entry(ui.name.read(&p)?).or_default() += 1;
                };
            }
            let mut perks = perks.into_iter().collect::<Vec<_>>();
            perks.sort_unstable_by_key(|(_, c)| -(*c as i32));
            Ok(perks)
        })
        .await
        .map_err(|e| {
            tracing::warn!(error=?e, "failed to read stats");
            CommandFailure::new("failed to read game data - is it running?".into())
        })?;

    let msg = perks
        .into_iter()
        .take(top_n.unwrap_or(u32::MAX) as _)
        .map(|(perk, count)| {
            format!(
                "{count}x {}",
                perk.trim_start_matches("$perk_").replace("_", " ")
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");

    ctx.reply(msg).await
}

/// Read the current non-1 damage multipliers of the player entity.
#[command(global_gate = 5s, permission = Vip)]
async fn damage_multipliers(ctx: CommandContext) -> Result<()> {
    let dmc = ctx
        .noita()
        .with(|n| {
            let (entity, _) = n.get_player()?.context("no player")?;

            let component = n
                .component_store::<DamageModelComponent>()?
                .get(&entity)?
                .context("no damage model component?")?;

            Ok(component)
        })
        .await
        .map_err(|e| {
            tracing::warn!(error=?e, "failed to read damage model component");
            CommandFailure::new("failed to read game data - is it running?".into())
        })?;

    let m = dmc.damage_multipliers;

    // no compile-time reflection sadge
    let list = vec![
        ("melee", m.melee),
        ("projectile", m.projectile),
        ("explosion", m.explosion),
        ("electricity", m.electricity),
        ("fire", m.fire),
        ("drill", m.drill),
        ("slice", m.slice),
        ("ice", m.ice),
        ("healing", m.healing),
        ("physics_hit", m.physics_hit),
        ("radioactive", m.radioactive),
        ("poison", m.poison),
        ("overeating", m.overeating),
        ("curse", m.curse),
        ("holy", m.holy),
    ];

    let msg = list
        .into_iter()
        .filter(|(_, v)| *v != 1.0)
        .map(|(name, value)| format!("{name}={value:?}",))
        .collect::<Vec<_>>()
        .join(",\n");
    ctx.reply(msg).await
}

static FLAG_PATH: &str = "../noita/steam-compat-data/pfx/drive_c/users/steamuser/AppData/LocalLow/Nolla_Games_Noita/save00/persistent/flags";

/// Checks if the persistent flag was set in the running save.
#[command(global_gate = 5s)]
async fn check_flag(ctx: CommandContext, flag: String) -> Result<()> {
    if flag.contains("/") || flag.contains("..") {
        fail!("nice try bucko");
    }
    if tokio::fs::try_exists(PathBuf::from(FLAG_PATH).join(flag))
        .await
        .is_ok_and(|b| b)
    {
        ctx.reply("flag set".into()).await?;
    } else {
        fail!("flag not set");
    }

    Ok(())
}

async fn read_flags() -> Result<HashSet<String>> {
    let mut set = HashSet::new();
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

/// Shows the amount of completed pillars vs total.
#[command(global_gate = 5s)]
async fn pillar_progress(ctx: CommandContext) -> Result<()> {
    let total = PILLAR_FLAGS.len();
    let done = PILLAR_FLAGS.intersection(&read_flags().await?).count();
    ctx.reply(format!(
        "{:.2}%! ({done}/{total})",
        (done as f32 / total as f32) * 100.0
    ))
    .await
}

/// Shows all the pillar achievements not yet completed in the running save.
#[command(global_gate = 5s)]
async fn pillar_todo(ctx: CommandContext, top_n: Option<u32>) -> Result<()> {
    ctx.reply(
        PILLAR_FLAGS
            .difference(&read_flags().await?)
            .cloned()
            .collect::<BTreeSet<_>>() // sort
            .into_iter()
            .take(top_n.unwrap_or(u32::MAX) as _)
            .map(|flag| PILLAR_FLAG_NAMES[&flag].clone())
            .collect::<Vec<_>>()
            .join(",\n"),
    )
    .await
}

/// Shows all the pillar achievements already completed in the running save.
#[command(global_gate = 5s)]
async fn pillar_done(ctx: CommandContext, top_n: Option<u32>) -> Result<()> {
    ctx.reply(
        PILLAR_FLAGS
            .intersection(&read_flags().await?)
            .cloned()
            .collect::<BTreeSet<_>>() // sort
            .into_iter()
            .take(top_n.unwrap_or(u32::MAX) as _)
            .map(|flag| PILLAR_FLAG_NAMES[&flag].clone())
            .collect::<Vec<_>>()
            .join(",\n"),
    )
    .await
}

static PILLAR_FLAGS: LazyLock<HashSet<String>> =
    LazyLock::new(|| PILLAR_FLAG_NAMES.keys().cloned().collect());

// scripts/biomes/mountain_tree.lua
static PILLAR_FLAG_NAMES: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
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
