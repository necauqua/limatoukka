use anyhow::Result;
use rustis::commands::{GenericCommands, StringCommands};

use crate::{
    commands::command,
    context::{app::AppContext, cmd::CommandContext},
};

pub async fn do_banish(ctx: &AppContext, id: &str, login: &str) -> Result<()> {
    // yeet em
    ctx.storage().set(format!("kick:begone:{id}"), "1").await?;
    tracing::info!(id, login, "sent to shadow realm");
    Ok(())
}

/// Instantly banish a user to the shadow realm.
#[command(permission = TwitchStaff)]
async fn banish(ctx: CommandContext, login: String) -> Result<()> {
    let id = ctx.chatter_id(Some(&login)).await?;
    if ctx.storage().exists(format!("kick:begone:{id}")).await? != 0 {
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
    if ctx.storage().del(format!("kick:begone:{id}")).await? == 0 {
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
    let begone = ctx.storage().exists(format!("kick:begone:{id}")).await?;
    if begone != 0 {
        ctx.reply("In the shadow realm xdd".into()).await?;
    } else {
        ctx.reply("They're good".into()).await?;
    }

    Ok(())
}
