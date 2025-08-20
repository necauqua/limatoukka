use crate::{
    commands::{CommandResult, args::HoldTime},
    context::cmd::CommandContext,
};

mod data;
mod economy;
mod keys;
mod macros;
mod moderation;
mod mouse;
mod sounds;
mod stats;
mod util;
mod voting;

async fn hold(
    ctx: CommandContext,
    duration: HoldTime,
    key: &str,
    down: impl AsyncFnOnce(&CommandContext) -> CommandResult,
    up: impl AsyncFnOnce(&CommandContext) -> CommandResult,
) -> CommandResult {
    let hold = ctx.storage().hold(key)?;

    if hold.down().await? {
        if let Err(e) = down(&ctx).await {
            _ = hold.up().await;
            return Err(e);
        }
    }

    let sleep = ctx.interruptible(tokio::time::sleep(duration.get())).await;

    if hold.up().await? {
        up(&ctx).await?;
    }

    sleep?;

    Ok(())
}
