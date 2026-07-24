use std::collections::HashMap;

use maud::{DOCTYPE, Markup, Render, html};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::{
    commands::{
        CommandResult,
        args::{Chatter, RestOfArgs},
        command,
    },
    context::{app::AppContext, cmd::CommandContext},
    fail,
    integration::justfile::just,
    services::{
        charges::{Charges, ChargesServiceExt, ConsumeResult},
        display::{DisplayServiceAux, DisplayServiceExt},
        names::NamesServiceExt,
        storage::StorageServiceExt,
    },
};

#[derive(Serialize, Deserialize, Debug)]
struct Bet {
    premise: String,
    options: Vec<String>,
    closed: bool,
    auto: bool,
    wagers: HashMap<String, (String, Charges, bool)>, // twitch_uid -> (option, amount)
}

impl Bet {
    fn amounts_by_option(&self) -> Vec<(&String, (Charges, usize))> {
        let mut amounts = HashMap::<_, (Charges, usize)>::new();
        for (option, amount, _) in self.wagers.values() {
            let (v, c) = amounts.entry(option).or_default();
            *v = *v + *amount;
            *c += 1;
        }
        self.options
            .iter()
            .map(|option| (option, amounts.get(option).copied().unwrap_or_default()))
            .collect::<Vec<_>>()
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct Win {
    user_id: String,
    bet: Charges,
    payout: Charges,
    all_in: bool,
}

#[derive(Serialize, Deserialize, Debug)]
struct LastBet {
    bet: Bet,
    result: String,
    wins: Vec<Win>,
}

impl Render for Bet {
    fn render(&self) -> Markup {
        let amounts = self.amounts_by_option();

        html! {
            div style="margin: 0 auto" {
                span style="color:orange" {
                    "BET OPEN (" (self.wagers.len()) " betters):"
                }
                br;
                span { (self.premise) }
                br;
                @for (option, (amount, count)) in &amounts {
                    span style="margin-right: 1rem" { (option) ": " (*amount) "(" (count) ")" }
                }
            }
        }
    }
}

static LOCK: Mutex<()> = Mutex::const_new(());

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

    // for now we just have a cringe global lock on all storage get-sets
    // ideally storage would allow atomic async function applications or something
    let guard = LOCK.lock().await;

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

    let bet = Bet {
        premise,
        options,
        closed: false,
        auto: auto_close.unwrap_or_default(),
        wagers: HashMap::new(),
    };

    storage.save(key, &bet).await?;
    drop(guard);

    ctx.display().render("bets", &bet);
    ctx.send(format!("[!!!] New bet started: {}", bet.premise))
        .await?;

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

    let guard = LOCK.lock().await;
    let Some(mut bet) = storage.load::<Bet>("bet:current").await? else {
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
    drop(guard);

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

    let guard = LOCK.lock().await;
    let Some(mut bet) = storage.load::<Bet>("bet:current").await? else {
        fail!("There is no active bet");
    };
    if !bet.closed {
        fail!("Bet is already open");
    }

    bet.closed = false;

    storage.save("bet:current", &bet).await?;
    drop(guard);

    ctx.display().render("bets", &bet);
    ctx.send("[!!!] Bet was reopened".into()).await?;

    Ok(())
}

/// Show the current active bet.
#[command(sender_gate = 10s)]
async fn is_bet(ctx: CommandContext) -> CommandResult {
    let Some(bet) = ctx.storage().load::<Bet>("bet:current").await? else {
        ctx.fail("There is no active bet").await?;
        return Ok(());
    };

    let amounts = bet.amounts_by_option();

    ctx.send(format!(
        "Current bet ({}!): {} ({} in {} bets)",
        if bet.closed { "closed" } else { "open" },
        bet.premise,
        amounts
            .iter()
            .map(|(option, (amount, count))| format!("{option}:{amount}({count})"))
            .collect::<Vec<_>>()
            .join("/"),
        bet.wagers.len(),
    ))
    .await?;

    Ok(())
}

/// Place a bet on the current active bet.
///
/// If you dont wager anything, you will still win 1⚡︎ if you were correct.
#[command(sender_gate = 5s)]
async fn bet(ctx: CommandContext, option: String, wager: Option<Charges>) -> CommandResult {
    let guard = LOCK.lock().await;
    let Some(mut bet) = ctx.storage().load::<Bet>("bet:current").await? else {
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

    if let Some((_, amount, _)) = bet.wagers.remove(ctx.sender())
        && amount.non_zero()
    {
        ctx.charges().add(ctx.sender(), amount).await?;
    }

    let all_in = match wager {
        Some(wager) => match ctx.charges().consume(ctx.sender(), wager).await? {
            ConsumeResult::Fail => fail!("poor"),
            ConsumeResult::Success { bankrupt } => bankrupt,
        },
        _ => false,
    };

    let wager = wager.unwrap_or_default();
    let msg = if wager.non_zero() {
        format!("accepted {wager} as a bet on '{option}'")
    } else {
        format!("zero-bet on '{option}' accepted")
    };

    bet.wagers
        .insert(ctx.sender().to_owned(), (option, wager, all_in));

    ctx.storage().save("bet:current", &bet).await?;
    drop(guard);

    ctx.display().render("bets", &bet);
    ctx.reply(msg).await?;

    Ok(())
}

/// Get your current bet, if any.
#[command(sender_gate = 5s)]
async fn get_bet(ctx: CommandContext, chatter: Chatter) -> CommandResult {
    let Some(bet) = ctx.storage().load::<Bet>("bet:current").await? else {
        ctx.fail("There is no active bet").await?;
        return Ok(());
    };

    let whom = chatter.them(ctx.owner(), "You", "They");

    if let Some((option, amount, _)) = bet.wagers.get(&chatter.id) {
        if amount.non_zero() {
            ctx.reply(format!("{whom} bet {amount} on '{option}'"))
                .await?
        } else {
            ctx.reply(format!("{whom} bet on '{option}'")).await?
        }
    } else {
        ctx.fail(format!("{whom} did not bet yet")).await?
    }

    Ok(())
}

/// Remove your bet from the current active bet, refunding your wager.
#[command(sender_gate = 5s)]
async fn unbet(ctx: CommandContext) -> CommandResult {
    let guard = LOCK.lock().await;
    let storage = ctx.storage();
    let Some(mut bet) = storage.load::<Bet>("bet:current").await? else {
        fail!("There is no active bet");
    };
    if bet.closed {
        fail!("Bet is closed");
    }

    let Some((option, amount, _)) = bet.wagers.remove(ctx.sender()) else {
        ctx.fail("you did not bet").await?;
        return Ok(());
    };

    if amount.non_zero() {
        // refund
        ctx.charges().add(ctx.sender(), amount).await?;

        ctx.reply(format!("removed your '{option}' bet (refunded {amount})"))
            .await?
    } else {
        ctx.reply(format!("removed your '{option}' bet")).await?
    }

    storage.save("bet:current", &bet).await?;
    drop(guard);

    ctx.display().render("bets", &bet);

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

    let guard = LOCK.lock().await;
    let Some(bet) = storage.load::<Bet>("bet:current").await? else {
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

    for (user_id, (_, amount, _)) in &bet.wagers {
        if amount.non_zero() {
            ctx.charges().add(user_id, *amount).await?;
        }
    }

    storage.save("last-bet", &bet).await?;
    storage.del("bet:current").await?;
    drop(guard);

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

    let guard = LOCK.lock().await;
    let Some(LastBet { bet, result, wins }) = storage.load::<LastBet>("last-bet").await? else {
        fail!("No bet to rollback");
    };

    let charges = ctx.charges();
    for win in wins {
        // add negative instead of consume to put people into negatives
        // if they managed to immediately spend the win
        charges.add(&win.user_id, -win.payout).await?;
    }

    storage.save("bet:current", &bet).await?;
    storage.del("last-bet").await?;
    drop(guard);

    ctx.send(format!("[!!!] Bet rolled back (settlement was '{result}')"))
        .await?;

    Ok(())
}

/// Settle the current bet, paying out the users who bet on the given option.
#[command(permission = Caster)]
async fn settle(ctx: CommandContext, option: String) -> CommandResult {
    let storage = ctx.storage();

    let guard = LOCK.lock().await;
    let Some(bet) = storage.load::<Bet>("bet:current").await? else {
        fail!("There is no active bet");
    };
    if !bet.options.iter().any(|o| o.eq_ignore_ascii_case(&option)) {
        fail!(
            "Invalid option. Valid options are: {}",
            bet.options.join(", ")
        );
    }

    let mut wins = Vec::new();
    let mut losers = Vec::new();

    let mut total_pool = 0;
    let mut winner_pool = 0;

    for (user_id, (bet_option, amount, all_in)) in &bet.wagers {
        total_pool += amount.as_u64();
        if bet_option.eq_ignore_ascii_case(&option) {
            winner_pool += amount.as_u64();
            wins.push(Win {
                user_id: user_id.clone(),
                bet: *amount,
                payout: Charges::ZERO,
                all_in: *all_in,
            });
        } else {
            losers.push((user_id.clone(), -*amount, *all_in));
        }
    }

    // huh
    #[allow(clippy::manual_checked_ops)]
    // seems to be false positive? we check once outside of the loop
    if winner_pool != 0 {
        for win in &mut wins {
            win.payout = (win.bet.as_u64() * total_pool / winner_pool + 1000).into();
        }
    } else {
        for win in &mut wins {
            win.payout = Charges::ONE;
        }
    }

    wins.sort_by_key(|win| -win.bet.as_i64());
    losers.sort_by_key(|(_, loss, _)| loss.as_i64());

    let last_bet = LastBet {
        bet,
        result: option,
        wins,
    };

    let charges = ctx.charges();
    for win in &last_bet.wins {
        charges.add(&win.user_id, win.payout).await?;
    }

    ctx.display().set("bets", html! {});

    // for rollbacks
    storage.save("last-bet", &last_bet).await?;
    storage.del("bet:current").await?;
    drop(guard);

    let names = ctx.names();
    names
        .warm_up(
            &last_bet
                .wins
                .iter()
                .map(|win| &*win.user_id)
                .chain(losers.iter().map(|(id, _, _)| &**id))
                .collect::<Vec<_>>(),
        )
        .await?;

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
                    .column {
                        display: flex;
                        flex-direction: column;
                    }
                " }
            }
            body style="height: 100%; margin:0; display: flex" {
                div style="margin: auto" {
                    div style="padding-bottom: 3rem" { "Bet result for: " (last_bet.bet.premise) }

                    div style="display: flex; gap: 2rem" {
                        div.column {
                            div style="color: aquamarine; text-align: center" {
                                "Winners"
                            }
                            @for win in &last_bet.wins {
                                div {
                                    @let profit = win.payout - win.bet;
                                    @if win.bet.is_zero() {
                                        span style="color: orange" { (names.lookup(&win.user_id).await?) ": +" (profit) }
                                    } @else {
                                        (names.lookup(&win.user_id).await?) ": +" (profit) " ("

                                        @if win.all_in {
                                            span style="color: yellow" { "bet " (win.bet) }
                                        } @else {
                                            "bet " (win.bet)
                                        }

                                        ")"
                                    }
                                }
                            }
                        }
                        div.column {
                            div style="color: brown; text-align: center" {
                                "Losers"
                            }
                            @for (id, loss, all_in) in &losers {
                                div {
                                    @let name = names.lookup(id).await?;
                                    @if loss.is_zero() {
                                        span style="color: green" { (name) }
                                    } @else {
                                        (name) ": "
                                        @if *all_in {
                                            span style="color: yellow" { (loss) }
                                        } @else {
                                            (loss)
                                        }
                                    }
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
