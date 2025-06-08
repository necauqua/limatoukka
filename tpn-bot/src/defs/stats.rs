use std::cmp::Ordering as Ord;

use anyhow::Result;
use rustis::commands::StringCommands;

use crate::{
    commands::{
        args::{Chatter, InRange},
        command,
    },
    context::cmd::CommandContext,
    fail,
    services::chat_log::{Edge, Rank},
};

/// Count the amount of messages typed by a chatter.
///
/// If given a word, counts the messages containing it (given string can give
/// multiple words separated by spaces).
///
/// Login defaults to the sender (you can do `stat::word~` too).
#[command(sender_gate = 3s)]
async fn stat(ctx: CommandContext, chatter: Chatter, word: Option<String>) -> Result<()> {
    let count = ctx
        .chat_log()
        .stat(word.as_deref(), Some(&chatter.id))
        .await?;

    ctx.reply_buffered(format!("count: {count}")).await
}

/// Similar to `stat` except works across all of chat.
///
/// Without an argument counts all messages ever typed in chat since I started
/// archiving it (which is way before the Twitch Plays Noita happened).
///
/// With an argument filters messages by the given word(s), just like `stat`.
#[command(sender_gate = 3s)]
async fn stat_global(ctx: CommandContext, word: Option<String>) -> Result<()> {
    let count = ctx.chat_log().stat(word.as_deref(), None).await?;

    ctx.reply_buffered(format!("count: {count}")).await
}

/// Get the first message sent by a user (or you) in chat.
#[command(sender_gate = 3s)]
async fn first_message(ctx: CommandContext, chatter: Chatter) -> Result<()> {
    let response = match ctx.chat_log().edge(&chatter.id, Edge::First).await? {
        Some(msg) if msg.true_first => format!("Their first message was: {}", msg.message),
        Some(msg) => format!("Their first recorded message was: {}", msg.message),
        None => fail!("they never typed in chat"),
    };
    ctx.reply(response).await
}

/// Get the last message sent by a user (or you) in chat.
#[command(sender_gate = 3s)]
async fn last_message(ctx: CommandContext, chatter: Chatter) -> Result<()> {
    let edge = Edge::Last {
        exclude_message_id: Some(&ctx.message().id),
    };
    let response = match ctx.chat_log().edge(&chatter.id, edge).await? {
        Some(msg) => format!("Their last message was: {}", msg.message),
        None => fail!("they never typed in chat"),
    };
    ctx.reply(response).await
}

/// Get a list of top-N chatters of all time, by number of sent messages.
#[command(sender_gate = 10m)]
async fn top(ctx: CommandContext, n: Option<InRange<1, 15>>) -> Result<()> {
    ctx.reply(
        ctx.chat_log()
            .top_n(n.map_or(5, |n| n.get() as _), &[ctx.twitch().bot_id()])
            .await?
            .into_iter()
            .map(|(name, count)| format!("{name}: {count}"))
            .collect::<Vec<_>>()
            .join("; "),
    )
    .await
}

/// Get the place of the chatter (or you) in the "leaderboard" of how many messages they ~~spammed~~ sent
#[command(sender_gate = 1m)]
async fn rank(ctx: CommandContext, chatter: Chatter) -> Result<()> {
    let (whom, whom2) = match chatter.id == ctx.shared.owner.id {
        true => ("You", "you"),
        false => ("They", "them"),
    };
    ctx.reply(
        match ctx
            .chat_log()
            .rank(&chatter.id, &[ctx.twitch().bot_id()])
            .await?
        {
            Rank::Top1 => {
                format!("{whom} are a top-1 chatter. There is no god up there, other than {whom2}")
            }
            Rank::Top1k { pos, to_climb } => {
                format!("{whom} are a top-{pos} spammer, gz; {to_climb} messages left to climb up")
            }
            Rank::Bottom => "Placed >999, not enough spam KEKW".into(),
        },
    )
    .await
}

/// Get the bless/curse balance for the current run
#[command(sender_gate = 3s)]
async fn balance(ctx: CommandContext) -> Result<()> {
    let [blesses, curses]: [i64; 2] = ctx
        .storage()
        .mget(["balance:blesses", "balance:curses"])
        .await?;

    let balance = blesses - curses;

    match balance.cmp(&0) {
        Ord::Equal => {
            if blesses == 0 {
                ctx.reply("Nothing yet".into()).await?;
            } else {
                ctx.reply(format!(
                    "Perfectly balanced, as all things should be (↑{blesses}/{curses}↓)"
                ))
                .await?;
            }
        }
        Ord::Less => {
            ctx.reply(format!(
                "This run is cursed PepeHands (↑{blesses}/{curses}↓)"
            ))
            .await?;
        }
        Ord::Greater => {
            ctx.reply(format!(
                "This run is blessed AngelThump (↑{blesses}/{curses}↓)"
            ))
            .await?;
        }
    }
    Ok(())
}
