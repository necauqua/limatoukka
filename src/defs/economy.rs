use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::{
    commands::{
        CommandResult,
        args::{Chatter, Required},
        command,
    },
    context::cmd::CommandContext,
    fail,
    services::{
        charges::{Charges, ChargesServiceExt},
        gates::GateServiceExt,
        sounds::SoundServiceExt,
        storage::StorageServiceExt,
        twitch::TwitchServiceExt,
    },
};

/// Check the current charge balance
#[command(sender_gate = 3s, shortcode = b)]
async fn balance(ctx: CommandContext, chatter: Chatter) -> CommandResult {
    let charges = ctx.charges().get(&chatter.id).await?;

    let whom = chatter.them(ctx.owner(), "Your", "Their");

    ctx.reply(format!("{whom} balance is {charges}")).await?;

    Ok(())
}

/// Spend a charge to remove all of your current timeouts.
#[command(cost = 1)]
async fn unleash_me(ctx: CommandContext) -> CommandResult {
    ctx.gates().ungate_all(ctx.sender()).await?;
    Ok(())
}

/// Transfer some of your charges to another user.
///
/// When run from global macros, the amount can be negative to "steal" charges.
/// This can be used by mods to set up macros that reward users.
#[command(sender_gate = 3s, cost = 0.004, GlobalMacroExempt)]
async fn transfer(
    ctx: CommandContext,
    target: Required<Chatter>,
    amount: Charges,
) -> CommandResult {
    let from = ctx.sender();
    let to = &*target.id;

    let (from, to, amount) = if ctx.in_global_macro && amount.as_i64() < 0 {
        (to, from, Charges::from(-amount.as_i64()))
    } else {
        (from, to, amount)
    };

    if ctx.charges().transfer(from, to, amount).await? {
        ctx.reply(format!(
            "Successfully transferred {amount} to {}",
            target.login
        ))
        .await?;
    } else {
        fail!("poor");
    }
    Ok(())
}

/// Conjure some charges out of thin air and award them to a user.
///
/// The amount can be negative 🙃
#[command(permission = Caster)]
async fn award(ctx: CommandContext, target: Chatter, amount: Charges) -> CommandResult {
    ctx.charges().add(&target.id, amount).await?;

    ctx.reply(format!("Awarded {amount} to {}", target.login))
        .await?;

    Ok(())
}

#[derive(Serialize, Deserialize)]
struct LastPinger {
    id: String,
    name: String,
    timestamp: SystemTime,
    next_gate: Duration,
}

/// Respond with "pong!".
///
/// I heard that scarcity creates value, so getting a pong is very _cool_ and
/// _pog_, because only one person can get it in an hour-ish.
///
/// This command has a dynamic global gate of 55-65 minutes, chosen at random.
///
/// Ping fails if you were the last person to do it!
///
/// There is also some magical property to this command..
#[command(cost = -1, sender_gate = 100ms)]
async fn ping(ctx: CommandContext) -> CommandResult {
    let storage = ctx.storage();

    let prev: Option<LastPinger> = storage.load("last-pinger").await?;

    if let Some(prev) = prev {
        if prev.id == ctx.message().sender.id {
            fail!(
                "You were the last person to ping~! Wait for someone else to ping~ before you can ping~ again."
            );
        }
        let elapsed = prev.timestamp.elapsed().unwrap_or_default();
        if elapsed < Duration::from_secs(600) {
            ctx.fail("KEKW U LOST KEKW").await?;
        }
        if elapsed < prev.next_gate {
            fail!("not yet");
        }
    }

    if !ctx.twitch().is_live().await? {
        fail!("stream is offline lmao")
    }

    let pinger = &ctx.message().sender;
    storage
        .save(
            "last-pinger",
            &LastPinger {
                id: pinger.id.clone(),
                name: pinger.login.clone(),
                timestamp: SystemTime::now(),
                next_gate: Duration::from_millis(rand::random_range(55 * 60_000..65 * 60_000)),
            },
        )
        .await?;

    // Xeanthorn
    if ctx.sender() == "132627333" {
        ctx.reply("ICMP Echo Reply".into()).await?;
    } else if rand::random_ratio(1, 100) {
        ctx.charges().add(ctx.sender(), Charges::ONE).await?;
        ctx.reply("ICMP Echo Reply".into()).await?;
    } else {
        ctx.reply("pong!".into()).await?;
    }

    ctx.sounds().play_builtin("PING").await?;

    Ok(())
}

/// Get the name of the last person who got the `ping~` command during the
/// current stream.
#[command(sender_gate = 15s)]
async fn last_pinger(ctx: CommandContext) -> CommandResult {
    match ctx.storage().load::<LastPinger>("last-pinger").await? {
        Some(p) => ctx.reply(format!("Last ping~ was by {}", p.name)).await?,
        None => ctx.reply("No one has pinged yet".into()).await?,
    }
    Ok(())
}

/// Say hi to the stream!
#[command(sender_gate = 12h, cost = -0.2, shortcode=hi)]
async fn hello(ctx: CommandContext) -> CommandResult {
    if !ctx.twitch().is_live().await? {
        fail!("stream is offline lmao")
    }
    ctx.reply(match ctx.sender() {
        "39063397" => "Lasiacchi".into(),
        _ => "hiii".into(),
    })
    .await?;
    if let Some(bot) = ctx.bot_id() {
        ctx.charges().add(bot, Charges::from(200)).await?;
    }
    Ok(())
}
