use std::time::Duration;

use crate::{
    commands::{
        args::{Chatter, HoldTime},
        command,
    },
    context::{app::AppContext, cmd::CommandContext},
    fail,
    services::messaging::PermissionLevel,
};
use anyhow::{Result, bail};
use humantime_serde::re::humantime;
use maud::html;
use rustis::commands::{GenericCommands, StringCommands};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::time::sleep;

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
async fn last_error(ctx: CommandContext, chatter: Chatter) -> Result<()> {
    ctx.reply(
        ctx.storage()
            .get::<_, Option<_>>(format!("last-error:{chatter}"))
            .await?
            .unwrap_or_else(|| "No errors in your last message".into()),
    )
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
    tracing::debug!(
        duration.ms = duration.as_millis(),
        "waiting for {}",
        humantime::format_duration(duration)
    );

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

    ctx.interruptible(sleep(duration))
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
    ctx.break_holds();
    Ok(())
}

/// Stop running all current commands.
/// This is similar to `break~`, except the commands following the holds that
/// get completed do not run.
#[command]
async fn interrupt(ctx: CommandContext) -> Result<()> {
    ctx.interrupt_holds();
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

/// Gets the current CPU usage of the game.
#[command(global_gate = 5s, permission = Subscriber, shortcode = cpu)]
async fn noita_cpu_usage(ctx: CommandContext) -> Result<()> {
    let pid = Pid::from(ctx.noita().with(|n| Ok(n.proc().pid())).await? as usize);

    let usage = tokio::task::spawn_blocking(move || {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_cpu(),
        );
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_cpu(),
        );
        system.process(pid).map(|p| p.cpu_usage())
    })
    .await;

    let Some(usage) = usage.ok().flatten() else {
        bail!("failed to get CPU usage, noita.exe not running?");
    };

    ctx.reply(if usage > 100.0 {
        format!("CPU usage: {usage:.2}% (100% is 1 core)")
    } else {
        format!("CPU usage: {usage:.2}%")
    })
    .await?;

    Ok(())
}
