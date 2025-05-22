use std::time::{Duration, SystemTime};

use anyhow::Result;
use humantime_serde::re::humantime;
use twitch_api::helix::channels::modify_channel_information::*;

use crate::{
    commands::{args::RequiredChatter, command},
    context::{app::AppContext, cmd::CommandContext},
    fail, storage,
};

pub async fn do_banish(
    ctx: &AppContext,
    chatter: &RequiredChatter,
    duration: Option<Duration>,
) -> Result<()> {
    if let Some(duration) = duration {
        storage!(
            ctx,
            psetex,
            "kick:begone:{chatter}",
            { duration.as_millis() as _ },
            { 1 }
        )?;
    } else {
        storage!(ctx, set, "kick:begone:{chatter}", { 1 })?;
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
    chatter: RequiredChatter,
    duration: Option<Duration>,
) -> Result<()> {
    if storage!(ctx, exists, "kick:begone:{chatter}")? != 0 {
        ctx.reply("already banished".into()).await?;
        return Ok(());
    }
    do_banish(&ctx, &chatter, duration).await?;
    ctx.reply("whoosh!".to_owned()).await?;
    Ok(())
}

/// Restore users ability to use the bot, bringing them back from the shadow
/// realm regardless of their crimes.
#[command(permission = Moderator)]
async fn unbanish(ctx: CommandContext, chatter: RequiredChatter) -> Result<()> {
    if storage!(ctx, del, "kick:begone:{chatter}")? == 0 {
        ctx.reply("was not there lmao".into()).await?;
    } else {
        tracing::info!(
            id = chatter.id,
            login = chatter.login,
            "pulled out of shadow realm"
        );
        ctx.reply("the deed is done".to_owned()).await?;
    }
    Ok(())
}

/// Check if a user was yeeted into the shadow realm.
///
/// If _you_ are yeeted, the bot ignores you utterly, so this won't work
/// ¯\\\_(ツ)_/¯.
#[command(sender_gate = 15s)]
async fn banished(ctx: CommandContext, chatter: RequiredChatter) -> Result<()> {
    ctx.reply(match storage!(ctx, pexpiretime, "kick:begone:{chatter}")? {
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
    })
    .await
}

/// Set the stream title, common moderation command, nothing special here.
#[command(permission = Moderator, global_gate = 5s, hidden)]
async fn set_title(ctx: CommandContext, title: String) -> Result<()> {
    ctx.twitch()
        .caster_call(async |t| {
            let request = ModifyChannelInformationRequest::broadcaster_id(t.caster_id);
            let mut body = ModifyChannelInformationBody::new();
            body.title(&title);

            let response: ModifyChannelInformation =
                t.helix.req_patch(request, body, &t.token).await?.data;

            Ok(response)
        })
        .await?;

    Ok(())
}

/// Stops and then starts the stream again, useful for when Twitch kills the
/// stream due to the 48h limit and OBS does not realize.
#[command(permission=Moderator, global_gate = 5m)]
async fn obs_restart_stream() -> Result<()> {
    AppContext::just("obs-restart-stream").await
}

/// An untested script that starts OBS and then starts the stream if OBS died.
/// Does nothing if the OBS process is running.
#[command(permission=Moderator, global_gate = 5m)]
async fn obs_revive(ctx: CommandContext) -> Result<()> {
    if !AppContext::just_bool("obs-revive").await? {
        fail!("OBS is running");
    }
    ctx.reply("OBS was not running, started it up".into()).await
}
