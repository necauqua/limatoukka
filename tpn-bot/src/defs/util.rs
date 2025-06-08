use std::{fmt::Write as _, time::Duration};

use crate::{
    commands::{
        self, CommandTag,
        args::{Chatter, HoldTime, Required},
        command,
    },
    context::{
        app::{AppContext, InterruptKind},
        cmd::CommandContext,
    },
    fail,
    services::messaging::PermissionLevel,
};
use anyhow::Result;
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
            .unwrap_or_else(|| {
                let whom = match chatter.id == ctx.shared.owner.id {
                    true => "your",
                    false => "their",
                };
                format!("No errors in {whom} last message")
            }),
    )
    .await
}

/// Sometimes the capture dies (but the game is fine) because of my brittle scripts.
///
/// Try running this first before doing a full restart etc etc.
#[command(global_gate = 30s, OBSControl)]
async fn fix_obs_capture() -> Result<()> {
    AppContext::just("obs-reset-display", &[]).await
}

/// The sound setup is the most brittle jank thing actually, and dies most often.
///
/// Try running this first before doing a full restart etc etc.
#[command(global_gate = 30s, OBSControl)]
async fn fix_obs_sound() -> Result<()> {
    AppContext::just("sound-setup", &[]).await
}

/// Check if noita.exe process is present, aka not dead.
#[command(sender_gate = 1m, NoitaData)]
async fn is_game_running(ctx: CommandContext) -> Result<()> {
    ctx.reply(
        if AppContext::just_bool("is-game-running").await.is_ok() {
            "It is running currently, yes"
        } else {
            "The game is NOT running"
        }
        .into(),
    )
    .await
}

/// Wait for a specified duration milliseconds.
///
/// Very useful for multi-command messages.
#[command(shortcode=w, NoWall)]
async fn wait(ctx: CommandContext, duration: HoldTime<500, 300_000>) -> Result<()> {
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
async fn r#break(ctx: CommandContext, chatter: Option<Required<Chatter>>) -> Result<()> {
    ctx.interrupt(chatter.as_ref().map(|c| &*c.id), InterruptKind::Break);
    Ok(())
}

/// Stop running all current commands.
///
/// This is similar to `break~`, except the commands following the holds that
/// get completed do not run.
#[command]
async fn interrupt(ctx: CommandContext, chatter: Option<Required<Chatter>>) -> Result<()> {
    ctx.interrupt(chatter.as_ref().map(|c| &*c.id), InterruptKind::Interrupt);
    Ok(())
}

/// Interrupt all commands *originating from the current message*.
///
/// On it's own this does nothing, but it can be used to limit the duration of
/// the current message or similar.
#[command]
async fn discard(ctx: CommandContext) -> Result<()> {
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
async fn flag(ctx: CommandContext, flag: String) -> Result<()> {
    if let Some(flag) = flag.strip_prefix("-") {
        ctx.storage().del(format!("flags:{flag}")).await?;
    } else {
        ctx.storage().set(format!("flags:{flag}"), "1").await?;
    }
    Ok(())
}

/// Makes the bot start the game with a specific seed.
#[command(permission = Moderator)]
async fn fix_seed(ctx: CommandContext, seed: u32) -> Result<()> {
    ctx.storage().set("set-seed", seed).await?;
    ctx.reply("Seed set".into()).await
}

/// Undoes the effect of `fix_seed~`, so the game will start with a random seed again.
#[command(permission = Moderator)]
async fn unfix_seed(ctx: CommandContext) -> Result<()> {
    ctx.storage().del("set-seed").await?;
    ctx.reply("Seed unset".into()).await
}

/// Say something on stream through the TTS.
///
/// Only works for >= subscriber level, or from global macros.
#[command(sender_gate = 1m)]
async fn tts(ctx: CommandContext, msg: String) -> Result<()> {
    if !ctx.in_global_macro && ctx.message().sender.level < PermissionLevel::Subscriber {
        fail!("TTS is pay to win, or from global macros");
    }
    if msg.is_empty() {
        fail!("message cannot be empty");
    }

    tracing::debug!(msg = msg, "sending TTS");
    let mut child = AppContext::cringe_aws_tts_through_shell(&msg).await?;
    let pid = child.id().unwrap();

    tokio::select! {
        _ = child.wait() => tracing::debug!(msg = msg, "finished TTS"),
        _ = ctx.wait_for_interrupt() => {

            // ugh meh
            unsafe {
                libc::killpg(pid as _, libc::SIGTERM);
            }

            child.wait().await?;
        },
    }

    Ok(())
}

/// Get information about a macro or command.
///
/// If you see someone running some weird command, run
/// `what-is:command:their-name~` to figure out what it was.
#[command(sender_gate = 3s)]
async fn what_is(ctx: CommandContext, name: String, to: Chatter) -> Result<()> {
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
    } else if let Some(command) = commands::find(&name).filter(|c| !c.is(CommandTag::Hidden)) {
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
