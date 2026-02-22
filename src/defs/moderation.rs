use std::time::Duration;

use humantime_serde::re::humantime;

use crate::{
    commands::{
        CommandResult,
        args::{Chatter, Required},
        command,
    },
    context::{app::InterruptKind, cmd::CommandContext},
    services::{
        banishes::{BanishServiceExt, BanishStatus},
        twitch::TwitchServiceExt,
    },
};

/// Instantly banish a user to the shadow realm.
///
/// Can be temporary if a duration is provided.
#[command(permission = TwitchStaff)]
async fn banish(
    ctx: CommandContext,
    chatter: Required<Chatter>,
    duration: Option<Duration>,
) -> CommandResult {
    if ctx.caster_id() == Some(&*chatter.id) {
        ctx.fail("🤨").await?;
    }
    if ctx.bot_id() == Some(&*chatter.id) {
        ctx.fail("lol. lmao.").await?;
    }
    if !ctx.banishes().banish(&chatter.id, duration).await? {
        ctx.fail("already banished").await?;
    }
    ctx.reply("whoosh!".into()).await?;
    Ok(())
}

/// Restore users ability to use the bot, bringing them back from the shadow
/// realm regardless of their crimes.
#[command(permission = Moderator)]
async fn unbanish(ctx: CommandContext, chatter: Required<Chatter>) -> CommandResult {
    if !ctx.banishes().unbanish(&chatter.id).await? {
        ctx.fail("was not there lmao").await?;
    }
    ctx.reply("the deed is done".into()).await?;
    Ok(())
}

/// Check if a user was yeeted into the shadow realm.
///
/// If _you_ are yeeted, the bot ignores you utterly, so this won't work
/// ¯\\\_(ツ)_/¯.
#[command(sender_gate = 15s)]
async fn banished(ctx: CommandContext, chatter: Required<Chatter>) -> CommandResult {
    ctx.reply(match ctx.banishes().status(&chatter.id).await? {
        BanishStatus::Good => "They're good".into(),
        BanishStatus::Banished => "In the shadow realm xdd".into(),
        BanishStatus::Temporary(time) => {
            format!("still {} to go welp", humantime::format_duration(time))
        }
    })
    .await?;
    Ok(())
}

/// Set the stream title, common moderation command, nothing special here.
#[command(permission = Moderator, global_gate = 5s)]
async fn set_title(ctx: CommandContext, title: String) -> CommandResult {
    ctx.twitch().set_stream_title(&title).await?;
    Ok(())
}

/// Kills the bot ¯\_(ツ)_/¯
#[command(permission = Caster)]
async fn die(ctx: CommandContext) -> CommandResult {
    ctx.interrupt(None, InterruptKind::Interrupt);
    ctx.quit();
    Ok(())
}
