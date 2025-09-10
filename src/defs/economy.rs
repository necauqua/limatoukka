use crate::{
    commands::{CommandResult, command},
    context::cmd::CommandContext,
};

/// Check your current balance of charges
#[command(sender_gate = 3s)]
async fn balance(ctx: CommandContext) -> CommandResult {
    let charges = ctx.storage().get_charges(&ctx.message().sender.id).await?;

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
