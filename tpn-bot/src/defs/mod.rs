use anyhow::Result;

use crate::{commands::args::HoldTime, context::cmd::CommandContext};

mod data;
mod keys;
mod macros;
mod moderation;
mod mouse;
mod stats;
mod util;
mod voting;

async fn hold<D, U, RD, RU>(
    ctx: CommandContext,
    duration: HoldTime,
    key: &str,
    down: D,
    up: U,
) -> Result<()>
where
    RD: Future<Output = Result<()>>,
    RU: Future<Output = Result<()>>,
    D: FnOnce(&CommandContext) -> RD,
    U: FnOnce(&CommandContext) -> RU,
{
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
