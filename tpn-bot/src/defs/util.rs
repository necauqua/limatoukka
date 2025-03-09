use std::time::Duration;

use crate::commands::{args::AtMost, command, context::CommandContext};
use anyhow::Result;
use rustis::commands::StringCommands;

/// Respond with "pong!". Global cooldown 42s.
#[command]
async fn ping(ctx: CommandContext) -> Result<()> {
    if !ctx.command_gate(Duration::from_secs(42)).await? {
        return Ok(());
    }
    ctx.reply("pong!".into()).await?;
    Ok(())
}

/// Respond with the last error message for user. Global cooldown 5s.
///
/// Last error message is set when you run invalid commands, or if the command
/// execution managed to crash somehow. In the latter case, you'll be given the
/// message id - please send it to me to look at logs and fix the issue.
#[command]
async fn last_error(ctx: CommandContext) -> Result<()> {
    if !ctx.command_gate(Duration::from_secs(5)).await? {
        return Ok(());
    }

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
