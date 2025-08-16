use anyhow::Result;

use crate::{commands::command, context::cmd::CommandContext};

/// Check your current balance of charges
#[command(sender_gate = 3s)]
async fn balance(ctx: CommandContext) -> Result<()> {
    let charges = ctx.storage().get_charges(&ctx.message().sender.id).await?;

    ctx.reply(format!(
        "Your have {}.{:03} charges",
        charges / 1000,
        charges % 1000
    ))
    .await?;

    Ok(())
}
