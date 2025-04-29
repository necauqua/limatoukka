use std::time::Duration;

use crate::{
    commands::{args::HoldTime, command},
    context::{app::AppContext, cmd::CommandContext},
    fail,
    services::messaging::PermissionLevel,
};
use anyhow::Result;
use maud::html;
use rustis::commands::{GenericCommands, StringCommands};
use tokio::time::sleep;

use super::chatter_id;

/// Respond with "pong!".
///
/// I heard that scarcity creates value, so getting a pong is very _cool_ and
/// _pog_, because only one person can get it in an hour.
#[command(global_gate = 1h)]
async fn ping(ctx: CommandContext) -> Result<()> {
    ctx.reply("pong!".into()).await
}

/// A building block for basic static text commands.
///
/// This just makes the bot print the given text, but non-moderators can only call it through global macros.
///
/// So you can call a global macro `discord~` which will resolve to `echo:"discord link etc"~` and print it.
#[command(sender_gate = 5s)]
async fn echo(ctx: CommandContext, text: String) -> Result<()> {
    if !ctx.in_global_macro && ctx.message().sender.level < PermissionLevel::Moderator {
        fail!("only works from inside of global macros")
    }
    ctx.send(text).await?;
    Ok(())
}

/// Respond with the last error message for user.
///
/// Last error message is set when you run invalid commands, or if the command
/// execution managed to crash somehow. In the latter case, you'll be given the
/// message id - please send it to me to look at logs and fix the issue.
#[command(sender_gate = 3s)]
async fn last_error(ctx: CommandContext, login: Option<String>) -> Result<()> {
    let id = chatter_id(&ctx, login.as_deref()).await?;
    let status: Option<String> = ctx.storage().get(format!("last-error:{id}")).await?;
    if let Some(status) = status {
        ctx.reply(status).await?;
    } else {
        ctx.reply("No errors in your last message".into()).await?;
    }
    Ok(())
}

/// Get the current seed (surely you are not planning to look at noitool, right?)
///
/// At the moment only replies with actual seed to moderators 🤷
#[command(global_gate = 15s)]
async fn seed(ctx: CommandContext) -> Result<()> {
    if ctx.message().sender.level < PermissionLevel::Moderator {
        ctx.reply("nah man stop checking the seed like that at the beginning".into())
            .await
    } else {
        ctx.reply(match ctx.noita().get_seed().await {
            Some(seed) => format!("{seed}"),
            None => "no data".into(),
        })
        .await
    }
}

/// Get the current death count
#[command(global_gate = 15s, shortcode=deaths)]
async fn death_count(ctx: CommandContext) -> Result<()> {
    ctx.reply(match ctx.noita().get_death_count().await {
        Some(count) => count.to_string(),
        None => "Couldn't read the death count - is the game running?".into(),
    })
    .await
}

/// Sometimes the capture dies (but the game is fine) because of my brittle scripts.
///
/// Try running this first before doing a full restart etc etc.
#[command(global_gate = 30s)]
async fn fix_obs_capture() -> Result<()> {
    AppContext::fix_obs_capture().await
}

/// The sound setup is the most brittle jank thing actually, and dies most often.
///
/// Try running this first before doing a full restart etc etc.
#[command(global_gate = 30s)]
async fn fix_obs_sound() -> Result<()> {
    AppContext::fix_obs_sound().await
}

/// Wait for a specified duration milliseconds.
///
/// Very useful for multi-command messages.
#[command(shortcode=w, no_wall)]
async fn wait(ctx: CommandContext, duration: HoldTime) -> Result<()> {
    let duration = duration.get();
    tracing::debug!(?duration, "waiting");

    let entry = ctx.status_wall().allocate().await;
    let inner_ctx = ctx.clone();
    let wall_task = tokio::spawn(async move {
        let name = &inner_ctx.message().sender.name;
        let nesting = inner_ctx.nesting_str();
        for i in (1..=duration.as_secs()).rev() {
            entry
                .set(html! {
                    span style="color: #E38AF0" { (name) } ": wait:" (i) "s " (nesting)
                })
                .await;
            sleep(Duration::from_secs(1)).await;
        }
    });

    ctx.holds()
        .interruptible(sleep(duration))
        .await
        .inspect_err(|_| wall_task.abort())?;

    Ok(())
}

/// Complete all current holds immediately.
///
/// "Holds" here are referring to all "non-instantaneous" commands that are
/// currently being executed **right now** - so movement, `hold~` and `wait~`.
///
/// This is kind of a niche thing, most likely you need `interrupt~`.
#[command]
async fn r#break(ctx: CommandContext) -> Result<()> {
    ctx.holds().send_break().await;
    Ok(())
}

/// Stop running all current commands.
/// This is similar to `break~`, except the commands following the holds that
/// get completed do not run.
#[command]
async fn interrupt(ctx: CommandContext) -> Result<()> {
    ctx.holds().send_interrupt().await;
    Ok(())
}

/// Set a bot flag.
///
/// Two flags that currently do things are `full-stop` and
/// `no-restarts` - first one disables processing any commands from non-mods,
/// and the latter one disables the game restarting on player death.
///
/// If the flag argument is prefixed with `-` it is removed if it was set
/// previously.
#[command(permission = Moderator)]
async fn flag(ctx: CommandContext, flag: String) -> Result<()> {
    if let Some(flag) = flag.strip_prefix("-") {
        ctx.storage().del(format!("flags:{flag}")).await?;
    } else {
        ctx.storage().set(format!("flags:{flag}"), "1").await?;
    }
    Ok(())
}
