use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result};
use noita_engine_reader::{
    memory::MemoryStorage,
    types::components::{DamageModelComponent, UIIconComponent},
};

use crate::{
    commands::{command, runner::CommandFailure},
    context::cmd::CommandContext,
    fail,
    services::noita::{PILLAR_FLAG_NAMES, PILLAR_FLAGS},
};

fn data_error(thing: &str) -> impl Fn(anyhow::Error) -> CommandFailure {
    move |e| {
        tracing::warn!(error=?e, "failed to read {thing}");
        CommandFailure::new(format!("failed to read {thing} - is the game running?"))
    }
}

/// Read the current seed
#[command(global_gate = 15s, permission = Vip)]
async fn seed(ctx: CommandContext) -> Result<()> {
    match ctx
        .noita()
        .with(|n| Ok(n.read_seed()?))
        .await
        .map_err(data_error("seed"))?
    {
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
        .map_err(data_error("stats"))?;
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
        .map_err(data_error("perks"))?;

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
        .map_err(data_error("damage model component"))?;

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

/// Checks if the persistent flag was set in the running save.
#[command(global_gate = 5s)]
async fn check_flag(ctx: CommandContext, flag: String) -> Result<()> {
    if flag.contains("/") || flag.contains("..") {
        fail!("nice try bucko");
    }
    if ctx.noita().has_flag(&flag).await? {
        ctx.reply("flag set".into()).await?;
    } else {
        fail!("flag not set");
    }

    Ok(())
}

/// Shows the amount of completed pillars vs total.
#[command(global_gate = 5s)]
async fn pillar_progress(ctx: CommandContext) -> Result<()> {
    let total = PILLAR_FLAGS.len();
    let done = PILLAR_FLAGS
        .intersection(&ctx.noita().read_flags().await?)
        .count();
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
            .difference(&ctx.noita().read_flags().await?)
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
            .intersection(&ctx.noita().read_flags().await?)
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
