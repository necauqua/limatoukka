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

// scripts/biomes/mountain_tree.lua
#[rustfmt::skip]
static PILLAR_FLAGS: LazyLock<HashSet<String>> = LazyLock::new(|| {
    [
        "misc_chest_rain", "misc_util_rain", "misc_worm_rain", "misc_greed_rain", "misc_altar_tablet", "misc_mimic_potion_rain", "misc_monk_bots",
        "misc_sun_effect", "misc_darksun_effect", "secret_tower", "player_status_ghostly", "player_status_ratty", "player_status_funky", "player_status_lukky",
        "player_status_halo", "essence_fire", "essence_water", "essence_laser", "essence_air", "essence_alcohol", "secret_moon", "secret_moon2", "special_mood",
        "secret_dmoon", "dead_mood", "secret_sun_collision", "secret_darksun_collision", "progress_ending0", "progress_ending1_toxic", "progress_ending1_gold",
        "progress_ending2", "progress_newgameplusplus3", "progress_nightmare", "miniboss_dragon", "miniboss_limbs", "miniboss_meat", "miniboss_ghost",
        "miniboss_pit", "miniboss_alchemist", "miniboss_robot", "miniboss_wizard", "miniboss_maggot", "miniboss_fish", "miniboss_islandspirit", "miniboss_threelk",
        "miniboss_gate_monsters", "final_secret_orb3", "miniboss_sky", "boss_centipede", "progress_orb_1", "progress_orb_evil", "progress_orb_all", "progress_pacifist",
        "progress_nogold", "progress_clock", "progress_minit", "progress_nohit", "progress_sun", "progress_darksun", "progress_sunkill", "secret_supernova",
        "secret_greed", "final_secret_orb", "final_secret_orb2", "secret_chest_dark", "secret_chest_light", "card_unlocked_everything", "card_unlocked_divide",
        "secret_fruit", "secret_allessences", "secret_meditation", "secret_buried_eye", "secret_hourglass", "progress_hut_a", "progress_hut_b", "secret_null",
    ]
    .into_iter()
    .map(|s| s.into())
    .collect()
});

/// Shows the amount of completed pillars vs total.
#[command(global_gate = 5s)]
async fn pillar_progress(ctx: CommandContext) -> Result<()> {
    let total = PILLAR_FLAGS.len();
    let done = PILLAR_FLAGS.difference(&read_flags().await?).count();
    ctx.reply(format!(
        "{:.2}%! ({done}/{total})",
        (done as f32 / total as f32) * 100.0
    ))
    .await
}

/// Shows all the pillar achievement flags not yet set in the running save.
#[command(global_gate = 5s)]
async fn pillar_todo(ctx: CommandContext, top_n: Option<u32>) -> Result<()> {
    ctx.reply(
        PILLAR_FLAGS
            .difference(&read_flags().await?)
            .cloned()
            .collect::<BTreeSet<_>>() // sort
            .into_iter()
            .take(top_n.unwrap_or(u32::MAX) as _)
            .collect::<Vec<_>>()
            .join(",\n"),
    )
    .await
}
