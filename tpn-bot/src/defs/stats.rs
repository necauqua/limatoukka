use std::cmp::Ordering as Ord;

use anyhow::Result;
use elasticsearch::{CountParts, SearchParts};
use rustis::commands::StringCommands;
use serde_json::{Value, json};

use crate::{commands::command, context::cmd::CommandContext, fail};

const INDEX: &str = "twitch-logs";

async fn stat_impl(
    ctx: &CommandContext,
    chatter_id: Option<&str>,
    word: Option<String>,
) -> Result<()> {
    let mut must = vec![json!({ "term": { "irc.cmd": "PRIVMSG" } })];

    if let Some(id) = chatter_id {
        must.push(json!({ "term": { "tags.user-id": id } }));
    }
    if let Some(word) = word {
        must.push(json!({ "match": { "message": word } }));
    }

    let response = ctx
        .storage()
        .stats()
        .count(CountParts::Index(&[INDEX]))
        .body(json!({ "query": { "bool": { "must": must } } }))
        .send()
        .await?;

    let response = response.error_for_status_code()?.json::<Value>().await?;
    let count = response.get("count").and_then(|v| v.as_i64()).unwrap_or(0);

    ctx.reply_buffered(format!("count: {count}")).await?;

    Ok(())
}

/// Count the amount of messages typed by a chatter.
///
/// If given a word, counts the messages containing it (given string can give
/// multiple words separated by spaces).
///
/// Login defaults to the sender (you can do `stat::word~` too).
#[command(sender_gate = 3s)]
async fn stat(ctx: CommandContext, login: Option<String>, word: Option<String>) -> Result<()> {
    let id = ctx.chatter_id(login.as_deref()).await?;
    stat_impl(&ctx, Some(&id), word).await
}

/// Similar to `stat` except works across all of chat.
///
/// Without an argument counts all messages ever typed in chat since I started
/// archiving it (which is way before the Twitch Plays Noita happened).
///
/// With an argument filters messages by the given word(s), just like `stat`.
#[command(sender_gate = 3s)]
async fn stat_global(ctx: CommandContext, word: Option<String>) -> Result<()> {
    stat_impl(&ctx, None, word).await
}

/// Get the first message sent by a user (or you) in chat.
#[command(sender_gate = 3s)]
async fn first_message(ctx: CommandContext, login: Option<String>) -> Result<()> {
    let id = ctx.chatter_id(login.as_deref()).await?;

    let response = ctx
        .storage()
        .stats()
        .search(SearchParts::Index(&[INDEX]))
        .body(json!({
            "query": {
                "bool": {
                    "must": [
                        { "term": { "irc.cmd": "PRIVMSG" } },
                        { "term": { "tags.user-id": id } },
                    ]
                }
            },
            "sort": [{ "@timestamp": "asc" }],
            "size": 1,
        }))
        .send()
        .await?;

    let response = response.error_for_status_code()?.json::<Value>().await?;

    let Some(found) = response.pointer("/hits/hits/0/_source") else {
        fail!("not found");
    };
    let message = found
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    ctx.reply(if found.pointer("/tags/first-msg") == Some(&json!(1)) {
        format!("Their first message was: `{message}`")
    } else {
        format!("Their first recorded message was: `{message}`")
    })
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
