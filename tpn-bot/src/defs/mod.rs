use std::{borrow::Cow, cmp::Ordering};

use anyhow::Result;
use rustis::{
    client::BatchPreparedCommand,
    commands::{ExpireOption, GenericCommands, StringCommands},
};

use crate::{commands::args::HoldTime, context::cmd::CommandContext, fail};

mod keys;
mod macros;
mod mouse;
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
    let key = format!("holds:{key}");
    let mut tx = ctx.storage().create_transaction();
    tx.incr(&key).queue();
    tx.pexpire(&key, 60_000, ExpireOption::Nx).forget(); // just in case
    if tx.execute::<i64>().await? == 1 {
        if let Err(e) = down(&ctx).await {
            _ = ctx.storage().decr(&key).await;
            return Err(e);
        }
    }

    let sleep = ctx
        .holds()
        .interruptible(tokio::time::sleep(duration.get()))
        .await;

    let counter = ctx.storage().decr(&key).await?;

    match counter.cmp(&0) {
        Ordering::Equal => up(&ctx).await?,
        Ordering::Less => {
            // oopsie
            ctx.storage().del(&key).await?;
            up(&ctx).await?;
        }
        _ => {}
    }
    sleep?;

    Ok(())
}

async fn chatter_id<'a>(ctx: &'a CommandContext, login: Option<&str>) -> Result<Cow<'a, str>> {
    match login {
        Some(login) => {
            let id: Option<String> = ctx.storage().get(format!("twitch-users:{login}")).await?;
            match id {
                Some(id) => Ok(Cow::Owned(id)),
                None => fail!("they never even typed in chat"),
            }
        }
        None => Ok(Cow::Borrowed(&*ctx.owner)),
    }
}
