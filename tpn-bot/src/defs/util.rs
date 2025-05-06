use std::{collections::HashMap, time::Duration};

use crate::{
    commands::{args::HoldTime, command, runner::CommandFailure},
    context::{app::AppContext, cmd::CommandContext},
    fail,
    services::messaging::PermissionLevel,
};
use anyhow::{Context, Result, bail};
use maud::html;
use noita_engine_reader::{
    memory::MemoryStorage,
    types::components::{DamageModelComponent, UIIconComponent},
};
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
async fn last_error(ctx: CommandContext, login: Option<String>) -> Result<()> {
    let id = ctx.chatter_id(login.as_deref()).await?;
    let status: Option<String> = ctx.storage().get(format!("last-error:{id}")).await?;
    if let Some(status) = status {
        ctx.reply(status).await?;
    } else {
        ctx.reply("No errors in your last message".into()).await?;
    }
    Ok(())
}

/// Read the current seed
#[command(global_gate = 15s, permission = Moderator)]
async fn seed(ctx: CommandContext) -> Result<()> {
    match ctx.noita().get_seed().await {
        Some(seed) => ctx.reply(format!("{seed}")).await,
        None => fail!("no data"),
    }
}

/// Read the current death count
#[command(global_gate = 15s, shortcode=deaths)]
async fn death_count(ctx: CommandContext) -> Result<()> {
    let stats = ctx
        .noita()
        .with(|n| Ok(n.read_stats()?))
        .await
        .map_err(|e| {
            tracing::warn!(error=?e, "failed to read stats");
            CommandFailure::new("failed to read game data - is it running?".into())
        })?;
    ctx.reply(stats.global.death_count.to_string()).await
}

/// Read the currently picked up perks. Look ma, streamer wands at home!
#[command(global_gate = 5s)]
async fn perks(ctx: CommandContext, top_n: Option<u32>) -> Result<()> {
    let perks = ctx
        .noita()
        .with(|n| {
            let (entity, _) = n.get_player()?.context("no player")?;
            let p = n.proc().clone();
            let store = n.component_store::<UIIconComponent>()?;
            let mut perks = HashMap::<_, u32>::new();

            for child in entity.children.read(&p)?.read(&p)? {
                if let Some(ui) = store.get(&child.read(&p)?)? {
                    *perks.entry(ui.name.read(&p)?).or_default() += 1;
                };
            }
            let mut perks = perks.into_iter().collect::<Vec<_>>();
            perks.sort_unstable_by_key(|(_, c)| -(*c as i32));
            Ok(perks)
        })
        .await
        .map_err(|e| {
            tracing::warn!(error=?e, "failed to read stats");
            CommandFailure::new("failed to read game data - is it running?".into())
        })?;

    let msg = perks
        .into_iter()
        .take(top_n.unwrap_or(u32::MAX) as _)
        .map(|(perk, count)| {
            format!(
                "{count}x {}",
                perk.trim_start_matches("$perk_").replace("_", " ")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    ctx.reply(msg).await
}

/// Read the current non-1 damage multipliers of the player entity.
#[command(global_gate = 5s, permission = Vip)]
async fn damage_multipliers(ctx: CommandContext) -> Result<()> {
    let dmc = ctx
        .noita()
        .with(|n| {
            let (entity, _) = n.get_player()?.context("no player")?;

            let component = n
                .component_store::<DamageModelComponent>()?
                .get(&entity)?
                .context("no damage model component?")?;

            Ok(component)
        })
        .await
        .map_err(|e| {
            tracing::warn!(error=?e, "failed to read damage model component");
            CommandFailure::new("failed to read game data - is it running?".into())
        })?;

    let m = dmc.damage_multipliers;

    // no compile-time reflection sadge
    let list = vec![
        ("melee", m.melee),
        ("projectile", m.projectile),
        ("explosion", m.explosion),
        ("electricity", m.electricity),
        ("fire", m.fire),
        ("drill", m.drill),
        ("slice", m.slice),
        ("ice", m.ice),
        ("healing", m.healing),
        ("physics_hit", m.physics_hit),
        ("radioactive", m.radioactive),
        ("poison", m.poison),
        ("overeating", m.overeating),
        ("curse", m.curse),
        ("holy", m.holy),
    ];

    let msg = list
        .into_iter()
        .filter(|(_, v)| *v != 1.0)
        .map(|(name, value)| format!("{name}={value:?}",))
        .collect::<Vec<_>>()
        .join(", ");
    ctx.reply(msg).await
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
    ctx.break_holds().await;
    Ok(())
}

/// Stop running all current commands.
/// This is similar to `break~`, except the commands following the holds that
/// get completed do not run.
#[command]
async fn interrupt(ctx: CommandContext) -> Result<()> {
    ctx.interrupt_holds().await;
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
