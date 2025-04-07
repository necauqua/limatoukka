use std::time::Duration;

use anyhow::Result;
use rustis::{
    client::BatchPreparedCommand,
    commands::{ExpireOption, GenericCommands, StringCommands},
};

use crate::commands::{args::AtMost, context::CommandContext};

mod keys;
mod mouse;
mod util;
mod voting;

pub(self) type HoldTime = Option<AtMost<15_000>>;

pub(self) async fn hold<D, U, RD, RU>(
    ctx: CommandContext,
    millis: HoldTime,
    key: &str,
    down: D,
    up: U,
) -> Result<()>
where
    RD: Future<Output = Result<()>>,
    RU: Future<Output = Result<()>>,
    D: FnOnce(CommandContext) -> RD,
    U: FnOnce(CommandContext) -> RU,
{
    let key = format!("holds:{key}");
    let mut tx = ctx.storage.create_transaction();
    tx.incr(&key).queue();
    tx.pexpire(&key, 60_000, ExpireOption::Nx).forget(); // just in case
    if tx.execute::<i64>().await? == 1 {
        if let Err(e) = down(ctx.clone()).await {
            _ = ctx.storage.decr(&key).await;
            return Err(e);
        }
    }

    let duration = Duration::from_millis(millis.map_or(500, |a| a.get()) as u64);
    ctx.holds.sleep(duration).await;

    let counter = ctx.storage.decr(&key).await?;
    if counter == 0 {
        up(ctx).await?;
    } else if counter < 0 {
        // oopsie
        ctx.storage.del(&key).await?;
        up(ctx).await?;
    }
    Ok(())
}
