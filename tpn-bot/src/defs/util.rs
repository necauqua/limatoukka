use std::time::Duration;

use crate::commands::{
    args::AtMost,
    command,
    context::{AppContext, CommandContext},
};
use anyhow::Result;
use rustis::commands::{GenericCommands, StringCommands};

/// Respond with "pong!".
#[command(global_gate = 42s)]
async fn ping(ctx: CommandContext) -> Result<()> {
    ctx.reply("pong!".into()).await
}

/// Respond with the last error message for user. Global cooldown 5s.
///
/// Last error message is set when you run invalid commands, or if the command
/// execution managed to crash somehow. In the latter case, you'll be given the
/// message id - please send it to me to look at logs and fix the issue.
#[command(global_gate = 15s)]
async fn last_error(ctx: CommandContext) -> Result<()> {
    let status: Option<String> = ctx.storage.get(ctx.sender_key("last-error")).await?;
    if let Some(status) = status {
        ctx.reply(status).await?;
    } else {
        ctx.reply("No errors yet".into()).await?;
    }

    Ok(())
}

/// Get the current seed (surely you are not planning to look at noitool, right?)
#[command(global_gate = 15s)]
async fn seed(ctx: CommandContext) -> Result<()> {
    ctx.reply(match ctx.noita.get_seed().await {
        Some(seed) => format!("{seed}"),
        None => "no data".into(),
    })
    .await
}

/// Wait for a duration of 1-500ms, defaulting to 500.
///
/// Very useful for multi-command messages.
#[command]
async fn wait(_ctx: CommandContext, millis: Option<AtMost<500>>) -> Result<()> {
    let duration = Duration::from_millis(millis.map_or(500, |m| m.get() as _));
    tracing::debug!(?duration, "waiting");
    tokio::time::sleep(duration).await;
    Ok(())
}

/// Pause the game.
///
/// This actually just presses the <kbd>Esc</kbd> key.
///
/// And yes, chatters will be able to move the mouse around and click stuff, so
/// this command is kinda annoying without `full-stop~` as they could mess up
/// the settings or start a different gamemode.
#[command(permission = Moderator)]
async fn pause(ctx: CommandContext) -> Result<()> {
    ctx.xdo.key("Escape").await
}

/// Stop processing commands from everyone below the moderator level.
#[command(permission = Moderator)]
async fn full_stop(ctx: CommandContext) -> Result<()> {
    Ok(ctx.storage.set("full-stop", "1").await?)
}

/// Undo the effect of `full-stop~`.
#[command(permission = Moderator)]
async fn full_ahead(ctx: CommandContext) -> Result<()> {
    ctx.storage.del("full-stop").await?;
    Ok(())
}

/// (Re)start the game immediately. This is the same as a successful
/// `vote-restart~`, but instant.
#[command(permission = Verified, global_gate = 2m)]
async fn restart() -> Result<()> {
    AppContext::restart().await
}

/// Reset the game (deleting the current world) immediately. This is the same
/// as a successful `vote-reset~`, but instant.
#[command(permission = Moderator, global_gate = 2m)]
async fn reset() -> Result<()> {
    AppContext::reset().await
}
