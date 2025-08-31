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

/// Check your current balance of charges
#[command(sender_gate = 3s)]
async fn balance(ctx: CommandContext) -> CommandResult {
    let charges = ctx
        .service::<dyn ChargesService>()
        .get(&ctx.message().sender.id)
        .await?;

    ctx.reply(format!("Your balance is {charges}")).await?;

    Ok(())
}

/// Spend a charge to remove all of your current timeouts.
#[command(cost = 1)]
async fn unleash_me(ctx: CommandContext) -> CommandResult {
    ctx.service::<dyn GateService>()
        .ungate_all(&ctx.message().sender.id)
        .await?;
    Ok(())
}

/// Transfer some of your charges to another user
#[command(sender_gate = 3s, cost = 0.004)]
async fn transfer(
    ctx: CommandContext,
    target: Required<Chatter>,
    amount: Charges,
) -> CommandResult {
    if ctx
        .service::<dyn ChargesService>()
        .transfer(&ctx.message().sender.id, &target.id, amount)
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
