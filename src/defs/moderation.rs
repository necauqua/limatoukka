use std::time::{Duration, SystemTime};

use humantime_serde::re::humantime;
use rustis::commands::{GenericCommands, StringCommands};

use crate::{
    commands::{
        CommandResult,
        args::{Chatter, Required},
        command,
    },
    context::{app::AppContext, cmd::CommandContext},
    fail,
    services::twitch::TwitchServiceExt,
};

pub async fn do_banish(
    ctx: &AppContext,
    chatter: &Required<Chatter>,
    duration: Option<Duration>,
) -> CommandResult {
    if let Some(duration) = duration {
        ctx.storage_old()
            .psetex(
                format!("kick:begone:{chatter}"),
                duration.as_millis() as _,
                1,
            )
            .await?;
    } else {
        ctx.storage_old()
            .set(format!("kick:begone:{chatter}"), 1)
            .await?;
    }
    tracing::info!(
        id = chatter.id,
        login = chatter.login,
        ?duration,
        "sent to shadow realm"
    );
    Ok(())
}

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
        ctx.reply("🤨".into()).await?;
        return Ok(());
    }
    if ctx.bot_id() == Some(&*chatter.id) {
        fail!("lol. lmao.")
    }
    if ctx
        .storage_old()
        .exists(format!("kick:begone:{chatter}"))
        .await?
        != 0
    {
        ctx.reply("already banished".into()).await?;
        return Ok(());
    }
    do_banish(&ctx, &chatter, duration).await?;
    ctx.reply("whoosh!".into()).await?;
    Ok(())
}

/// Restore users ability to use the bot, bringing them back from the shadow
/// realm regardless of their crimes.
#[command(permission = Moderator)]
async fn unbanish(ctx: CommandContext, chatter: Required<Chatter>) -> CommandResult {
    if ctx
        .storage_old()
        .del(format!("kick:begone:{chatter}"))
        .await?
        == 0
    {
        ctx.reply("was not there lmao".into()).await?;
        return Ok(());
    }
    tracing::info!(
        id = chatter.id,
        login = chatter.login,
        "pulled out of shadow realm"
    );
    ctx.reply("the deed is done".into()).await?;
    Ok(())
}

/// Check if a user was yeeted into the shadow realm.
///
/// If _you_ are yeeted, the bot ignores you utterly, so this won't work
/// ¯\\\_(ツ)_/¯.
#[command(sender_gate = 15s)]
async fn banished(ctx: CommandContext, chatter: Required<Chatter>) -> CommandResult {
    ctx.reply(
        match ctx
            .storage_old()
            .pexpiretime(format!("kick:begone:{chatter}"))
            .await?
        {
            -2 => "They're good".into(),
            -1 => "In the shadow realm xdd".into(),
            time => {
                let now = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_millis();
                let left = Duration::from_millis((time - now as i64).unsigned_abs());
                let left = humantime::format_duration(left);
                format!("still {left} to go welp")
            }
        },
    )
    .await?;
    Ok(())
}

/// Set the stream title, common moderation command, nothing special here.
#[command(permission = Moderator, global_gate = 5s)]
async fn set_title(ctx: CommandContext, title: String) -> CommandResult {
    ctx.twitch().set_stream_title(&title).await?;
    Ok(())
}
