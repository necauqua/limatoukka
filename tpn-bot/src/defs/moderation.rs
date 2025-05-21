use anyhow::Result;
use twitch_api::helix::channels::modify_channel_information::*;

use crate::{
    commands::{args::RequiredChatter, command},
    context::{app::AppContext, cmd::CommandContext},
    fail, storage,
};

pub async fn do_banish(ctx: &AppContext, chatter: &RequiredChatter) -> Result<()> {
    // yeet em
    storage!(ctx, set, "kick:begone:{chatter}", { 1 })?;
    tracing::info!(
        id = chatter.id,
        login = chatter.login,
        "sent to shadow realm"
    );
    Ok(())
}

/// Instantly banish a user to the shadow realm.
#[command(permission = TwitchStaff)]
async fn banish(ctx: CommandContext, chatter: RequiredChatter) -> Result<()> {
    if storage!(ctx, exists, "kick:begone:{chatter}")? != 0 {
        ctx.reply("already banished".into()).await?;
        return Ok(());
    }
    do_banish(&ctx, &chatter).await?;
    ctx.reply("whoosh!".to_owned()).await?;
    Ok(())
}

/// Restore users ability to use the bot, bringing them back from the shadow
/// realm regardless of their crimes.
#[command(permission = Moderator)]
async fn unbanish(ctx: CommandContext, chatter: RequiredChatter) -> Result<()> {
    if storage!(ctx, del, "kick:begone:{chatter}")? == 0 {
        ctx.reply("was not banished lmao".into()).await?;
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
    if storage!(ctx, exists, "kick:begone:{chatter}")? != 0 {
        ctx.reply("In the shadow realm xdd".into()).await?;
    } else {
        ctx.reply("They're good".into()).await?;
    }

    Ok(())
}

/// Set the stream title, common moderation command, nothing special here.
#[command(permission = Moderator, global_gate = 5s, hidden)]
async fn set_title(ctx: CommandContext, title: String) -> Result<()> {
    let title = &*title;

    ctx.twitch()
        .caster_call(move |t| async move {
            let request = ModifyChannelInformationRequest::broadcaster_id(t.caster_id);
            let mut body = ModifyChannelInformationBody::new();
            body.title(title);

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
