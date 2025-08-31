use std::{fmt::Write as _, time::Duration};

use crate::{
    commands::{
        CommandResult, CommandTag,
        args::{Chatter, HoldTime, Required},
        command,
    },
    context::{
        app::{AppContext, InterruptKind},
        cmd::CommandContext,
    },
    services::messaging::PermissionLevel,
};
use humantime_serde::re::humantime;
use maud::html;
use rustis::{
    client::BatchPreparedCommand,
    commands::{GenericCommands, HashCommands, StringCommands},
};
use tokio::time::sleep;

/// Respond with "pong!".
///
/// I heard that scarcity creates value, so getting a pong is very _cool_ and
/// _pog_, because only one person can get it in an hour.
///
/// There is also some magical property to this command..
#[command(global_gate = 1h, cost = -1)]
async fn ping(ctx: CommandContext) -> CommandResult {
    ctx.reply("pong!".into()).await?;
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
            .get::<_, Option<_>>(format!("last-error:{chatter}"))
            .await?
            .unwrap_or_else(|| {
                let whom = match chatter.id == ctx.shared.owner.id {
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
        if AppContext::just("is-game-running", &[])?
            .get()
            .await?
            .is_ok()
        {
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

/// Set a bot flag.
///
/// Flags that currently do things are:
///   - `full-stop`: disables processing any commands from non-mods
///   - `no-restarts`: disables the game restarting on player death
///   - `nightmare`: enables the nightmare mode
///
/// If the flag argument is prefixed with `-` it is removed if it was set
/// previously.
#[command(permission = Moderator)]
async fn flag(ctx: CommandContext, flag: String) -> CommandResult {
    if let Some(flag) = flag.strip_prefix("-") {
        ctx.storage().del(format!("flags:{flag}")).await?;
    } else {
        ctx.storage().set(format!("flags:{flag}"), "1").await?;
    }
    Ok(())
}

/// Makes the bot start the game in nightmare/ng.
#[command(permission = Vip)]
async fn set_nightmare(ctx: CommandContext, value: bool) -> CommandResult {
    if value {
        ctx.storage().set("flags:nightmare", "1").await?;
        ctx.reply("Nightmare mode enabled".into()).await?;
    } else {
        ctx.storage().del("flags:nightmare").await?;
        ctx.reply("Nightmare mode disabled".into()).await?;
    }
    Ok(())
}

/// Makes the bot start the game with a specific seed.
#[command(permission = Vip)]
async fn fix_seed(ctx: CommandContext, seed: u32) -> CommandResult {
    ctx.storage().set("set-seed", seed).await?;
    ctx.reply("Seed set".into()).await?;
    Ok(())
}

/// Undoes the effect of `fix_seed~`, so the game will start with a random seed again.
#[command(permission = Vip)]
async fn unfix_seed(ctx: CommandContext) -> CommandResult {
    ctx.storage().del("set-seed").await?;
    ctx.reply("Seed unset".into()).await?;
    Ok(())
}

/// Get information about a macro or command.
///
/// If you see someone running some weird command, run
/// `what-is:command:their-name~` to figure out what it was.
#[command(sender_gate = 3s)]
async fn what_is(ctx: CommandContext, name: String, to: Chatter) -> CommandResult {
    let (personal, global): (Option<String>, Option<String>) = {
        let storage = ctx.storage();
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
