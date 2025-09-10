use std::{fmt::Write as _, time::Duration};

use crate::{
    commands::{
        CommandResult, CommandTag,
        args::{Chatter, HoldTime, Required},
        command,
    },
    context::{app::InterruptKind, cmd::CommandContext},
    fail,
    integration::justfile::just,
    services::{
        messaging::PermissionLevel, sounds::SoundServiceExt, status_wall::StatusServiceExt,
        storage::StorageServiceExt, twitch::TwitchServiceExt,
    },
};
use humantime_serde::re::humantime;
use maud::html;
use rustis::{client::BatchPreparedCommand, commands::HashCommands};
use tokio::time::sleep;

/// Respond with "pong!".
///
/// I heard that scarcity creates value, so getting a pong is very _cool_ and
/// _pog_, because only one person can get it in an hour.
///
/// There is also some magical property to this command..
#[command(global_gate = 1h, cost = -1)]
async fn ping(ctx: CommandContext) -> CommandResult {
    if !ctx.twitch().is_live().await? {
        fail!("stream is offline lmao")
    }

    ctx.storage()
        .set("last-pinger", &ctx.message().sender.name)
        .await?;

    ctx.reply("pong!".into()).await?;

    ctx.sounds().play_builtin("PING").await?;

    Ok(())
}

/// Get the name of the last person who got the `ping~` command during the
/// current stream.
#[command(sender_gate = 15s)]
async fn last_pinger(ctx: CommandContext) -> CommandResult {
    let pinger = ctx.storage().get("last-pinger").await?;

    match pinger {
        Some(pinger) => ctx.reply(format!("Last ping~ was by {pinger}")).await?,
        None => ctx.reply("No one has pinged yet".into()).await?,
    }

    Ok(())
}

/// A building block for basic static text commands.
///
/// This just makes the bot print the given text, but non-moderators can only call it through global macros.
///
/// So you can call a global macro `discord~` which will resolve to `echo:"discord link etc"~` and print it.
#[command(sender_gate = 5s, permission = Caster, GlobalMacroExempt)]
async fn echo(ctx: CommandContext, text: String) -> CommandResult {
    ctx.send(text).await?;
    Ok(())
}

/// Respond with the last error message for user.
///
/// Last error message is set when you run invalid commands, or if the command
/// execution managed to crash somehow. In the latter case, you'll be given the
/// message id - please send it to me to look at logs and fix the issue.
#[command(sender_gate = 3s)]
async fn last_error(ctx: CommandContext, chatter: Chatter) -> CommandResult {
    ctx.reply(
        ctx.storage()
            .get(&format!("last-error:{chatter}"))
            .await?
            .unwrap_or_else(|| {
                let whom = match ctx.is_owner(&chatter) {
                    true => "your",
                    false => "their",
                };
                format!("No errors in {whom} last message")
            }),
    )
    .await?;
    Ok(())
}

/// Check if noita.exe process is present, aka not dead.
#[command(sender_gate = 1m, NoitaData)]
async fn is_game_running(ctx: CommandContext) -> CommandResult {
    ctx.reply(
        if just("is-game-running", &[])?.get().await?.is_ok() {
            "It is running currently, yes"
        } else {
            "The game is NOT running"
        }
        .into(),
    )
    .await?;
    Ok(())
}

/// Wait for a specified duration milliseconds.
///
/// Very useful for multi-command messages.
#[command(shortcode=w, NoWall)]
async fn wait(ctx: CommandContext, duration: HoldTime<500, 300_000>) -> CommandResult {
    let duration = duration.get();
    tracing::debug!(
        duration.ms = duration.as_millis(),
        "waiting for {}",
        humantime::format_duration(duration)
    );

    let wall = ctx.status();
    let entry = wall.allocate().await;
    let inner_ctx = ctx.clone();
    let wall_task = tokio::spawn(async move {
        let name = &inner_ctx.message().sender.name;
        let nesting = inner_ctx.nesting_str();
        for i in (1..=duration.as_secs()).rev() {
            let status = html! {
                span style="color: #E38AF0" { (name) } ": wait:" (i) "s " (nesting)
            };
            entry.set(status.into()).await;
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
async fn r#break(ctx: CommandContext, chatter: Option<Required<Chatter>>) -> CommandResult {
    ctx.interrupt(chatter.as_ref().map(|c| &*c.id), InterruptKind::Break);
    Ok(())
}

/// Stop running all current commands.
///
/// This is similar to `break~`, except the commands following the holds that
/// get completed do not run.
#[command]
async fn interrupt(ctx: CommandContext, chatter: Option<Required<Chatter>>) -> CommandResult {
    ctx.interrupt(chatter.as_ref().map(|c| &*c.id), InterruptKind::Interrupt);
    Ok(())
}

/// Interrupt all commands *originating from the current message*.
///
/// On it's own this does nothing, but it can be used to limit the duration of
/// the current message or similar.
#[command]
async fn discard(ctx: CommandContext) -> CommandResult {
    ctx.local_interrupt();
    Ok(())
}

/// Set a bot setting.
///
/// Settings that currently do things are:
///   - `full-stop`: disables processing any commands from non-mods
///   - `nosr`: disables song requests
///
/// If the key argument is prefixed with `-` the setting is removed (value is
/// ignored).
#[command(permission = Moderator)]
async fn setting(ctx: CommandContext, key: String, value: Option<String>) -> CommandResult {
    if let Some(key) = key.strip_prefix("-") {
        ctx.storage().del(&format!("setting:{key}")).await?;
        ctx.reply(format!("Bot setting '{key}' removed")).await?;
    } else {
        ctx.storage()
            .set(&format!("setting:{key}"), value.as_deref().unwrap_or("1"))
            .await?;
        ctx.reply(format!("Bot setting '{key}' set")).await?;
    }
    Ok(())
}

/// Get information about a macro or command.
///
/// If you see someone running some weird command, run
/// `what-is:command:their-name~` to figure out what it was.
#[command(sender_gate = 3s)]
async fn what_is(ctx: CommandContext, name: String, to: Chatter) -> CommandResult {
    let (personal, global): (Option<String>, Option<String>) = {
        let storage = ctx.storage_old();
        let mut p = storage.create_pipeline();
        p.hget::<_, _, Option<String>>(format!("macros:{to}"), &name)
            .queue();
        p.hget::<_, _, Option<String>>("macros:global", &name)
            .queue();
        p.execute().await?
    };

    if let Some(script) = personal {
        let whom = match to.id == ctx.shared.owner.id {
            true => "your",
            false => "their",
        };
        ctx.reply(format!("`{name}` is one of {whom} macros: {script}"))
            .await?;
    } else if let Some(script) = global {
        ctx.reply(format!("`{name}` is a global macro: {script}"))
            .await?;
    } else if let Some(command) = ctx
        .runner()
        .get_command_meta(&name)
        .filter(|c| !c.is(CommandTag::Hidden))
    {
        let mut s = String::new();

        match command.shortcode {
            Some(shortcode) => {
                write!(&mut s, "`{}` (shortcode `{shortcode}`)", command.name)
            }
            None => write!(&mut s, "`{}`", command.name),
        }
        .unwrap();

        s.push_str(" is a ");
        if command.permission != PermissionLevel::Viewer {
            s.push_str(&format!("{:?}", command.permission).to_lowercase());
            s.push_str("-level ");
        }
        s.push_str("command");

        if let Some(gate) = command.sender_gate {
            write!(&mut s, ", sender gate {}", humantime::format_duration(gate)).unwrap();
        }
        if let Some(gate) = command.global_gate {
            write!(&mut s, ", global gate {}", humantime::format_duration(gate)).unwrap();
        }

        let required = command
            .args
            .iter()
            .filter(|a| (a.optional)().is_none())
            .count();
        let all = command.args.len();

        if all == 0 {
            s.push_str(". Takes no arguments");
        } else if required == all {
            if required == 1 {
                write!(&mut s, ". Takes 1 argument").unwrap();
            } else {
                write!(&mut s, ". Takes {all} arguments").unwrap();
            }
        } else {
            write!(&mut s, ". Takes {required}-{all} arguments").unwrap();
        }

        ctx.reply(s).await?;
    } else {
        ctx.reply(format!("`{name}` is not a macro or command"))
            .await?;
    }

    Ok(())
}
