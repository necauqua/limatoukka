use crate::{
    commands::{CommandResult, command},
    context::cmd::CommandContext,
    fail,
    services::{charges::ChargesService, gates::GateService},
};

/// Check your current balance of charges
#[command(sender_gate = 3s)]
async fn balance(ctx: CommandContext) -> CommandResult {
    let charges = ctx
        .service::<dyn ChargesService>()
        .get(&ctx.message().sender.id)
        .await?;

    match (charges / 1000, charges % 1000) {
        (1, 0) => ctx.reply("Your have 1 charge".into()),
        (whole, 0) => ctx.reply(format!("Your have {whole} charges")),
        (whole, fraction) => ctx.reply(format!(
            "Your have {whole}.{} charges",
            format!("{fraction:03}",).trim_end_matches('0')
        )),
    }
    .await?;

    Ok(())
}

/// Spend a charge to remove all of your current timeouts.
#[command]
async fn unleash_me(ctx: CommandContext) -> CommandResult {
    if !ctx.consume_charges(1_000).await? {
        fail!("poor");
    }

    ctx.service::<dyn GateService>()
        .ungate_all(&ctx.message().sender.id)
        .await?;

    Ok(())
}
