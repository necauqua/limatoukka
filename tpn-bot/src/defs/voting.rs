use std::{borrow::Cow, fmt::Display, time::Duration};

use anyhow::Result;
use maud::{Markup, html};
use rustis::{
    client::BatchPreparedCommand,
    commands::{
        ExpireOption, GenericCommands, SetCommands, SetCondition, SetExpiration, StringCommands,
    },
};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;

use crate::{
    commands::{
        args::{Chatter, Required},
        command,
    },
    config::Voting,
    context::cmd::CommandContext,
    fail,
    services::status_wall::EntryKey,
};

#[derive(Clone, Copy)]
enum Vote {
    Yes,
    No,
}

impl Display for Vote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Vote::Yes => write!(f, "yes"),
            Vote::No => write!(f, "no"),
        }
    }
}

impl Vote {
    fn inverse(&self) -> Self {
        match self {
            Vote::Yes => Vote::No,
            Vote::No => Vote::Yes,
        }
    }

    fn lambda<A>(&self, a: A, b: A) -> (A, A) {
        match self {
            Vote::Yes => (a, b),
            Vote::No => (b, a),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct VoteData<'s> {
    key: Cow<'s, str>,
    title: Cow<'s, str>,
    chat_title: Cow<'s, str>,
    wall: EntryKey,
}

async fn vote(ctx: CommandContext, vote: Vote) -> Result<()> {
    let Some(vote_data) = ctx.storage().get::<_, Option<String>>("vote").await? else {
        fail!("no ongoing vote");
    };
    let vote_data: VoteData = serde_json::from_str(&vote_data)?;

    let key = format!("vote:{}:{vote}", vote_data.key);
    let key_inv = format!("vote:{}:{}", vote_data.key, vote.inverse());

    let mut tx = ctx.storage().create_transaction();
    tx.sadd(&key, &ctx.message().sender.id).forget();
    tx.srem(&key_inv, &ctx.message().sender.id).forget();
    tx.scard(&key).queue();
    tx.scard(&key_inv).queue();
    tx.exists("vote").queue(); // re-check the vote status to avoid a race here ig

    let (count, inv_count, exists): (usize, usize, usize) = tx.execute().await?;
    if exists == 0 {
        fail!("no ongoing vote");
    }
    let (yes, no) = vote.lambda(count, inv_count);
    let percentage = (yes as f64 / (yes + no) as f64) * 100.0;

    let status = format!(
        "{}:<br>{yes}/{no} ({percentage:.2}%, need {:.0}%)",
        vote_data.title,
        ctx.config().vote_min_ratio * 100.0
    );
    ctx.status_wall().set_top(vote_data.wall, status).await;

    Ok(())
}

/// Vote yes in an ongoing vote.
#[command]
async fn yes(ctx: CommandContext) -> Result<()> {
    vote(ctx, Vote::Yes).await
}

/// Vote no in an ongoing vote.
#[command]
async fn no(ctx: CommandContext) -> Result<()> {
    vote(ctx, Vote::No).await
}

/// Check if there is an ongoing vote and what it is about.
#[command(global_gate = 10s)]
async fn is_vote(ctx: CommandContext) -> Result<()> {
    let vote = ctx.storage().get::<_, Option<String>>("vote").await?;
    if let Some(vote) = vote {
        let data: VoteData = serde_json::from_str(&vote)?;
        ctx.reply(format!("Ongoing vote is: {}", data.chat_title))
            .await?;
    } else {
        ctx.reply("No ongoing vote".into()).await?;
    }
    Ok(())
}

async fn vote_trigger<R>(
    ctx: &CommandContext,
    key: String,
    wall_title: Markup,
    chat_title: String,
    vote_config: Voting,
    action: R,
) -> Result<()>
where
    R: Future<Output = Result<()>> + Send,
{
    if ctx.storage().exists("vote").await? != 0 {
        fail!("a vote is ongoing already")
    }

    let trig_key = format!("vote:trigger:{key}");

    let mut tx = ctx.storage().create_transaction();
    tx.sadd(&trig_key, &*ctx.message().sender.id).forget();
    tx.scard(&trig_key).queue();

    let triggerers = tx.execute::<usize>().await?;
    if triggerers == 1 {
        // fresh trigger
        tracing::info!(trig_key, "vote trigger started");
        ctx.storage()
            .pexpire(
                &trig_key,
                vote_config.trigger_interval.as_millis() as _,
                ExpireOption::Nx,
            )
            .await?;
        return Ok(());
    }

    if triggerers != ctx.config().vote_trigger_people {
        return Ok(());
    }

    let wall_entry = ctx.status_wall().allocate().await;

    let data = serde_json::to_string(&VoteData {
        key: (&key).into(),
        title: (&wall_title.0).into(),
        chat_title: (&chat_title).into(),
        wall: wall_entry.key(),
    })?;
    let ongoing: Option<String> = ctx
        .storage()
        .set_get_with_options("vote", data, SetCondition::NX, SetExpiration::None, false)
        .await?;

    let yes_key = format!("vote:{}:yes", key);
    let no_key = format!("vote:{}:no", key);

    let mut tx = ctx.storage().create_transaction();
    // only delete the trigger set after we tried to start the vote
    tx.del(&trig_key).forget();
    // cleanup any existing votes just in case idk
    tx.del(&yes_key).forget();
    tx.del(&no_key).forget();
    tx.execute::<[(); 0]>().await?;

    if let Some(ongoing) = ongoing {
        tracing::info!(key, ongoing, "already voting, skip");
        return Ok(());
    }

    wall_entry
        .set_top(html! { "Vote started (type yes~/no~):\n"(wall_title) })
        .await;

    let mut tx = ctx.storage().create_transaction();
    tx.scard(&yes_key).queue();
    tx.scard(&no_key).queue();
    tx.del(yes_key).forget();
    tx.del(no_key).forget();
    tx.del("vote").forget();

    let (yes, no): (usize, usize) = tx.execute().await?;
    let (yes, no) = (yes as f32, no as f32);

    let sum = yes + no;

    if yes / sum >= ctx.config().vote_min_ratio {
        tracing::info!(key, "vote passed");
        wall_entry.set_top("Vote passed!").await;
        ctx.send(format!("Vote '{chat_title}' passed! :)")).await?;
        action.await?;
    } else {
        tracing::info!(key, "vote failed");
        wall_entry.set_top("Vote failed!").await;
        ctx.send(format!("Vote '{chat_title}' failed! :(")).await?;
    }

    // leave the status wall entry there for a bit
    sleep(Duration::from_secs(5)).await;

    Ok(())
}

/// If several(!) people run this command with the same Twitch **login** (this
/// is important, use their login, not display name) as an argument - this will
/// start a vote to send that user to the shadow realm.
///
/// The vote has to have >=70% yes votes for them to get yeeted.
///
/// By "shadow realm" I mean that their commands will be ignored - initially
/// for an hour, but if they get voted for the second time then _forever_,
/// unless a moderator clears them.
///
/// This is obviously to deal with trolls and other problematic users. Channel
/// moderators and above are immune.
///
/// Be aware that there can be only one vote at a time and this command has a
/// large per-user cooldown, so dont waste it.
#[command(sender_gate = 5m)]
async fn votekick(ctx: CommandContext, chatter: Required<Chatter>) -> Result<()> {
    if ctx
        .storage()
        .exists(format!("kick:begone:{chatter}"))
        .await?
        != 0
    {
        fail!("already banished")
    }

    let config = ctx.config().kick_votes.clone();

    let ctx = &ctx;
    vote_trigger(
        ctx,
        format!("kick:{chatter}"),
        html! { "Banish " span style="color: #E38AF0" { (chatter.login) } },
        format!("Banish {}", chatter.login),
        config,
        async move { super::moderation::do_banish(ctx, &chatter, None).await },
    )
    .await
}

/// This allows to start a vote to restart the game in case it crashed or got
/// stuck or whatever, and I'm not there to fix it.
///
/// This command could fix a lot of issues as it is fully kills and restarts
/// everything (like the OBS capture might get broken etc).
///
/// Needs several(!) people to run this to start the vote, and the vote itself
/// has to have >=70% yes votes.
///
/// Be aware that there can be only one vote at a time and this command has a
/// large per-user cooldown, so dont waste it.
#[command(sender_gate = 5m, NoitaControl)]
async fn vote_restart(ctx: CommandContext) -> Result<()> {
    let config = ctx.config().restart_votes.clone();
    vote_trigger(
        &ctx,
        "restart".into(),
        html! { span style="color: orange" { "Restart the game" } },
        "Restart the game".into(),
        config,
        ctx.restart(),
    )
    .await
}

/// (Re)start the game immediately. This is the same as a successful
/// `vote-restart~`, but instant.
#[command(permission = Verified, global_gate = 2m, NoitaControl)]
async fn restart(ctx: CommandContext) -> Result<()> {
    ctx.restart().await
}

/// This allows to start a vote to restart the game and ***delete the world***,
/// in case you got soft-locked or the world is corrupted etc.
///
/// Needs several(!) people to run this to start the vote, and the vote itself
/// has to have >=70% yes votes.
///
/// Be aware that there can be only one vote at a time and this command has a
/// large per-user cooldown, so dont waste it.
#[command(sender_gate = 5m, NoitaControl)]
async fn vote_reset(ctx: CommandContext) -> Result<()> {
    let config = ctx.config().reset_votes.clone();
    vote_trigger(
        &ctx,
        "reset".into(),
        html! { span style="color: red" { "Reset the game" } },
        "Reset the game".into(),
        config,
        ctx.reset(),
    )
    .await
}

/// Reset the game (deleting the current world) immediately. This is the same
/// as a successful `vote-reset~`, but instant.
#[command(permission = Moderator, global_gate = 2m, NoitaControl)]
async fn reset(ctx: CommandContext) -> Result<()> {
    ctx.reset().await
}
