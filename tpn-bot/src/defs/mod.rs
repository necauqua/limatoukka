use std::time::Duration;

use anyhow::Result;
use rustis::{
    client::BatchPreparedCommand,
    commands::{ExpireOption, GenericCommands, StringCommands},
};
use tokio::time::sleep;

use crate::commands::{args::AtMost, context::CommandContext};

mod keys;
mod mouse;
mod util;
mod voting;

pub(self) async fn hold<D, U, RD, RU>(
    ctx: CommandContext,
    millis: Option<AtMost<500>>,
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
        down(ctx.clone()).await?;
    }
    sleep(Duration::from_millis(millis.map_or(500, |a| a.get()) as u64)).await;

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
