use std::time::Duration;

use crate::commands::{
    args::AtMost,
    command,
    context::{AppContext, CommandContext},
};
use anyhow::Result;
use rustis::commands::StringCommands;

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
#[command(global_gate = 5s)]
async fn last_error(ctx: CommandContext) -> Result<()> {
    let status: Option<String> = ctx.storage.get(ctx.sender_key("last-error")).await?;
    if let Some(status) = status {
        ctx.reply(status).await?;
    } else {
        ctx.reply("No errors yet".into()).await?;
    }

    Ok(())
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

/// (Re)start the game immediately.
#[command(permission = Verified, global_gate = 2m)]
async fn restart() -> Result<()> {
    AppContext::restart().await
}

/// Reset the game immediately.
#[command(permission = TwitchStaff, global_gate = 2m)]
async fn reset() -> Result<()> {
    AppContext::reset().await
}
