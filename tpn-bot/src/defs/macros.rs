use std::borrow::Cow;

use anyhow::{Result, bail};
use maud::html;
use rustis::{
    client::BatchPreparedCommand,
    commands::{GenericCommands, HashCommands, SetCondition, SetExpiration, StringCommands},
};

use crate::{
    commands::{
        args::InRange,
        command,
        context::CommandContext,
        parsing::CommandMessage,
        runner::{self, CommandError, CommandInterrupt},
    },
    fail,
};

/// Stores a string as a personal macro.
///
/// Note that macros **cannot** call other macros! This will fail if you try.
///
/// Commands can accept complicated strings if you put them in quotes like so:
/// ```tpn
/// macro-record:hop:"wait~ up~ wait~ up~ wait~ up~ wait~ up~"~
/// ```
#[command(shortcode=mr)]
async fn macro_record(ctx: CommandContext, name: String, script: String) -> Result<()> {
    let mut tx = ctx.storage.create_transaction();
    let key = format!("macros:{}", ctx.message.sender.id);
    tx.hset(&key, (&name, &script)).forget();
    tx.hlen(&key).queue();
    let len: usize = tx.execute().await?;
    if len == 1000 {
        ctx.storage.hdel(key, name).await?;
        ctx.reply("too many macros brother, this incident will be investigated Stare".into())
            .await
    } else {
        ctx.reply(format!("Recorded macro `{name}` as: {script}"))
            .await
    }
}

/// Deletes a macro created with `macro-record~`.
#[command(shortcode=md)]
async fn macro_delete(ctx: CommandContext, name: String) -> Result<()> {
    let key = format!("macros:{}", ctx.message.sender.id);
    if ctx.storage.hdel(key, &name).await? == 0 {
        fail!("no macro named `{name}`");
    } else {
        ctx.reply(format!("Deleted macro `{name}`")).await?;
    }
    Ok(())
}

/// Stores a string as a global macro, meaning it can be used by everyone.
#[command(permission=Moderator, shortcode=gmr)]
async fn global_macro_record(ctx: CommandContext, name: String, script: String) -> Result<()> {
    ctx.storage.hset("macros:global", (&name, &script)).await?;
    ctx.reply(format!("Recorded global macro `{name}` as: {script}"))
        .await?;
    Ok(())
}

/// Deletes a macro created with `global-macro-record~`.
#[command(permission=Moderator, shortcode=gmd)]
async fn global_macro_delete(ctx: CommandContext, name: String) -> Result<()> {
    if ctx.storage.hdel("macros:global", &name).await? == 0 {
        fail!("no macro named `{name}`");
    }
    ctx.reply(format!("Deleted global macro {name}")).await?;
    Ok(())
}

async fn chatter_id<'a>(ctx: &'a CommandContext, login: Option<&str>) -> Result<Cow<'a, str>> {
    match login {
        Some(login) => {
            let id: Option<String> = ctx.storage.get(format!("twitch-users:{login}")).await?;
            match id {
                Some(id) => Ok(Cow::Owned(id)),
                None => fail!("they never even typed in chat"),
            }
        }
        None => Ok(Cow::Borrowed(&*ctx.message.sender.id)),
    }
}

async fn macro_get(
    ctx: &CommandContext,
    name: &str,
    login: Option<&str>,
    with_global: bool,
) -> Result<String> {
    let id = chatter_id(ctx, login).await?;
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
#[command(sender_gate=5s, shortcode=mp)]
async fn macro_print(ctx: CommandContext, name: String, login: Option<String>) -> Result<()> {
    ctx.reply(macro_get(&ctx, &name, login.as_deref(), false).await?)
        .await
}

/// List macros you/given chatter has recorded.
#[command(sender_gate=5s, shortcode=ml)]
async fn macro_list(ctx: CommandContext, login: Option<String>) -> Result<()> {
    let id = chatter_id(&ctx, login.as_deref()).await?;
    let keys: Vec<String> = ctx.storage.hkeys(format!("macros:{id}")).await?;
    ctx.reply(keys.join(", ")).await
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
///
/// Macros can call other macros, but there is a recursion limit!
#[command(shortcode=q, no_wall)]
async fn r#macro(ctx: CommandContext, name: String) -> Result<()> {
    let script = macro_get(&ctx, &name, None, true).await?;
    let command_msg = CommandMessage::parse(&script);

    let status = html! {
        span style="color: #E38AF0" { (ctx.message.sender.name) } ": macro:" (name) " " (ctx.nesting)
    };
    let _guard = ctx.status_wall.push(status).await;

    let errors = runner::eval(&ctx, command_msg, ctx.nesting.nest_macro()).await;
    if !errors.is_empty() {
        if errors.iter().any(|e| matches!(e, CommandError::Interrupt)) {
            bail!(CommandInterrupt);
        }
        fail!("{} error(s) in macro", errors.len())
    }
    Ok(())
}

const REPEAT_LIMIT: u32 = 1000;

/// Execute a given string several times.
///
/// ```tpn
/// jump five times: repeat:5:" wait~ up~ "~
/// ```
///
/// The total limit of repetitions in a given message is 1000! This counts all
/// repetitions, nested or inside of macros etc. Once the limit is reached, the
/// command will error out.
///
/// Like with macros, errors in the evaluated string are lost.
#[command(no_wall)]
async fn repeat(ctx: CommandContext, times: InRange<2, 15>, script: String) -> Result<()> {
    let command_msg = CommandMessage::parse(&script);
    let times = times.get();

    let status = html! {
        span style="color: #E38AF0" { (ctx.message.sender.name) } ": repeat:" (times) " " (ctx.nesting)
    };
    let _guard = ctx.status_wall.push(status).await;

    for _ in 0..times {
        if ctx.inc_repeats() > REPEAT_LIMIT {
            fail!("repeat limit exceeded");
        }
        let errors = runner::eval(&ctx, command_msg.clone(), ctx.nesting.nest()).await;
        if !errors.is_empty() {
            if errors.iter().any(|e| matches!(e, CommandError::Interrupt)) {
                bail!(CommandInterrupt);
            }
            fail!("{} error(s) in repeated expr", errors.len())
        }
    }
    Ok(())
}

/// Executes a given string, identical to what `repeat:1:"script"` could've
/// been if a singular repeat was allowed.
///
/// This is useful to group together parallel actions, for example:
/// ```tpn
/// wait:5s~ group:" up~ | left~ "~
/// ```
///
/// Like with macros, errors in the evaluated string are lost.
///
/// Additionally, there is an optional name that you can attach to the group to
/// have it shown on the status wall.
#[command(shortcode=g, no_wall)]
async fn group(
    ctx: CommandContext,
    script: String,
    custom_status_name: Option<String>,
) -> Result<()> {
    let command_msg = CommandMessage::parse(&script);

    let name = match custom_status_name {
        Some(text) => html! { "group:" span style="color: #CCCCFF" { (text) } },
        None => html! { (ctx.command) },
    };
    let status = html! {
        span style="color: #E38AF0" { (ctx.message.sender.name) } ": " (name) " " (ctx.nesting)
    };
    let _guard = ctx.status_wall.push(status).await;

    let errors = runner::eval(&ctx, command_msg, ctx.nesting.nest()).await;
    if !errors.is_empty() {
        if errors.iter().any(|e| matches!(e, CommandError::Interrupt)) {
            bail!(CommandInterrupt);
        }
        fail!("{} error(s) in grouped expr", errors.len())
    }
    Ok(())
}

/// Similarly to `group`, executes a given string without counting towards
/// limits.
///
/// The difference is that only one `lock` script can run at a time, if
/// one is already running this command does nothing.
#[command(shortcode=b, no_wall)]
async fn lock(ctx: CommandContext, script: String) -> Result<()> {
    let exclusive = ctx
        .storage
        .set_with_options(
            "holds:exclusive",
            "1",
            SetCondition::NX,
            SetExpiration::None,
            false,
        )
        .await?;
    if !exclusive {
        fail!("non-exclusive")
    }

    let status = html! {
        "current lock: " span style="color: #E38AF0" { (ctx.message.sender.name) }
    };
    let _guard = ctx.status_wall.push_top(status).await;

    let command_msg = CommandMessage::parse(&script);
    let errors = runner::eval(&ctx, command_msg, ctx.nesting.nest()).await;

    ctx.storage.del("holds:exclusive").await?;

    if !errors.is_empty() {
        if errors.iter().any(|e| matches!(e, CommandError::Interrupt)) {
            bail!(CommandInterrupt);
        }
        fail!("{} error(s) in lock expr", errors.len())
    }

    Ok(())
}
