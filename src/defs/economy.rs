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
#[command(sender_gate = 3s)]
async fn balance(ctx: CommandContext, chatter: Chatter) -> CommandResult {
    let charges = ctx.charges().get(&chatter.id).await?;

    let whom = match ctx.owner() == &chatter {
        true => "Your",
        false => "Their",
    };

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

/// Respond with "pong!".
///
/// I heard that scarcity creates value, so getting a pong is very _cool_ and
/// _pog_, because only one person can get it in an hour.
///
/// Ping fails if you were the last person to do it!
///
/// There is also some magical property to this command..
#[command(global_gate = 1h, cost = -1)]
async fn ping(ctx: CommandContext) -> CommandResult {
    if !ctx.twitch().is_live().await? {
        fail!("stream is offline lmao")
    }

    let storage = ctx.storage();
    let pinger = &ctx.message().sender;

    let prev: Option<(String, String)> = storage.load("last-pinger").await?;
    if let Some((prev_id, _)) = prev
        && prev_id == pinger.id
    {
        fail!(
            "You were the last person to ping~! Wait for someone else to ping~ before you can ping~ again."
        );
    }

    storage
        .save("last-pinger", &(&pinger.id, &pinger.login))
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
    let pinger: Option<(String, String)> = ctx.storage().load("last-pinger").await?;

    match pinger {
        Some((_, pinger)) => ctx.reply(format!("Last ping~ was by {pinger}")).await?,
        None => ctx.reply("No one has pinged yet".into()).await?,
    }

    Ok(())
}

/// Say hi to the stream!
#[command(sender_gate = 12h, cost = -0.2)]
async fn hello(ctx: CommandContext) -> CommandResult {
    if !ctx.twitch().is_live().await? {
        fail!("stream is offline lmao")
    }
    ctx.reply("hiii".into()).await?;
    Ok(())
}
