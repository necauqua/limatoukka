use anyhow::Result;
use twitch_api::helix::channels::modify_channel_information::*;

use crate::{
    commands::command,
    context::{app::AppContext, cmd::CommandContext},
    storage,
};

pub async fn do_banish(ctx: &AppContext, id: &str, login: &str) -> Result<()> {
    // yeet em
    storage!(ctx, set, "kick:begone:{id}", { 1 })?;
    tracing::info!(id, login, "sent to shadow realm");
    Ok(())
}

/// Instantly banish a user to the shadow realm.
#[command(permission = TwitchStaff)]
async fn banish(ctx: CommandContext, login: String) -> Result<()> {
    let id = ctx.chatter_id(Some(&login)).await?;
    if storage!(ctx, exists, "kick:begone:{id}")? != 0 {
        ctx.reply("already banished".into()).await?;
        return Ok(());
    }
    do_banish(&ctx, &id, &login).await?;
    ctx.reply("whoosh!".to_owned()).await?;
    Ok(())
}

/// Restore users ability to use the bot, bringing them back from the shadow
/// realm regardless of their crimes.
#[command(permission = Moderator)]
async fn unbanish(ctx: CommandContext, login: String) -> Result<()> {
    let id = ctx.chatter_id(Some(&login)).await?;
    if storage!(ctx, del, "kick:begone:{id}")? == 0 {
        ctx.reply("was not banished lmao".into()).await?;
    } else {
        ctx.reply("the deed is done".to_owned()).await?;
    }
    Ok(())
}

/// Check if a user was yeeted into the shadow realm. Per-user 15 second
/// cooldown.
///
/// If _you_ are yeeted, the bot ignores you utterly, so this won't work
/// ¯\\\_(ツ)_/¯.
#[command(sender_gate = 15s)]
async fn banished(ctx: CommandContext, login: String) -> Result<()> {
    let id = ctx.chatter_id(Some(&login)).await?;
    if storage!(ctx, exists, "kick:begone:{id}")? != 0 {
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
