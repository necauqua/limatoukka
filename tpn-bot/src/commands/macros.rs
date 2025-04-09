use std::borrow::Cow;

use anyhow::Result;
use rustis::{
    client::BatchPreparedCommand,
    commands::{HashCommands, StringCommands},
};

use crate::fail;

use super::{args::InRange, command, context::CommandContext, parsing::CommandMessage, runner};

fn validate(script: &str) -> Result<()> {
    if CommandMessage::parse(&script)
        .parallel
        .iter()
        .any(|seq| seq.iter().any(|cmd| cmd.name == "macro" || cmd.name == "q"))
    {
        fail!("macros cannot call macros");
    }
    Ok(())
}

/// Stores a string as a personal macro.
///
/// Note that macros **cannot** call other macros! This will fail if you try.
///
/// Commands can accept complicated strings if you put them in quotes like so:
/// ```tpn
/// macro-record:hop:"wait~ up~ wait~ up~ wait~ up~ wait~ up~"~
/// ```
#[command]
async fn macro_record(ctx: CommandContext, name: String, script: String) -> Result<()> {
    validate(&script)?;
    let mut tx = ctx.storage.create_transaction();
    let key = format!("macros:{}", ctx.message.sender.id);
    tx.hset(&key, (&name, script)).forget();
    tx.hlen(&key).queue();
    let len: usize = tx.execute().await?;
    if len == 1000 {
        ctx.storage.hdel(key, name).await?;
        ctx.reply("too many macros brother, this incident will be investigated".into())
            .await?;
    }
    Ok(())
}

/// Deletes a macro created with `macro-record~`.
#[command]
async fn macro_delete(ctx: CommandContext, name: String) -> Result<()> {
    let key = format!("macros:{}", ctx.message.sender.id);
    if ctx.storage.hdel(key, &name).await? == 0 {
        fail!("no macro named `{name}`");
    }
    Ok(())
}

/// Stores a string as a global macro, meaning it can be used by everyone.
#[command(permission=Moderator)]
async fn global_macro_record(ctx: CommandContext, name: String, script: String) -> Result<()> {
    validate(&script)?;
    ctx.storage.hset("macros:global", (&name, script)).await?;
    Ok(())
}

/// Deletes a macro created with `global-macro-record~`.
#[command(permission=Moderator)]
async fn global_macro_delete(ctx: CommandContext, name: String) -> Result<()> {
    if ctx.storage.hdel("macros:global", &name).await? == 0 {
        fail!("no macro named `{name}`");
    }
    Ok(())
}

async fn macro_get(
    ctx: &CommandContext,
    name: &str,
    login: Option<&str>,
    with_global: bool,
) -> Result<String> {
    let id = match login {
        Some(login) => {
            let id: Option<String> = ctx.storage.get(format!("twitch-users:{login}")).await?;
            match id {
                Some(id) => Cow::Owned(id),
                None => fail!("they never even typed in chat"),
            }
        }
        None => Cow::Borrowed(&*ctx.message.sender.id),
    };
    let script: Option<String> = ctx.storage.hget(format!("macros:{id}"), name).await?;
    match script {
        Some(script) => Ok(script),
        None => {
            if with_global {
                let script: Option<String> = ctx.storage.hget("macros:global", name).await?;
                if let Some(script) = script {
                    return Ok(script);
                }
            }
            fail!("no macro named `{name}`")
        }
    }
}

/// Replies with the stored macro.
///
/// Can peek at other users macros if their login is specified, they're all
/// public here.
#[command(sender_gate=15s)]
async fn macro_print(ctx: CommandContext, name: String, login: Option<String>) -> Result<()> {
    ctx.reply(macro_get(&ctx, &name, login.as_deref(), false).await?)
        .await
}

/// Copy someones macro to yourself.
#[command]
async fn yoink(
    ctx: CommandContext,
    login: String,
    name: String,
    rename: Option<String>,
) -> Result<()> {
    let script = macro_get(&ctx, &name, Some(&login), false).await?;
    macro_record(ctx, rename.unwrap_or(name), script).await
}

/// Run the macro.
///
/// Note that errors in the macro string are lost, `last-error~`
/// will only know that an error happened.
#[command(shortcode=q)]
async fn r#macro(ctx: CommandContext, name: String) -> Result<()> {
    let script = macro_get(&ctx, &name, None, true).await?;
    let command_msg = CommandMessage::parse(&script);
    let errors = runner::eval(&*ctx, command_msg).await;
    if !errors.is_empty() {
        fail!("{} error(s) in macro", errors.len())
    }
    Ok(())
}

/// Execute a given string several times.
///
/// ```tpn
/// jump five times repeat:5:"wait~ up~"~
/// ```
///
/// Like macros, repeats cannot contain other repeats.
#[command]
async fn repeat(ctx: CommandContext, times: InRange<2, 15>, script: String) -> Result<()> {
    let command_msg = CommandMessage::parse(&script);
    if command_msg
        .parallel
        .iter()
        .any(|seq| seq.iter().any(|cmd| cmd.name == "repeat"))
    {
        fail!("repeat cannon contain repeats");
    }

    for _ in 0..times.get() {
        let errors = runner::eval(&*ctx, command_msg.clone()).await;
        if !errors.is_empty() {
            fail!("{} error(s) in repeated expr", errors.len())
        }
    }
    Ok(())
}
