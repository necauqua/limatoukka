use std::time::Duration;

use anyhow::Result;
use rustis::{
    client::BatchPreparedCommand,
    commands::{GenericCommands, SetCommands, StringCommands},
};

use crate::commands::{
    command,
    context::{AppContext, CommandContext},
};

async fn get_current(ctx: &AppContext) -> Result<Option<String>> {
    Ok(ctx
        .storage
        .get::<_, Option<String>>("votes:current")
        .await?)
}

/// Vote yes in an ongoing vote.
#[command]
async fn yes(ctx: CommandContext) -> Result<()> {
    let vote = get_current(&ctx).await?;
    todo!()
}

/// Vote no in an ongoing vote.
#[command]
async fn no(ctx: CommandContext) -> Result<()> {
    let vote = get_current(&ctx).await?;
    todo!()
}

async fn vote_trigger(ctx: CommandContext, key: &str) -> Result<()> {
    if get_current(&ctx).await?.is_some() {
        return Ok(());
    }
    if !ctx
        .sender_command_gate(Duration::from_secs(ctx.config.voting.trigger_gate_secs))
        .await?
    {
        return Ok(());
    }

    let key = format!("vote_trigger:{key}");

    let mut tx = ctx.storage.create_transaction();
    tx.sadd(&key, &*ctx.message.sender.id).forget();
    tx.scard(&key).queue();

    if tx.execute::<usize>().await? > 1 {
        // added to an ongoing trigger
        return Ok(());
    }

    tracing::info!(key, "vote trigger started");

    ctx.schedule(
        Duration::from_secs(ctx.config.voting.trigger_interval_secs),
        |ctx| async move {
            let mut tx = ctx.storage.create_transaction();
            tx.scard(&key).queue();
            tx.del(&key).forget();

            let triggerers: usize = tx.execute().await?;
            if triggerers >= ctx.config.voting.trigger_min_people as _ {
                // todo start the actual vote here
                if get_current(&ctx).await?.is_some() {
                    tracing::info!(key, "already voting, skip");
                    return Ok(());
                }
            } else {
                tracing::info!(key, "not enough people triggered, skip");
            }
            Ok(())
        },
    );

    Ok(())
}
