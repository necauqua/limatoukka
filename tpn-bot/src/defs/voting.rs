use std::{fmt::Display, time::Duration};

use anyhow::Result;
use maud::html;
use rustis::{
    client::BatchPreparedCommand,
    commands::{GenericCommands, SetCommands, SetCondition, SetExpiration, StringCommands},
};
use tokio::time::sleep;

use crate::{
    commands::{
        command,
        context::{AppContext, CommandContext},
    },
    fail,
    services::status_wall::EntryKey,
};

async fn sdel(ctx: &AppContext, key: &str) -> Result<usize> {
    let mut tx = ctx.storage.create_transaction();
    tx.scard(key).queue();
    tx.del(key).forget();
    Ok(tx.execute::<usize>().await?)
}

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

async fn vote(ctx: CommandContext, vote: Vote) -> Result<()> {
    let Some(vote_data) = ctx.storage.get::<_, Option<String>>("vote").await? else {
        fail!("no ongoing vote");
    };
    let (vote_key, rest) = vote_data.split_once("|").unwrap();
    let (title, wall_key) = rest.split_once("|").unwrap();
    let wall_key = EntryKey(wall_key.parse().unwrap());

    let key = format!("vote:{vote_key}:{vote}");
    let key_inv = format!("vote:{vote_key}:{}", vote.inverse());
    let (sadd, srem) = tokio::join!(
        ctx.storage.sadd(&key, &ctx.message.sender.id),
        ctx.storage.srem(&key_inv, &ctx.message.sender.id),
    );
    sadd?;
    srem?;

    let (count, inv_count) = tokio::join!(ctx.storage.scard(&key), ctx.storage.scard(&key_inv),);
    let (yes, no) = vote.lambda(count?, inv_count?);
    let percentage = (yes as f64 / (yes + no) as f64) * 100.0;

    let status = format!(
        "{title}:<br>{yes}/{no} ({percentage:.2}%, need {:.0}%)",
        ctx.config.voting.vote_min_ratio * 100.0
    );
    ctx.status_wall.set(wall_key, status).await;

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

async fn vote_trigger<F, R>(
    ctx: CommandContext,
    key: String,
    title: String,
    vote_time: Duration,
    action: F,
) -> Result<()>
where
    R: Future<Output = Result<()>> + Send + 'static,
    F: FnOnce(AppContext) -> R + Send + 'static,
{
    if !ctx
        .sender_command_gate(Duration::from_secs(ctx.config.voting.trigger_gate_secs))
        .await?
    {
        return Ok(());
    }
    if ctx.storage.exists("vote").await? != 0 {
        fail!("a vote is ongoing already")
    }

    let trig_key = format!("vote:trigger:{key}");

    let mut tx = ctx.storage.create_transaction();
    tx.sadd(&trig_key, &*ctx.message.sender.id).forget();
    tx.scard(&trig_key).queue();

    if tx.execute::<usize>().await? > 1 {
        // added to an ongoing trigger
        return Ok(());
    }

    tracing::info!(trig_key, "vote trigger started");

    ctx.schedule(
        Duration::from_secs(ctx.config.voting.trigger_interval_secs),
        move |ctx| async move {
            let triggerers = sdel(&ctx, &trig_key).await?;
            if triggerers < ctx.config.voting.trigger_min_people as _ {
                tracing::info!(key, "not enough people triggered, skip");
                return Ok(());
            }

            let wall_entry = ctx.status_wall.allocate().await;

            let data = format!("{key}|{title}|{}", wall_entry.0);
            let ongoing: Option<String> = ctx
                .storage
                .set_get_with_options("vote", data, SetCondition::NX, SetExpiration::None, false)
                .await?;
            if let Some(ongoing) = ongoing {
                tracing::info!(key, ongoing, "already voting, skip");
                return Ok(());
            }

            let status = html! {
                "Vote started (type yes~/no~): " span style="text-decoration: underline dotted red" { (title) }
            };
            ctx.status_wall.set(wall_entry, status.0).await;
            let _guard = ctx.status_wall.guard(wall_entry);

            sleep(vote_time).await;

            let yes_key = format!("vote:{key}:yes");
            let no_key = format!("vote:{key}:no");
            let mut tx = ctx.storage.create_transaction();
            tx.scard(&yes_key).queue();
            tx.del(yes_key).forget();
            tx.scard(&no_key).queue();
            tx.del(no_key).forget();
            tx.del("vote").forget();

            let (yes, no): (usize, usize) = tx.execute().await?;
            let (yes, no) = (yes as f32, no as f32);

            let sum = yes + no;

            if yes / sum > ctx.config.voting.vote_min_ratio {
                tracing::info!(key, "vote passed");
                ctx.status_wall.set(wall_entry, "Vote passed!".into()).await;
                ctx.send(format!("Vote '{title}' passed! :)")).await?;
                action(ctx).await?;
            } else {
                tracing::info!(key, "vote failed");
                ctx.status_wall.set(wall_entry, "Vote failed!".into()).await;
                ctx.send(format!("Vote '{title}' failed! :(")).await?;
            }

            // leave the status wall entry there for a bit
            sleep(Duration::from_secs(5)).await;

            Ok(())
        },
    );

    Ok(())
}

/// If several(!) people run this command with the same Twitch **login** (this
/// is important, use their login, not display name) as an argument - this will
/// start a vote to send that user to the shadow realm.
///
/// By "shadow realm" I mean that their commands will be ignored - initially
/// for an hour, but if they get voted again then _forever_, unless I
/// personally clear them.
///
/// This is obviously to deal with trolls and other problematic users.
#[command]
async fn votekick(ctx: CommandContext, login: String) -> Result<()> {
    if ctx
        .storage
        .get::<_, Option<String>>(format!("kick:protected:{login}"))
        .await?
        .is_some()
    {
        fail!("this user is protected")
    }

    let Some(id) = ctx
        .storage
        .get::<_, Option<String>>(format!("twitch-users:{login}"))
        .await?
    else {
        fail!("target never typed in chat, lmao")
    };

    let vote_time = Duration::from_secs(ctx.config.voting.kick_vote_time);
    vote_trigger(
        ctx,
        format!("kick:{id}"),
        format!("Banish {login}"),
        vote_time,
        move |ctx| async move {
            let thin_ice = format!("kick:thin-ice:{id}");
            if ctx.storage.del(&thin_ice).await? != 0 {
                // yeet em
                ctx.storage.set(format!("kick:begone:{id}"), "1").await?;
                tracing::info!(id, login, "sent to shadow realm");
                return Ok(());
            }

            ctx.storage.set(&thin_ice, "1").await?;
            ctx.storage
                .set_with_options(
                    format!("kick:begone:{id}"),
                    "1",
                    SetCondition::None,
                    SetExpiration::Ex(ctx.config.first_time_kick_secs),
                    false,
                )
                .await?;
            tracing::info!(id, login, "temporarily sent to shadow realm");

            Ok(())
        },
    )
    .await
}

/// Check if a user was yeeted into the shadow realm. Per-user 15s cooldown.
///
/// They will or will not return depending on if it was their first offence,
/// and if they dipped their toes in the shadow realm they're on thin ice.
///
/// If _you_ are yeeted, the bot ignores you utterly, so this won't work
/// ¯\\\_(ツ)_/¯.
#[command]
async fn shadowbanned(ctx: CommandContext, login: String) -> Result<()> {
    if !ctx.sender_command_gate(Duration::from_secs(15)).await? {
        return Ok(());
    }
    let Some(id) = ctx
        .storage
        .get::<_, Option<String>>(format!("twitch-users:{login}"))
        .await?
    else {
        return ctx
            .reply("They never even typed in chat. Or don't exist, who knows 🤷".into())
            .await;
    };

    let (thin_ice, begone) = tokio::join!(
        ctx.storage.exists(format!("kick:thin-ice:{id}")),
        ctx.storage.exists(format!("kick:begone:{id}")),
    );
    let (thin_ice, begone) = (thin_ice?, begone?);
    if begone != 0 {
        if thin_ice != 0 {
            ctx.reply("In the shadow realm, but they will return..".into())
                .await?;
        } else {
            ctx.reply("In the shadow realm, not coming back lmao".into())
                .await?;
        }
    } else if thin_ice != 0 {
        ctx.reply("They're on thin ice".into()).await?;
    } else {
        ctx.reply("They're good".into()).await?;
    }

    Ok(())
}
