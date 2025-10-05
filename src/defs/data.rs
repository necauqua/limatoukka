use std::{cmp::Ordering, collections::BTreeSet};

use std::fmt::Write as _;

use crate::services::noita::NoitaServiceExt;
use crate::{
    commands::{CommandResult, args::RestOfArgs, command, runner::CommandError},
    context::cmd::CommandContext,
    services::noita::{NoitaError, PILLAR_FLAG_NAMES, PILLAR_FLAGS},
};

impl From<NoitaError> for CommandError {
    fn from(value: NoitaError) -> Self {
        match value {
            NoitaError::NoNoita => CommandError::PreconditionFail("noita is not running".into()),
            NoitaError::NoPlayer => CommandError::PreconditionFail("no player entity".into()),
            NoitaError::Internal(e) => CommandError::Internal(e),
        }
    }
}

/// Read the current seed
#[command(global_gate = 15s, NoitaData)]
async fn seed(ctx: CommandContext) -> CommandResult {
    let seed = ctx.noita().get_seed().await?;
    ctx.reply(seed.unwrap_or_else(|| "<unset>".into())).await?;
    Ok(())
}

/// Read the current death count
#[command(global_gate = 15s, shortcode=deaths, NoitaData)]
async fn death_count(ctx: CommandContext) -> CommandResult {
    let count = ctx.noita().get_death_count().await?;
    ctx.reply(count.to_string()).await?;
    Ok(())
}

/// The amount of kicks registered by the game in the current run
#[command(global_gate = 15s, NoitaData)]
async fn kicks(ctx: CommandContext) -> CommandResult {
    let kicks = ctx.noita().get_kick_count().await?;
    ctx.reply(kicks.to_string()).await?;
    Ok(())
}

/// Read the currently picked up perks. Look ma, streamer wands at home!
#[command(global_gate = 5s, NoitaData)]
async fn perks(ctx: CommandContext, top_n: Option<u32>) -> CommandResult {
    let perks = ctx.noita().get_perk_counts().await?;

    let msg = perks
        .into_iter()
        .take(top_n.unwrap_or(u32::MAX) as _)
        .map(|(perk, count)| format!("{count}x {perk}",))
        .collect::<Vec<_>>()
        .join(",\n");

    ctx.reply(msg).await?;
    Ok(())
}

/// Read the current non-1 damage multipliers of the player entity.
#[command(global_gate = 5s, permission = Subscriber, NoitaData)]
async fn damage_multipliers(ctx: CommandContext) -> CommandResult {
    let msg = ctx
        .noita()
        .get_damage_multipliers()
        .await?
        .into_iter()
        .filter(|(_, value)| *value != 1.0)
        .map(|(name, value)| format!("{name}={value:?}",))
        .collect::<Vec<_>>()
        .join(",\n");

    ctx.reply(msg).await?;

    Ok(())
}

/// Checks if the persistent flag was set in the running save.
#[command(global_gate = 5s, NoitaData)]
async fn check_flag(ctx: CommandContext, flag: String) -> CommandResult {
    if ctx.noita().get_flags().await?.contains(&*flag) {
        ctx.reply("flag set".into()).await?;
    } else {
        ctx.fail("flag not set").await?;
    }

    Ok(())
}

/// Shows the amount of completed pillars vs total.
#[command(global_gate = 5s, NoitaData)]
async fn pillar_progress(ctx: CommandContext) -> CommandResult {
    let total = PILLAR_FLAGS.len();
    let done = PILLAR_FLAGS
        .intersection(&ctx.noita().get_flags().await?)
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
            .difference(&ctx.noita().get_flags().await?)
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
            .intersection(&ctx.noita().get_flags().await?)
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

/// Prints the current player position in pixels
#[command(permission = Subscriber, NoitaData)]
async fn player_pos(ctx: CommandContext) -> CommandResult {
    let pos = ctx.noita().get_player_pos().await?;

    let mut msg = format!("x: {:.2}, y: {:.2}", pos.x, pos.y);

    match pos.parallel.cmp(&0) {
        Ordering::Less => writeln!(&mut msg, " (←{})", -pos.parallel),
        Ordering::Greater => writeln!(&mut msg, " (→{})", pos.parallel),
        Ordering::Equal => Ok(()),
    }
    .unwrap();

    match pos.parallel_v.cmp(&0) {
        Ordering::Less => writeln!(&mut msg, " (↑{})", -pos.parallel_v),
        Ordering::Greater => writeln!(&mut msg, " (↓{})", pos.parallel_v),
        Ordering::Equal => Ok(()),
    }
    .unwrap();

    ctx.reply(msg).await?;

    Ok(())
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

    let data = ctx.noita().get_entity_tag_data().await?;

    let tag_indices = tags
        .iter()
        .filter_map(|tag| match tag {
            Some(t) => data.all_tags.iter().position(|t2| t == t2),
            None => None,
        })
        .collect::<Vec<_>>();

    if !msg_tags.is_empty() {
        msg_tags.push(' ');
    }

    let count = data
        .entity_tags
        .into_iter()
        .filter(|b| tag_indices.iter().all(|&i| b[i]))
        .count();

    ctx.reply(format!("{count} {msg_tags}entities are loaded"))
        .await?;

    Ok(())
}

/// Aggregate a top list of tags that mark loaded entities.
#[command(permission = Subscriber, sender_gate = 5s, NoitaData)]
async fn entity_tag_counts(ctx: CommandContext) -> CommandResult {
    let data = ctx.noita().get_entity_tag_data().await?;
    let mut tag_counts = [0; 512];

    // a very tight loop, eh
    // should be very optimizable I hope
    for tags in data.entity_tags {
        for i in 0..512 {
            tag_counts[i] += tags[i] as usize;
        }
    }

    let mut top = data
        .all_tags
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
