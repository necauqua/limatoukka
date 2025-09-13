use maud::{Markup, html};
use serde::{Deserialize, Serialize};

use crate::{
    commands::{CommandResult, args::RestOfArgs, command},
    context::cmd::CommandContext,
    fail,
    services::{
        bets::{Bet, BetsServiceExt},
        charges::{Charges, ChargesServiceExt},
        status_wall::{EntryKey, StatusServiceExt},
        storage::StorageServiceExt,
    },
};

#[derive(Serialize, Deserialize, Debug)]
struct BetSetup {
    premise: String,
    options: Vec<String>,
    closed: bool,
    status_key: EntryKey,
}

fn render_status(total: u64, premise: &str) -> Markup {
    html! {
        span style="color:orange" { "BET OPEN (" (total) " betters):" }
        br
        span { (premise) }
    }
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

    let bet = BetSetup {
        premise,
        options,
        closed: false,
        status_key,
    };

    storage.save(key, &bet).await?;
    ctx.send(format!("[!!!] New bet started: {}", bet.premise))
        .await?;

    wall.set_and_bump(status_key, render_status(0, &bet.premise).into())
        .await;

    Ok(())
}

/// Close the current bet, preventing new bets from being placed.
#[command(permission = Caster)]
async fn close(ctx: CommandContext) -> CommandResult {
    let storage = ctx.storage();

    let Some(mut bet) = storage.load::<BetSetup>("bet:current").await? else {
        fail!("There is no active bet");
    };
    if bet.closed {
        fail!("Bet is already closed");
    }

    bet.closed = true;
    storage.save("bet:current", &bet).await?;

    ctx.send("[!!!] Bet is now closed".into()).await?;

    ctx.status().remove(bet.status_key).await;

    Ok(())
}

/// Reopen a closed bet, allowing new bets to be placed.
#[command (permission = Caster)]
async fn reopen(ctx: CommandContext) -> CommandResult {
    let storage = ctx.storage();

    let Some(mut bet) = storage.load::<BetSetup>("bet:current").await? else {
        fail!("There is no active bet");
    };
    if !bet.closed {
        fail!("Bet is already open");
    }

    bet.closed = false;
    storage.save("bet:current", &bet).await?;

    ctx.send("[!!!] Bet was reopened".into()).await?;

    let total = ctx.bets().bet_count("current").await?.unwrap_or_default();
    ctx.status()
        .set_and_bump(bet.status_key, render_status(total, &bet.premise).into())
        .await;

    Ok(())
}

/// Show the current active bet.
#[command(sender_gate = 10s)]
async fn is_bet(ctx: CommandContext) -> CommandResult {
    let Some(bet) = ctx.storage().load::<BetSetup>("bet:current").await? else {
        ctx.fail("There is no active bet").await?;
        return Ok(());
    };

    ctx.send(format!(
        "Current bet ({}!): {} (options: {}) ({} bets placed)",
        if bet.closed { "closed" } else { "open" },
        bet.premise,
        bet.options.join(", "),
        ctx.bets().bet_count("current").await?.unwrap_or_default(),
    ))
    .await?;

    Ok(())
}

/// Place a bet on the current active bet.
///
/// If you dont wager anything, you will still win 1⚡︎ if you were correct.
#[command(sender_gate = 5s)]
async fn bet(ctx: CommandContext, option: String, wager: Option<Charges>) -> CommandResult {
    let Some(bet) = ctx.storage().load::<BetSetup>("bet:current").await? else {
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

    let bets = ctx.bets();

    if let (Some(prev), _) = bets.remove_bet("current", ctx.sender()).await?
        && prev.amount != 0
    {
        ctx.charges().add(ctx.sender(), prev.amount.into()).await?;
    }

    if let Some(wager) = wager
        && !ctx.charges().consume(ctx.sender(), wager).await?
    {
        fail!("poor");
    }

    let wager = Bet {
        option,
        amount: wager.map_or(0, |c| c.as_i64() as _),
    };

    let total = ctx
        .bets()
        .place_bet("current", ctx.sender(), &wager)
        .await?;

    ctx.reply(match wager.amount {
        0 => format!("bet on '{}' accepted", wager.option),
        _ => format!(
            "accepted {} for as a bet on '{}'",
            Charges::from(wager.amount),
            wager.option,
        ),
    })
    .await?;

    ctx.status()
        .set_and_bump(bet.status_key, render_status(total, &bet.premise).into())
        .await;

    Ok(())
}

/// Remove your bet from the current active bet, refunding your wager.
#[command(sender_gate = 5s)]
async fn unbet(ctx: CommandContext) -> CommandResult {
    let Some(bet) = ctx.storage().load::<BetSetup>("bet:current").await? else {
        fail!("There is no active bet");
    };
    if bet.closed {
        fail!("Bet is closed");
    }

    let (wager, total) = ctx.bets().remove_bet("current", ctx.sender()).await?;

    ctx.reply(match wager {
        Some(wager) if wager.amount != 0 => {
            // refund
            ctx.charges().add(ctx.sender(), wager.amount.into()).await?;

            format!(
                "removed your '{}' bet (refunded {})",
                wager.option,
                Charges::from(wager.amount)
            )
        }
        Some(wager) => format!("removed your '{}' bet", wager.option),
        None => "you did not bet".into(),
    })
    .await?;

    ctx.status()
        .set_and_bump(bet.status_key, render_status(total, &bet.premise).into())
        .await;

    Ok(())
}

/// Cancel the current bet, refunding all wagers.
#[command(permission = Caster)]
async fn cancel_bet(ctx: CommandContext) -> CommandResult {
    let storage = ctx.storage();

    let Some(bet) = storage.load::<BetSetup>("bet:current").await? else {
        fail!("There is no active bet");
    };

    for (user_id, bet) in ctx.bets().finalize("current").await? {
        if bet.amount != 0 {
            ctx.charges().add(&user_id, bet.amount.into()).await?;
        }
    }

    storage.del("bet:current").await?;

    ctx.send("[!!!] Bet cancelled".into()).await?;

    ctx.status().remove(bet.status_key).await;

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

    let charges = ctx.charges();
    for (user_id, amount) in wins {
        charges.subtract(&user_id, amount.into()).await?;
    }

    storage.save("bet:current", &bet).await?;
    storage.del("last-bet").await?;

    ctx.send("[!!!] Bet rolled back".into()).await?;

    Ok(())
}

#[derive(Serialize, Deserialize, Debug)]
struct LastBet {
    bet: BetSetup,
    wins: Vec<(String, u64)>,
}

/// Settle the current bet, paying out the users who bet on the given option.
#[command(permission = Caster)]
async fn settle(ctx: CommandContext, option: String) -> CommandResult {
    let storage = ctx.storage();

    let Some(bet) = storage.load::<BetSetup>("bet:current").await? else {
        fail!("There is no active bet");
    };
    if !bet.options.iter().any(|o| o.eq_ignore_ascii_case(&option)) {
        fail!(
            "Invalid option. Valid options are: {}",
            bet.options.join(", ")
        );
    }

    let bets = ctx.bets().finalize("current").await?;

    let mut wins = Vec::new();

    let mut total_pool = 0;
    let mut winner_pool = 0;
    let mut losers = 0;
    for (user_id, bet) in bets {
        total_pool += bet.amount;
        if bet.option.eq_ignore_ascii_case(&option) {
            winner_pool += bet.amount;
            wins.push((user_id, bet.amount));
        } else {
            losers += 1;
        }
    }

    // huh
    if winner_pool != 0 {
        for win in &mut wins {
            win.1 = (win.1 * total_pool / winner_pool).max(1000);
        }
    } else {
        for win in &mut wins {
            win.1 = 1000;
        }
    }

    let last_bet = LastBet { bet, wins };

    let charges = ctx.charges();
    for (user_id, amount) in &last_bet.wins {
        charges.add(user_id, (*amount).into()).await?;
    }

    ctx.status().remove(last_bet.bet.status_key).await;

    // for rollbacks
    storage.save("last-bet", &last_bet).await?;
    storage.del("bet:current").await?;

    ctx.send(format!(
        "[!!!] Bet settled! ↑{}/{}↓ (total pool {})",
        last_bet.wins.len(),
        losers,
        Charges::from(total_pool),
    ))
    .await?;

    Ok(())
}
