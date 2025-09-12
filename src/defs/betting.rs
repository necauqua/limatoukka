use maud::html;
use serde::{Deserialize, Serialize};

use crate::{
    commands::{CommandResult, args::RestOfArgs, command},
    context::cmd::CommandContext,
    fail,
    services::{
        bets::{BetsServiceExt, Win},
        charges::{Charges, ChargesServiceExt},
        status_wall::{EntryKey, StatusServiceExt},
        storage::StorageServiceExt,
    },
};

#[derive(Serialize, Deserialize, Debug)]
struct Bet {
    premise: String,
    options: Vec<String>,
    closed: bool,
    status_key: EntryKey,
}

/// Create a new bet. If no options are provided, they default to "believe" and "doubt".
#[command(permission = Caster)]
async fn mkbet(ctx: CommandContext, premise: String, options: RestOfArgs) -> CommandResult {
    let storage = ctx.storage();

    let key = "bet:current"; //format!("bet:{id}");
    if storage.has(key).await? {
        ctx.fail("There is already an active bet").await?;
    }

    let options = options.get(&ctx).await?;
    let options = match options.len() {
        0 => vec!["believe".into(), "doubt".into()],
        1 => {
            ctx.fail("Must provide at least two options").await?;
            return Ok(());
        }
        _ => options.into_iter().map(|s| s.unwrap_or_default()).collect(),
    };

    let wall = ctx.status();
    let status_key = wall.new_key();
    wall.set_and_bump(
        status_key,
        html! {
            span style="color:orange" { "BET OPEN:" }
            br
            span { (premise) }
        }
        .into(),
    )
    .await;

    let bet = Bet {
        premise,
        options,
        closed: false,
        status_key,
    };

    storage.save(key, &bet).await?;
    ctx.send(format!("[!!!] New bet started: {}", bet.premise))
        .await?;

    Ok(())
}

/// Close the current bet, preventing new bets from being placed.
#[command(permission = Caster)]
async fn close(ctx: CommandContext) -> CommandResult {
    let storage = ctx.storage();

    let Some(mut bet) = storage.load::<Bet>("bet:current").await? else {
        fail!("There is no active bet");
    };
    if bet.closed {
        fail!("Bet is already closed");
    }

    bet.closed = true;
    storage.save("bet:current", &bet).await?;

    ctx.status().remove(bet.status_key).await;

    ctx.send("[!!!] Bet is now closed".into()).await?;

    Ok(())
}

/// Place a bet on the current active bet.
///
/// If you dont wager anything, you will still win 1⚡︎ if you were correct.
#[command(sender_gate = 5s)]
async fn bet(ctx: CommandContext, option: String, wager: Option<Charges>) -> CommandResult {
    let Some(bet) = ctx.storage().load::<Bet>("bet:current").await? else {
        fail!("There is no active bet");
    };
    if bet.closed {
        fail!("Bet is closed");
    }
    if wager.is_some_and(|w| w.as_i64() < 0) {
        fail!("Wager must be non-negative");
    }
    if !bet.options.iter().any(|o| o.eq_ignore_ascii_case(&option)) {
        let suffix = if ctx.in_global_macro {
            " (you likely want to run bet:<option>:<wager>~)"
        } else {
            ""
        };
        fail!(
            "Invalid option. Valid options are: {}{suffix}",
            bet.options.join(", ")
        );
    }

    // todo fix double-charging lmao
    if let Some(wager) = wager
        && !ctx.charges().consume(ctx.sender(), wager).await?
    {
        fail!("poor");
    }

    let total = ctx
        .bets()
        .place_bet(
            "current",
            ctx.sender(),
            &option,
            wager.map_or(0, |c| c.as_i64()),
        )
        .await?;

    ctx.status()
        .set_and_bump(
            bet.status_key,
            html! {
                span style="color:orange" { "BET OPEN (" (total) " betters):" }
                br
                span { (bet.premise) }
            }
            .into(),
        )
        .await;

    Ok(())
}

/// Remove your bet from the current active bet, refunding your wager.
#[command(sender_gate = 5s)]
async fn unbet(ctx: CommandContext) -> CommandResult {
    let Some(bet) = ctx.storage().load::<Bet>("bet:current").await? else {
        fail!("There is no active bet");
    };
    if bet.closed {
        fail!("Bet is closed");
    }

    let (_option, refund, betters) = ctx.bets().remove_bet("current", ctx.sender()).await?;
    if refund > 0 {
        ctx.charges()
            .add(ctx.sender(), Charges::from(refund))
            .await?;
    }

    ctx.status()
        .set_and_bump(
            bet.status_key,
            html! {
                span style="color:orange" { "BET OPEN (" (betters) " betters):" }
                br
                span { (bet.premise) }
            }
            .into(),
        )
        .await;

    Ok(())
}

/// Rollback the last settled bet, returning all winnings to the users and
/// restoring the bet state.
///
/// This is obviously to fix potential fat-finger mistakes.
#[command(permission = Caster)]
async fn rollback(ctx: CommandContext) -> CommandResult {
    let storage = ctx.storage();

    let Some(LastBet { bet, wins }) = storage.load::<LastBet>("last-bet").await? else {
        fail!("No bet to rollback");
    };

    for (user_id, amount) in &wins {
        ctx.charges()
            .consume(user_id, Charges::from(*amount))
            .await?;
    }

    storage.save("bet:current", &bet).await?;
    storage.del("last-bet").await?;

    ctx.send("[!!!] Bet rolled back".into()).await?;

    Ok(())
}

#[derive(Serialize, Deserialize, Debug)]
struct LastBet {
    bet: Bet,
    wins: Vec<(String, i64)>,
}

/// Settle the current bet, paying out the users who bet on the given option.
#[command(permission = Caster)]
async fn settle(ctx: CommandContext, option: String) -> CommandResult {
    let storage = ctx.storage();

    let Some(bet) = storage.load::<Bet>("bet:current").await? else {
        fail!("There is no active bet");
    };
    if !bet.options.iter().any(|o| o.eq_ignore_ascii_case(&option)) {
        fail!(
            "Invalid option. Valid options are: {}",
            bet.options.join(", ")
        );
    }

    let bet_result = ctx.bets().settle("current", &option).await?;

    let charges = ctx.charges();
    let mut wins = vec![];
    for Win { user_id, amount } in &bet_result.winners {
        let amount = *amount.max(&1000);
        charges.add(user_id, Charges::from(amount)).await?;
        wins.push((user_id.clone(), amount));
    }

    ctx.status().remove(bet.status_key).await;

    // for rollbacks
    storage.save("last-bet", &LastBet { bet, wins }).await?;
    storage.del("bet:current").await?;

    ctx.send(format!(
        "[!!!] Bet settled! ↑{}/{}↓ (total pool {})",
        bet_result.winners.len(),
        bet_result.losers,
        Charges::from(bet_result.total_pool),
    ))
    .await?;

    Ok(())
}
