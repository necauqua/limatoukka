use crate::{
    commands::{
        CommandResult,
        args::{Chatter, InRange},
        command,
    },
    context::cmd::CommandContext,
    fail,
    services::{
        chat_log::{ChatLogServiceExt, Edge, Rank},
        stats::StatsServiceExt,
    },
};

/// Count the amount of messages typed by a chatter.
///
/// If given a word, counts the messages containing it (given string can give
/// multiple words separated by spaces).
///
/// Login defaults to the sender (you can do `stat::word~` too).
#[command(sender_gate = 3s)]
async fn stat(ctx: CommandContext, chatter: Chatter, word: Option<String>) -> CommandResult {
    let count = ctx
        .chat_logs()
        .stat(word.as_deref(), Some(&chatter.id))
        .await?;

    ctx.reply_buffered(format!("count: {count}")).await?;
    Ok(())
}

/// Similar to `stat` except works across all of chat.
///
/// Without an argument counts all messages ever typed in chat since I started
/// archiving it (which is way before the Twitch Plays Noita happened).
///
/// With an argument filters messages by the given word(s), just like `stat`.
#[command(sender_gate = 3s)]
async fn stat_global(ctx: CommandContext, word: Option<String>) -> CommandResult {
    let count = ctx.chat_logs().stat(word.as_deref(), None).await?;

    ctx.reply_buffered(format!("count: {count}")).await?;
    Ok(())
}

/// Get the first message sent by a user (or you) in chat.
#[command(sender_gate = 3s, shortcode = fm)]
async fn first_message(ctx: CommandContext, chatter: Chatter) -> CommandResult {
    let whom = chatter.them(ctx.owner(), "Your", "Their");
    let response = match ctx.chat_logs().edge(&chatter.id, Edge::First).await? {
        Some(msg) if msg.true_first => format!("{whom} first message was: {}", msg.message),
        Some(msg) => format!("{whom} first recorded message was: {}", msg.message),
        None => fail!("they never typed in chat"),
    };
    ctx.reply(response).await?;
    Ok(())
}

/// Get the last message sent by a user (or you) in chat.
#[command(sender_gate = 3s, shortcode = lm)]
async fn last_message(ctx: CommandContext, chatter: Chatter) -> CommandResult {
    let edge = Edge::Last {
        exclude_message_id: Some(&ctx.message().id),
    };
    let whom = chatter.them(ctx.owner(), "Your", "Their");
    let response = match ctx.chat_logs().edge(&chatter.id, edge).await? {
        Some(msg) => format!("{whom} last message was: {}", msg.message),
        None => fail!("they never typed in chat"),
    };
    ctx.reply(response).await?;
    Ok(())
}

/// Get a list of top-N chatters of all time, by number of sent messages.
#[command(sender_gate = 10m)]
async fn top(ctx: CommandContext, n: Option<InRange<1, 15>>) -> CommandResult {
    let exclude: &[&str] = match ctx.bot_id() {
        Some(bot_id) => &[bot_id],
        None => &[],
    };
    ctx.reply(
        ctx.chat_logs()
            .top_n(n.map_or(5, |n| n.get() as _), exclude)
            .await?
            .into_iter()
            .map(|(name, count)| format!("{name}: {count}"))
            .collect::<Vec<_>>()
            .join("; "),
    )
    .await?;
    Ok(())
}

/// Get the place of the chatter (or you) in the "leaderboard" of how many messages they ~~spammed~~ sent
#[command(sender_gate = 1m)]
async fn rank(ctx: CommandContext, chatter: Chatter) -> CommandResult {
    let (whom, whom2) = chatter.them(ctx.owner(), ("You", "you"), ("They", "them"));
    let exclude: &[&str] = match ctx.bot_id() {
        Some(bot_id) => &[bot_id],
        None => &[],
    };
    ctx.reply(match ctx.chat_logs().rank(&chatter.id, exclude).await? {
        Rank::Top1 => {
            format!("{whom} are a top-1 chatter. There is no god up there, other than {whom2}")
        }
        Rank::Top1k { pos, to_climb } => {
            format!("{whom} are a top-{pos} spammer, gz; {to_climb} messages left to climb up")
        }
        Rank::Bottom => "Placed >999, not enough spam KEKW".into(),
    })
    .await?;
    Ok(())
}

/// Get the amount of times you or some other chatter has successfully(!) used
/// the given command (does not work with shortcodes, use the full command
/// name).
///
/// Note that the accurate counting only started since the introduction of this
/// command (with ping~ manually backfilled from chat logs of bot replies).
#[command(sender_gate = 15s)]
async fn command_stat(ctx: CommandContext, name: String, chatter: Chatter) -> CommandResult {
    let name = name.to_lowercase();
    match ctx
        .stats()
        .count(&chatter.id, "command", &[("command", &name)])
        .await?
    {
        0 => ctx.reply(format!("never ran the {name} command")).await?,
        1 => ctx.reply(format!("ran the {name} command 1 time")).await?,
        count => {
            ctx.reply(format!("ran the {name} command {count} times"))
                .await?
        }
    }
    Ok(())
}
