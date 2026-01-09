use std::{iter, time::Duration};

use maud::{DOCTYPE, Markup, html};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::task::JoinHandle;

use crate::{
    commands::{
        CommandResult,
        args::{Chatter, RestOfArgs},
        command,
        runner::CommandError,
    },
    context::{app::AppContext, cmd::CommandContext},
    fail,
    integration::justfile::just,
    services::{
        bets::{Bet, BetsServiceExt},
        caches::CacheServiceExt,
        charges::{Charges, ChargesServiceExt},
        display::DisplayServiceExt,
        storage::StorageServiceExt,
        twitch::TwitchServiceExt,
    },
};

#[derive(Serialize, Deserialize, Debug)]
struct BetSetup {
    premise: String,
    options: Vec<String>,
    closed: bool,
    auto: bool,
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
async fn mkbet(
    ctx: CommandContext,
    premise: String,
    auto_close: Option<bool>,
    options: RestOfArgs,
) -> CommandResult {
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

    let bet = BetSetup {
        premise,
        options,
        closed: false,
        auto: auto_close.unwrap_or_default(),
    };

    storage.save(key, &bet).await?;
    ctx.send(format!("[!!!] New bet started: {}", bet.premise))
        .await?;

    ctx.display().set("bets", render_status(0, &bet.premise));

    Ok(())
}

#[derive(Error, Debug)]
pub enum BetCloseError {
    #[error("No active bet")]
    NoActiveBet,
    #[error("Bet is already closed")]
    AlreadyClosed,
    #[error("Bet is not auto-closable")]
    NotAutoClosable,
    #[error("internal")]
    Internal(#[from] anyhow::Error),
}

pub async fn close_bet(ctx: &AppContext, auto: bool) -> Result<(), BetCloseError> {
    let storage = ctx.storage();

    let Some(mut bet) = storage.load::<BetSetup>("bet:current").await? else {
        return Err(BetCloseError::NoActiveBet);
    };
    if bet.closed {
        return Err(BetCloseError::AlreadyClosed);
    }
    if auto && !bet.auto {
        return Err(BetCloseError::NotAutoClosable);
    }

    bet.closed = true;
    storage.save("bet:current", &bet).await?;

    ctx.display().set("bets", html! {});

    Ok(())
}

/// Close the current bet, preventing new bets from being placed.
#[command(permission = Caster)]
async fn close(ctx: CommandContext) -> CommandResult {
    match close_bet(&ctx, false).await {
        Ok(()) => {
            ctx.send("[!!!] Bet is now closed".into()).await?;
            Ok(())
        }
        Err(e @ (BetCloseError::NoActiveBet | BetCloseError::AlreadyClosed)) => fail!("{e}"),
        Err(BetCloseError::NotAutoClosable) => unreachable!(),
        Err(BetCloseError::Internal(e)) => Err(e.into()),
    }
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

    let total = ctx.bets().count("current").await?.unwrap_or_default();

    ctx.display()
        .set("bets", render_status(total, &bet.premise));

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
        ctx.bets().count("current").await?.unwrap_or_default(),
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

    if let (Some(prev), _) = bets.remove("current", ctx.sender()).await?
        && prev.amount.non_zero()
    {
        ctx.charges().add(ctx.sender(), prev.amount).await?;
    }

    if let Some(wager) = wager
        && !ctx.charges().consume(ctx.sender(), wager).await?
    {
        fail!("poor");
    }

    let wager = Bet {
        option,
        amount: wager.unwrap_or_default(),
    };

    let total = ctx.bets().place("current", ctx.sender(), &wager).await?;

    ctx.reply(if wager.amount.non_zero() {
        format!("accepted {} as a bet on '{}'", wager.amount, wager.option,)
    } else {
        format!("zero-bet on '{}' accepted", wager.option)
    })
    .await?;

    ctx.display()
        .set("bets", render_status(total, &bet.premise));

    Ok(())
}

/// Get your current bet, if any.
#[command(sender_gate = 5s)]
async fn get_bet(ctx: CommandContext, chatter: Chatter) -> CommandResult {
    if ctx
        .storage()
        .load::<BetSetup>("bet:current")
        .await?
        .is_none()
    {
        ctx.fail("There is no active bet").await?;
        return Ok(());
    };

    let whom = chatter.them(ctx.owner(), "You", "They");

    match ctx.bets().get("current", &chatter.id).await? {
        None => ctx.fail(format!("{whom} did not bet yet")).await?,
        Some(wager) if wager.amount.non_zero() => {
            ctx.reply(format!("{whom} bet {} on '{}'", wager.amount, wager.option))
                .await?
        }
        Some(wager) => {
            ctx.reply(format!("{whom} bet on '{}'", wager.option))
                .await?
        }
    }

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

    let (wager, total) = ctx.bets().remove("current", ctx.sender()).await?;

    match wager {
        None => ctx.fail("you did not bet").await?,
        Some(wager) if wager.amount.non_zero() => {
            // refund
            ctx.charges().add(ctx.sender(), wager.amount).await?;

            ctx.reply(format!(
                "removed your '{}' bet (refunded {})",
                wager.option, wager.amount,
            ))
            .await?
        }
        Some(wager) => {
            ctx.reply(format!("removed your '{}' bet", wager.option))
                .await?
        }
    };

    ctx.display()
        .set("bets", render_status(total, &bet.premise));

    Ok(())
}

#[derive(Error, Debug)]
pub enum BetCancelError {
    #[error("No active bet")]
    NoActiveBet,
    #[error("Bet is not auto-cancellable")]
    NotAutoCancellable,
    #[error("Bet is already closed")]
    AlreadyClosed,
    #[error("internal")]
    Internal(#[from] anyhow::Error),
}

pub async fn do_cancel_bet(ctx: &AppContext, auto: bool) -> Result<(), BetCancelError> {
    let storage = ctx.storage();

    let Some(bet) = storage.load::<BetSetup>("bet:current").await? else {
        return Err(BetCancelError::NoActiveBet);
    };
    if auto {
        if !bet.auto {
            return Err(BetCancelError::NotAutoCancellable);
        }
        if bet.closed {
            return Err(BetCancelError::AlreadyClosed);
        }
    }

    for (user_id, bet) in ctx.bets().finalize("current").await? {
        if bet.amount.non_zero() {
            ctx.charges().add(&user_id, bet.amount).await?;
        }
    }

    storage.del("bet:current").await?;

    ctx.display().set("bets", html! {});

    Ok(())
}

/// Cancel the current bet, refunding all wagers.
#[command(permission = Caster)]
async fn cancel_bet(ctx: CommandContext) -> CommandResult {
    match do_cancel_bet(&ctx, false).await {
        Ok(()) => {
            ctx.send("[!!!] Bet cancelled".into()).await?;
            Ok(())
        }
        Err(e @ BetCancelError::NoActiveBet) => fail!("{e}"),
        Err(BetCancelError::NotAutoCancellable | BetCancelError::AlreadyClosed) => unreachable!(),
        Err(BetCancelError::Internal(e)) => Err(e.into()),
    }
}

/// Rollback the last settled bet, returning all winnings to the users and
/// restoring the bet state.
///
/// This is obviously to fix potential fat-finger mistakes.
#[command(permission = Caster)]
async fn rollback(ctx: CommandContext) -> CommandResult {
    let storage = ctx.storage();

    let Some(LastBet { bet, result, wins }) = storage.load::<LastBet>("last-bet").await? else {
        fail!("No bet to rollback");
    };

    let charges = ctx.charges();
    for (user_id, _, payout) in wins {
        // add negative instead of consume to put people into negatives
        // if they managed to immediately spend the win
        charges.add(&user_id, -payout).await?;
    }

    storage.save("bet:current", &bet).await?;
    storage.del("last-bet").await?;

    ctx.send(format!("[!!!] Bet rolled back (settlement was '{result}')"))
        .await?;

    Ok(())
}

#[derive(Serialize, Deserialize, Debug)]
struct LastBet {
    bet: BetSetup,
    result: String,
    wins: Vec<(String, Charges, Charges)>,
}

fn get_name_task(ctx: &CommandContext, user_id: &str) -> JoinHandle<Result<String, CommandError>> {
    let ctx = ctx.clone();
    let user_id = user_id.to_owned();
    tokio::spawn(async move {
        Ok(ctx
            .caches()
            .get_cached(
                "twitch-name",
                Duration::from_secs(24 * 60 * 60),
                &user_id,
                async || {
                    anyhow::Ok(
                        ctx.twitch()
                            .get_display_name(&user_id)
                            .await?
                            .unwrap_or_else(|| format!("unknown twitch user {user_id}")),
                    )
                },
            )
            .await?)
    })
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
    let mut losers = Vec::new();

    let mut total_pool = 0;
    let mut winner_pool = 0;

    for (user_id, bet) in bets {
        total_pool += bet.amount.as_u64();
        if bet.option.eq_ignore_ascii_case(&option) {
            winner_pool += bet.amount.as_u64();
            wins.push((user_id, bet.amount, Charges::ZERO));
        } else {
            losers.push((user_id, -bet.amount));
        }
    }

    // huh
    if winner_pool != 0 {
        for (_, bet, payout) in &mut wins {
            *payout = (bet.as_u64() * total_pool / winner_pool + 1000).into();
        }
    } else {
        for (_, _, payout) in &mut wins {
            *payout = Charges::ONE;
        }
    }

    wins.sort_by_key(|(_, bet, payout)| (*bet - *payout).as_i64());
    losers.sort_by_key(|(_, loss)| loss.as_i64());

    let last_bet = LastBet {
        bet,
        result: option,
        wins,
    };

    let charges = ctx.charges();
    for (user_id, _, payout) in &last_bet.wins {
        charges.add(user_id, *payout).await?;
    }

    ctx.display().set("bets", html! {});

    // for rollbacks
    storage.save("last-bet", &last_bet).await?;
    storage.del("bet:current").await?;

    let zipped = last_bet
        .wins
        .iter()
        .map(|(uid, bet, payout)| Some((get_name_task(&ctx, uid), *bet, (*payout - *bet))))
        .chain(iter::repeat_with(|| None))
        .zip(
            losers
                .iter()
                .map(|(uid, loss)| Some((get_name_task(&ctx, uid), *loss)))
                .chain(iter::repeat_with(|| None)),
        )
        .take_while(|(a, b)| a.is_some() || b.is_some());

    let html = html! {
        (DOCTYPE)
        html lang="en" style="background: #1d1f21; color: #c9cacc; height: 100%;" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1.0";
                title { "Last Bet" }
                style { "
                    td {
                      padding: 0 2.5rem;
                    }
                    * {
                      font-family: 'JetBrains Mono',mono;
                      font-size: 1.75rem;
                      white-space: nowrap;
                    }
                " }
            }
            body style="height: 100%; margin:0; display: flex" {
                div style="margin: auto" {
                    div style="padding-bottom: 3rem" { "Bet result for: " (last_bet.bet.premise) }
                    table {
                        tr {
                            th style="color: aquamarine" { "Winners" }
                            th style="color: brown" { "Losers" }
                        }
                        @for (winner, loser) in zipped {
                            tr {
                                @if let Some((name_task, amount, win)) = winner {
                                    td {
                                        (name_task.await.unwrap()?) ": "

                                        @if win.is_zero() {
                                            span style="color: orange" { "+" (win) }
                                        } @else {
                                            "+" (win)
                                        }

                                        " (bet " (amount) ")"
                                    }
                                } @else {
                                    td {}
                                }
                                @if let Some((name_task, amount)) = loser {
                                    td {
                                        (name_task.await.unwrap()?) ": "
                                        @if amount.is_zero() {
                                            span style="color: green" { (amount) }
                                        } @else {
                                            (amount)
                                        }
                                    }
                                } @else {
                                    td {}
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    just("upload-large-reply", &[&html.0, "last-bet"])?
        .check()
        .await?;

    ctx.send(format!(
        "[!!!] Bet settled! ↑{}/{}↓ (total pool {}) | uq.rs/last-bet",
        last_bet.wins.len(),
        losers.len(),
        Charges::from(total_pool),
    ))
    .await?;

    Ok(())
}
