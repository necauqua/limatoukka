use std::cmp::Ordering as Ord;

use anyhow::{Context, Result, bail};
use elasticsearch::{CountParts, SearchParts};
use rustis::commands::StringCommands;
use serde_json::{Value, json};

use crate::{
    commands::{
        args::{Chatter, InRange},
        command,
    },
    context::cmd::CommandContext,
    fail,
};

const INDEX: &str = "twitch-logs";

async fn stat_impl(
    ctx: &CommandContext,
    chatter: Option<Chatter>,
    word: Option<String>,
) -> Result<()> {
    let mut must = vec![json!({ "term": { "irc.cmd": "PRIVMSG" } })];

    if let Some(chatter) = chatter {
        must.push(json!({ "term": { "tags.user-id": chatter.id } }));
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
async fn stat(ctx: CommandContext, chatter: Chatter, word: Option<String>) -> Result<()> {
    stat_impl(&ctx, Some(chatter), word).await
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

async fn edge_message(
    ctx: &CommandContext,
    chatter: Chatter,
    sort: &str,
    exclude: Option<&str>,
) -> Result<Option<(String, bool)>> {
    let mut bool = serde_json::Map::new();
    bool.insert(
        "must".into(),
        json!([
            { "term": { "irc.cmd": "PRIVMSG" } },
            { "term": { "tags.user-id": chatter.id } },
        ]),
    );

    if let Some(exclude) = exclude {
        bool.insert(
            "must_not".into(),
            json!([{
                "term": { "_id": exclude }
            }]),
        );
    }

    let response = ctx
        .storage()
        .stats()
        .search(SearchParts::Index(&[INDEX]))
        .body(json!({
            "query": { "bool": bool },
            "sort": [{ "@timestamp": sort }],
            "size": 1,
        }))
        .send()
        .await?;
    let response = response.error_for_status_code()?.json::<Value>().await?;

    let Some(found) = response.pointer("/hits/hits/0/_source") else {
        return Ok(None);
    };
    let message = found
        .get("message")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .unwrap_or_default();
    let first = found.pointer("/tags/first-msg") == Some(&json!(1));
    Ok(Some((message, first)))
}

/// Get the first message sent by a user (or you) in chat.
#[command(sender_gate = 3s)]
async fn first_message(ctx: CommandContext, chatter: Chatter) -> Result<()> {
    let response = match edge_message(&ctx, chatter, "asc", None).await? {
        Some((message, true)) => format!("Their first message was: {message}"),
        Some((message, false)) => format!("Their first recorded message was: {message}"),
        None => fail!("they never typed in chat"),
    };
    ctx.reply(response).await
}

/// Get the last message sent by a user (or you) in chat.
#[command(sender_gate = 3s)]
async fn last_message(ctx: CommandContext, chatter: Chatter) -> Result<()> {
    let response = match edge_message(&ctx, chatter, "desc", Some(&ctx.message().id)).await? {
        Some((message, _)) => format!("Their last message was: {message}"),
        None => fail!("they never typed in chat"),
    };
    ctx.reply(response).await
}

/// Get a list of top-N chatters of all time, by number of sent messages.
#[command(sender_gate = 10m)]
async fn top(ctx: CommandContext, n: Option<InRange<1, 15>>) -> Result<()> {
    let n = n.map_or(5, |n| n.get());

    let response = ctx
        .storage()
        .stats()
        .search(SearchParts::Index(&[INDEX]))
        .body(json!({
            "size": 0,
            "query": {
                "bool": { "must_not": { "term": { "tags.user-id": ctx.twitch().bot_id() } } },
            },
            "aggs": {
              "top": {
                "terms": { "field": "tags.user-id", "size": n },
                "aggs": {
                  "name": {
                    "top_hits": { "size": 1, "_source": ["name"] }
                  }
                }
              }
            }
        }))
        .send()
        .await?;
    let response = response.error_for_status_code()?.json::<Value>().await?;

    let buckets = response
        .pointer("/aggregations/top/buckets")
        .and_then(|v| v.as_array())
        .context("malformed aggregation reply")?;

    let results = buckets
        .iter()
        .filter_map(|b| {
            let name = b
                .pointer("/name/hits/hits/0/_source/name")
                .and_then(|n| n.as_str());
            let count = b.get("doc_count").and_then(|c| c.as_i64());
            name.zip(count)
        })
        .map(|(name, count)| format!("{name}: {count}"))
        .collect::<Vec<_>>();
    if results.is_empty() {
        bail!("malformed aggregation reply");
    }
    ctx.reply(results.join("; ")).await
}

/// Get the place of the chatter (or you) in the "leaderboard" of how many messages they ~~spammed~~ sent
#[command(sender_gate = 1m)]
async fn rank(ctx: CommandContext, chatter: Chatter) -> Result<()> {
    let response = ctx
        .storage()
        .stats()
        .search(SearchParts::Index(&[INDEX]))
        .body(json!({
            "size": 0,
            "query": {
                "bool": { "must_not": { "term": { "tags.user-id": ctx.twitch().bot_id() } } },
            },
            "aggs": {
                "top": {
                    "terms": { "field": "tags.user-id", "size": 999 },
                }
            }
        }))
        .send()
        .await?;
    let response = response.error_for_status_code()?.json::<Value>().await?;

    let buckets = response
        .pointer("/aggregations/top/buckets")
        .and_then(|v| v.as_array())
        .context("malformed aggregation reply")?;

    let mut prev = None;
    let mut found = None;
    for (i, bucket) in buckets.iter().enumerate() {
        let count = bucket
            .get("doc_count")
            .and_then(|c| c.as_i64())
            .context("malformed aggregation reply")?;
        if bucket
            .get("key")
            .and_then(|k| k.as_str())
            .context("malformed aggregation reply")?
            == chatter.id
        {
            found = Some((i + 1, prev.map(|p| p - count)));
            break;
        }
        prev = Some(count)
    }

    ctx.reply(match found {
        Some((found, None)) => {
            // found is always 1 here
            format!("You are a top-{found} chatter. There is no god up there, other than you")
        }
        Some((found, Some(diff))) => {
            format!("You are a top-{found} spammer, gz; {diff} messages left to climb up")
        }
        None => "Placed >999, not enough spam KEKW".into(),
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
