use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap},
};

use anyhow::{Context, Result, bail};
use noita_engine_reader::{
    memory::MemoryStorage,
    types::{
        Bitset512, Vec2,
        components::{DamageModelComponent, UIIconComponent},
    },
};
use serde_json::json;

use crate::{
    commands::{CommandResult, args::RestOfArgs, command, runner::CommandError},
    context::cmd::CommandContext,
    fail,
    services::noita::{ACTION_FLAGS, ACTION_NAMES, PILLAR_FLAG_NAMES, PILLAR_FLAGS},
};

fn data_error(thing: &str) -> impl Fn(anyhow::Error) -> CommandError {
    move |e| {
        tracing::warn!(error=?e, "failed to read {thing}");
        CommandError::PreconditionFail(format!("failed to read {thing} - is the game running?"))
    }
}

/// Read the current seed
#[command(global_gate = 15s, permission = Vip, NoitaData)]
async fn seed(ctx: CommandContext) -> CommandResult {
    let seed = ctx
        .noita()
        .with(|n| n.read_seed()?.context("no seed"))
        .await
        .map_err(data_error("seed"))?;
    ctx.reply(format!("{seed}")).await?;
    Ok(())
}

/// Read the current death count
#[command(global_gate = 15s, shortcode=deaths, NoitaData)]
async fn death_count(ctx: CommandContext) -> CommandResult {
    let stats = ctx
        .noita()
        .with(|n| Ok(n.read_stats()?))
        .await
        .map_err(data_error("stats"))?
        .global
        .death_count;
    ctx.reply(stats.to_string()).await?;
    Ok(())
}

/// The amount of kicks registered by the game in the current run
#[command(global_gate = 15s, NoitaData)]
async fn kicks(ctx: CommandContext) -> CommandResult {
    let kicks = ctx
        .noita()
        .with(|n| Ok(n.read_config_player_stats()?))
        .await
        .map_err(data_error("stats"))?
        .stats
        .kicks;
    ctx.reply(kicks.to_string()).await?;
    Ok(())
}

/// Read the currently picked up perks. Look ma, streamer wands at home!
#[command(global_gate = 5s, NoitaData)]
async fn perks(ctx: CommandContext, top_n: Option<u32>) -> CommandResult {
    let perks = ctx
        .noita()
        .with(|n| {
            let Some((entity, _)) = n.get_player()? else {
                bail!("no player entity");
            };
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

    ctx.reply(msg).await?;
    Ok(())
}

/// Read the current non-1 damage multipliers of the player entity.
#[command(global_gate = 5s, permission = Vip, NoitaData)]
async fn damage_multipliers(ctx: CommandContext) -> CommandResult {
    let dmc = ctx
        .noita()
        .with(|n| {
            let Some((entity, _)) = n.get_player()? else {
                bail!("no player entity");
            };

            let component = n
                .component_store::<DamageModelComponent>()?
                .get(&entity)?
                .context("no damage model component?")?;

            Ok(component)
        })
        .await
        .map_err(data_error("damage model component"))?;

    let m = dmc.damage_multipliers;

    // using serde for compile-time reflection here lmao
    let json = serde_json::to_value(&m)?;
    let msg = json
        .as_object()
        .unwrap()
        .into_iter()
        .filter(|(_, v)| **v != json!(1.0))
        .map(|(name, value)| format!("{name}={value:?}",))
        .collect::<Vec<_>>()
        .join(",\n");
    ctx.reply(msg).await?;
    Ok(())
}

/// Checks if the persistent flag was set in the running save.
#[command(global_gate = 5s, NoitaData)]
async fn check_flag(ctx: CommandContext, flag: String) -> CommandResult {
    if ctx.noita().has_flag(&flag).await? {
        ctx.reply("flag set".into()).await?;
    } else {
        fail!("flag not set");
    }

    Ok(())
}

/// Shows the amount of completed pillars vs total.
#[command(global_gate = 5s, NoitaData)]
async fn pillar_progress(ctx: CommandContext) -> CommandResult {
    let total = PILLAR_FLAGS.len();
    let done = PILLAR_FLAGS
        .intersection(&ctx.noita().read_flags().await?)
        .count();
    ctx.reply(format!(
        "{:.2}%! ({done}/{total})",
        (done as f32 / total as f32) * 100.0
    ))
    .await?;
    Ok(())
}

/// Shows all the pillar achievements not yet completed in the running save.
#[command(global_gate = 5s, NoitaData)]
async fn pillar_todo(ctx: CommandContext, top_n: Option<u32>) -> CommandResult {
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
    .await?;
    Ok(())
}

/// Shows all the pillar achievements already completed in the running save.
#[command(global_gate = 5s, NoitaData)]
async fn pillars_done(ctx: CommandContext, top_n: Option<u32>) -> CommandResult {
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
    .await?;
    Ok(())
}

/// Shows the amount of unique spells ever cast vs total.
#[command(global_gate = 5s, NoitaData)]
async fn spell_progress(ctx: CommandContext) -> CommandResult {
    let total = ACTION_NAMES.len();
    let done = ACTION_FLAGS
        .intersection(&ctx.noita().read_flags().await?)
        .count();
    ctx.reply(format!(
        "{:.2}%! ({done}/{total})",
        (done as f32 / total as f32) * 100.0
    ))
    .await?;
    Ok(())
}

/// Shows all the spells that were never cast in the running save.
#[command(global_gate = 5s, NoitaData)]
async fn spell_todo(ctx: CommandContext, top_n: Option<u32>) -> CommandResult {
    ctx.reply(
        ACTION_FLAGS
            .difference(&ctx.noita().read_flags().await?)
            .cloned()
            .collect::<BTreeSet<_>>() // sort
            .into_iter()
            .take(top_n.unwrap_or(u32::MAX) as _)
            .map(|flag| ACTION_NAMES[&flag].clone())
            .collect::<Vec<_>>()
            .join(",\n"),
    )
    .await?;
    Ok(())
}

/// Shows all the spells that were already cast in the running save.
#[command(global_gate = 5s, NoitaData)]
async fn spells_done(ctx: CommandContext, top_n: Option<u32>) -> CommandResult {
    ctx.reply(
        ACTION_FLAGS
            .intersection(&ctx.noita().read_flags().await?)
            .cloned()
            .collect::<BTreeSet<_>>() // sort
            .into_iter()
            .take(top_n.unwrap_or(u32::MAX) as _)
            .map(|flag| ACTION_NAMES[&flag].clone())
            .collect::<Vec<_>>()
            .join(",\n"),
    )
    .await?;
    Ok(())
}

/// Prints the current player position in pixels
#[command(permission = Moderator, NoitaData)]
async fn player_pos(ctx: CommandContext) -> CommandResult {
    let (e, _) = ctx
        .noita()
        .with(|n| n.get_player()?.context("no player"))
        .await
        .map_err(data_error("player pos"))?;

    let ng_count = ctx
        .noita()
        .with(|n| n.read_ng_plus().context("reading NG+ count"))
        .await
        .map_err(data_error("NG+ count"))?;

    let Vec2 { x, y } = e.transform.pos;

    let pw = if ng_count == 0 {
        x / 512.0 / 70.0 // NG is 70 chunks
    } else {
        x / 512.0 / 64.0 // and NG+ is 64
    }
    .round() as i32;

    match pw.cmp(&0) {
        Ordering::Equal => ctx.reply(format!("x: {x:.2}, y: {y:.2}")).await?,
        Ordering::Less => {
            ctx.reply(format!("x: {x:.2}, y: {y:.2} (←{})", -pw))
                .await?
        }
        Ordering::Greater => ctx.reply(format!("x: {x:.2}, y: {y:.2} (→{pw})")).await?,
    };
    Ok(())
}

async fn entity_tags(ctx: &CommandContext) -> Result<Vec<Bitset512>> {
    ctx.noita()
        .with(move |n| {
            let manager = n.read_entity_manager().context("no entity manager")?;
            let entities = manager.entities.read(n.proc())?;

            let mut tags = Vec::with_capacity(entities.len());
            for entity in entities {
                if !entity.is_null() {
                    tags.push(entity.read(n.proc())?.tags);
                }
            }

            Ok(tags)
        })
        .await
}

/// Count the amount of loaded entities.
///
/// Can be filtered down by tags, for example `entity-count:gold_nugget~`.
#[command(permission = Subscriber, sender_gate = 5s, NoitaData)]
async fn entity_count(ctx: CommandContext, tags: RestOfArgs) -> CommandResult {
    let tags = tags.get(&ctx).await?;

    // cringe lmao
    let mut msg_tags = tags
        .iter()
        .filter_map(|s| s.as_deref())
        .collect::<Vec<_>>()
        .join(",");

    let tag_indices = ctx
        .noita()
        .with(move |n| {
            let mut indices = Vec::with_capacity(tags.len());
            for tag in tags.iter() {
                let Some(tag) = tag else {
                    continue;
                };
                if let Some(index) = n.get_entity_tag_index(tag)? {
                    indices.push(index);
                }
                // } else {
                //     fail!("tag {tag} not found (was never loaded by game)");
                // }
            }
            Ok(indices)
        })
        .await
        .map_err(data_error("entity count"))?;

    let entities_tags = entity_tags(&ctx).await.map_err(data_error("entity tags"))?;

    if !msg_tags.is_empty() {
        msg_tags.push(' ');
    }

    let count = entities_tags
        .into_iter()
        .filter(|b| tag_indices.iter().all(|&i| b[i]))
        .count();

    ctx.reply(format!("{count} {msg_tags}entities are loaded"))
        .await?;

    Ok(())
}

/// Aggregate a top list of tags that mark loaded entities.
#[command(permission = Vip, sender_gate = 5s, NoitaData)]
async fn entity_tag_counts(ctx: CommandContext) -> CommandResult {
    let entity_tags = entity_tags(&ctx).await.map_err(data_error("entity tags"))?;
    let mut tag_counts = [0; 512];

    // a very tight loop, eh
    // should be very optimizable I hope
    for tags in entity_tags {
        for i in 0..512 {
            tag_counts[i] += tags[i] as usize;
        }
    }

    let tags = ctx
        .noita()
        .with(move |n| Ok(n.read_entity_tag_manager()?.tags.read_storage(n.proc())?))
        .await
        .map_err(data_error("entity tag names"))?;

    let mut top = tags
        .into_iter()
        .zip(tag_counts.into_iter())
        .filter(|(_, count)| *count > 0)
        .collect::<Vec<_>>();

    top.sort_by_key(|&(_, count)| -(count as isize));

    ctx.reply(
        top.into_iter()
            .map(|(name, count)| format!("{name}: {count}"))
            .collect::<Vec<_>>()
            .join(";\n"),
    )
    .await?;
    Ok(())
}
