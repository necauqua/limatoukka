use crate::{
    commands::{
        CommandResult,
        args::{Chatter, Required},
        command,
    },
    context::cmd::CommandContext,
    fail,
    services::{
        charges::{Charges, ChargesService},
        gates::GateService,
    },
};

/// Check the current charge balance
#[command(sender_gate = 3s)]
async fn balance(ctx: CommandContext, chatter: Chatter) -> CommandResult {
    let charges = ctx
        .service::<dyn ChargesService>()
        .get(ctx.sender())
        .await?;

    let whom = match ctx.is_owner(&chatter) {
        true => "Your",
        false => "Their",
    };

    ctx.reply(format!("{whom} balance is {charges}")).await?;

    Ok(())
}

/// Spend a charge to remove all of your current timeouts.
#[command(cost = 1)]
async fn unleash_me(ctx: CommandContext) -> CommandResult {
    ctx.service::<dyn GateService>()
        .ungate_all(ctx.sender())
        .await?;
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

    if ctx
        .service::<dyn ChargesService>()
        .transfer(from, to, amount)
        .await?
    {
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
    ctx.service::<dyn ChargesService>()
        .add(&target.id, amount)
        .await?;

    ctx.reply(format!("Awarded {amount} to {}", target.login))
        .await?;

    Ok(())
}
