use std::{
    num::NonZero,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use maud::html;
use neca_cmd::CommandMessage;
use rustis::{
    client::BatchPreparedCommand,
    commands::{GenericCommands, HashCommands, SetCondition, SetExpiration, StringCommands},
};

use crate::{
    commands::{
        args::{Chatter, InRange, RawScript, RestOfArgs, Script},
        command,
        runner::{self, CommandError, CommandInterrupt},
    },
    context::cmd::CommandContext,
    fail,
};

fn print_inner_errors(errors: &[CommandError]) -> String {
    format!(
        "{{ {} }}",
        errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Stores a string as a personal macro.
///
/// Commands can accept complicated strings if you put them in quotes like so:
/// ```tpn
/// macro-record:hop:"wait~ up~ wait~ up~ wait~ up~ wait~ up~"~
/// ```
#[command(shortcode=mr)]
async fn macro_record(ctx: CommandContext, name: String, script: RawScript) -> Result<()> {
    if script.commands.original.len() > 8192 {
        fail!("script too long (max 8192 chars)");
    }
    let name = name.to_lowercase();

    if let Err(errors) = runner::prepare_commands(&ctx, &script.commands).await {
        fail!("script contained errors: {}", print_inner_errors(&errors));
    }

    let mut tx = ctx.storage().create_transaction();
    let key = format!("macros:{}", ctx.shared.owner);
    tx.hset(&key, (&name, script.commands.original)).forget();
    tx.hlen(&key).queue();
    let len: usize = tx.execute().await?;
    if len == 1000 {
        ctx.storage().hdel(key, name).await?;
        ctx.reply("too many macros brother, this incident will be investigated Stare".into())
            .await
    } else {
        ctx.reply_buffered(format!("recorded macro `{name}`")).await
    }
}

/// Deletes a macro created with `macro-record~`.
#[command(shortcode=md)]
async fn macro_delete(ctx: CommandContext, name: String) -> Result<()> {
    let key = format!("macros:{}", ctx.shared.owner);
    if ctx.storage().hdel(key, &name).await? == 0 {
        fail!("no macro named `{name}`");
    } else {
        ctx.reply_buffered(format!("deleted macro `{name}`"))
            .await?;
    }
    Ok(())
}

/// Stores a string as a global macro, meaning it can be used by everyone.
#[command(permission=Moderator, shortcode=gmr)]
async fn global_macro_record(ctx: CommandContext, name: String, script: RawScript) -> Result<()> {
    if script.commands.original.len() > 8192 {
        fail!("script too long (max 8192 chars)");
    }
    let name = name.to_lowercase();
    ctx.storage()
        .hset("macros:global", (&name, &script.commands.original))
        .await?;
    ctx.reply_buffered(format!("recorded global macro `{name}`"))
        .await?;
    Ok(())
}

/// Deletes a macro created with `global-macro-record~`.
#[command(permission=Moderator, shortcode=gmd)]
async fn global_macro_delete(ctx: CommandContext, name: String) -> Result<()> {
    if ctx.storage().hdel("macros:global", &name).await? == 0 {
        fail!("no macro named `{name}`");
    }
    ctx.reply_buffered(format!("deleted global macro `{name}`"))
        .await?;
    Ok(())
}

async fn macro_get(ctx: &CommandContext, name: &str, chatter: Chatter) -> Result<String> {
    let script: Option<String> = ctx
        .storage()
        .hget(format!("macros:{chatter}"), name)
        .await?;
    match script {
        Some(script) => Ok(script),
        None => fail!("no macro named `{name}`"),
    }
}

/// Replies with the stored macro.
///
/// Can peek at other users macros if their login is specified, they're all
/// public here.
#[command(sender_gate=5s, shortcode=mp)]
async fn macro_print(ctx: CommandContext, name: String, chatter: Chatter) -> Result<()> {
    ctx.reply(macro_get(&ctx, &name, chatter).await?).await
}

/// Replies with the stored global macro.
#[command(sender_gate=5s, shortcode=gmp)]
async fn global_macro_print(ctx: CommandContext, name: String) -> Result<()> {
    let script: Option<String> = ctx.storage().hget("macros:global", &name).await?;
    let Some(script) = script else {
        fail!("no global macro named `{name}`");
    };
    ctx.reply(script).await
}

/// List macros you/given chatter has recorded.
#[command(sender_gate=5s, shortcode=ml)]
async fn macro_list(ctx: CommandContext, chatter: Chatter) -> Result<()> {
    let keys: Vec<String> = ctx.storage().hkeys(format!("macros:{chatter}")).await?;
    ctx.reply(keys.join(", ")).await
}

/// List global macros recorded.
#[command(sender_gate=5s, shortcode=gml)]
async fn global_macro_list(ctx: CommandContext) -> Result<()> {
    let keys: Vec<String> = ctx.storage().hkeys("macros:global").await?;
    ctx.reply(keys.join(", ")).await
}

/// Copy someones macro to yourself.
#[command]
async fn yoink(
    ctx: CommandContext,
    name: String,
    chatter: Chatter,
    rename: Option<String>,
) -> Result<()> {
    let script = macro_get(&ctx, &name, chatter).await?;
    macro_record(
        ctx,
        rename.unwrap_or(name),
        RawScript {
            commands: CommandMessage::parse(&script),
        },
    )
    .await
}

/// Run the macro.
///
/// Macros can call other macros, but there is a recursion limit!
///
/// Also you can run a macro recorded by someone else by appending their login
/// as the third argument.
#[command(shortcode=q, no_wall)]
async fn r#macro(
    ctx: CommandContext,
    name: String,
    chatter: Chatter,
    rest: RestOfArgs,
) -> Result<()> {
    let script: Option<String> = ctx
        .storage()
        .hget(format!("macros:{chatter}"), &name)
        .await?;

    let (script, global) = match script {
        Some(script) => (script, false),
        None => {
            let script: Option<String> = ctx.storage().hget("macros:global", &name).await?;
            if let Some(script) = script {
                (script, true)
            } else {
                fail!("no macro named `{name}`")
            }
        }
    };

    let args = rest.get(&ctx).await?;
    let command_msg = CommandMessage::parse(&script);

    let status = html! {
        span style="color: #E38AF0" { (ctx.message().sender.name) } ": macro:" (name) " " (ctx.nesting_str())
    };
    let _guard = ctx.status_wall().push(status).await;

    let errors = runner::eval(ctx.nest_macro(chatter, global, args).await?, command_msg).await;

    if !errors.is_empty() {
        if errors.iter().any(|e| matches!(e, CommandError::Interrupt)) {
            bail!(CommandInterrupt);
        }
        fail!("script errors: {}", print_inner_errors(&errors));
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
/// repetitions, nested in each other or inside of macros etc etc. Once the
/// limit is reached, the command will error out.
#[command(no_wall)]
async fn repeat(ctx: CommandContext, times: InRange<0, 1000>, script: RawScript) -> Result<()> {
    let times = times.get();

    let entry = ctx.status_wall().allocate().await;

    for i in (1..=times).rev() {
        if ctx.inc_repeats() > REPEAT_LIMIT {
            fail!("total repeat limit exceeded");
        }
        entry.set(html! {
            span style="color: #E38AF0" { (ctx.message().sender.name) } ": repeat:" (i) " " (ctx.nesting_str())
        }).await;

        let errors = runner::eval(
            ctx.nest_repeat(NonZero::new(times - i + 1).unwrap()),
            script.commands.clone(),
        )
        .await;

        if !errors.is_empty() {
            if errors.iter().any(|e| matches!(e, CommandError::Interrupt)) {
                bail!(CommandInterrupt);
            }
            fail!("script errors: {}", print_inner_errors(&errors));
        }
    }
    Ok(())
}

/// Executes a given string, basically identical to `repeat:1:"script"`.
///
/// This is useful to group together parallel actions, for example:
/// ```tpn
/// wait:5s~ group:" up~ | left~ "~
/// ```
///
/// Additionally, there is an optional name that you can attach to the group to
/// have it shown on the status wall.
#[command(shortcode=g, no_wall)]
async fn group(
    ctx: CommandContext,
    script: Script,
    custom_status_name: Option<String>,
) -> Result<()> {
    let name = match custom_status_name {
        Some(text) => html! { "group:" span style="color: #CCCCFF" { (text) } },
        None => html! { (ctx.command) },
    };
    let status = html! {
        span style="color: #E38AF0" { (ctx.message().sender.name) } ": " (name) " " (ctx.nesting_str())
    };
    let _guard = ctx.status_wall().push(status).await;

    let errors = runner::eval(ctx.nest(), script.commands).await;
    if !errors.is_empty() {
        if errors.iter().any(|e| matches!(e, CommandError::Interrupt)) {
            bail!(CommandInterrupt);
        }
        fail!("script errors: {}", print_inner_errors(&errors));
    }
    Ok(())
}

/// Similarly to `group`, executes a given string without counting towards
/// limits.
///
/// The difference is that any script errors are ignored, and this command
/// always succeeds, without preventing the repeats from continuing or setting
/// last-error.
#[command(no_wall)]
async fn r#try(ctx: CommandContext, script: Script) -> Result<()> {
    let status = html! {
        span style="color: #E38AF0" { (ctx.message().sender.name) } ": " (ctx.command) " " (ctx.nesting_str())
    };
    let _guard = ctx.status_wall().push(status).await;

    let errors = runner::eval(ctx.nest(), script.commands).await;
    if errors.iter().any(|e| matches!(e, CommandError::Interrupt)) {
        bail!(CommandInterrupt);
    }
    let internal = errors.iter().filter(|e| e.internal()).collect::<Vec<_>>();
    if !internal.is_empty() {
        fail!(
            "script had internal errors: {}",
            print_inner_errors(&errors)
        );
    }

    Ok(())
}

/// Similarly to `group`, executes a given string without counting towards
/// limits.
///
/// The difference is that only one `lock` script can run at a time, if
/// one is already running this command does nothing.
#[command(shortcode=b, no_wall)]
async fn lock(ctx: CommandContext, script: Script) -> Result<()> {
    let exclusive = ctx
        .storage()
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
        "current lock: " span style="color: #E38AF0" { (ctx.message().sender.name) }
    };
    let _guard = ctx.status_wall().push_top(status).await;

    let errors = runner::eval(ctx.nest(), script.commands).await;

    ctx.storage().del("holds:exclusive").await?;

    if !errors.is_empty() {
        if errors.iter().any(|e| matches!(e, CommandError::Interrupt)) {
            bail!(CommandInterrupt);
        }
        fail!("script errors: {}", print_inner_errors(&errors));
    }

    Ok(())
}

/// Repeatedly execute a given script indefinitely - until an interrupt is
/// sent.
///
/// A special condition: a single loop iteration must take at least 100ms,
/// otherwise after the first iteration the command will error out.
///
/// Unless you have a repeat _inside_ of the loop that exceeds it, the loop is
/// not limited by the repeat limit.
///
/// The only way to stop a running loop is `interrupt~`, and putting a loop
/// inside of a loop is obviously pointless.
///
/// ```tpn
/// stalling: repeat:5:" wait~ up~ "~
/// ```
#[command(permission=Subscriber, no_wall)]
async fn r#loop(ctx: CommandContext, script: RawScript) -> Result<()> {
    let entry = ctx.status_wall().allocate().await;

    let mut i = 0;
    loop {
        i += 1;
        ctx.reset_repeats();
        entry.set(html! {
            span style="color: #E38AF0" { (ctx.message().sender.name) } ": loop:" (i) " " (ctx.nesting_str())
        }).await;
        let start = Instant::now();
        let errors = runner::eval(
            ctx.nest_repeat(NonZero::new(i).unwrap()),
            script.commands.clone(),
        )
        .await;
        if !errors.is_empty() {
            if errors.iter().any(|e| matches!(e, CommandError::Interrupt)) {
                bail!(CommandInterrupt);
            }
            fail!("script errors: {}", print_inner_errors(&errors));
        } else if start.elapsed() < Duration::from_millis(100) {
            fail!("loop iteration took less than 100ms");
        }
    }
}

/// Evaluates and prints a given math expression. Mostly useful for debugging
/// my calculator bugs :(
#[command(sender_gate=3s)]
async fn math(ctx: CommandContext, value: i64) -> Result<()> {
    ctx.reply(format!("= {value}")).await
}

/// Stores text that will be replaced in any commands you
/// call (including macros) if you reference it as `%name`.
///
/// Note that while this command accepts text, an argument wrapped in
/// parenthesis - without quotes or braces - will be passed through the
/// calculator first as if the command expected a number.
#[command]
async fn set(ctx: CommandContext, name: String, value: Option<String>) -> Result<()> {
    let value = value.unwrap_or_default();
    if value.len() > 8192 {
        fail!("value too long (max 8192 chars)");
    }
    let name = name.to_lowercase();

    let mut tx = ctx.storage().create_transaction();
    let key = format!("vars:{}", ctx.shared.owner);
    tx.hset(&key, (&name, &value)).forget();
    tx.hlen(&key).queue();
    let len: usize = tx.execute().await?;
    if len == 1000 {
        ctx.storage().hdel(key, name).await?;
        ctx.reply("too many variables brother, this incident will be investigated Stare".into())
            .await
    } else {
        ctx.vars.write().await.insert(name, value);
        Ok(())
    }
}

/// Similar to `set~`, but the text is only stored for the chat message being executed.
#[command]
async fn r#let(ctx: CommandContext, name: String, value: Option<String>) -> Result<()> {
    let value = value.unwrap_or_default();
    if value.len() > 1048576 {
        fail!("value too long (max 1048576 chars)");
    }
    let name = name.to_lowercase();
    ctx.vars.write().await.insert(name, value);
    Ok(())
}

/// Deletes a variable. Can delete more than one at once.
#[command]
async fn del(ctx: CommandContext, names: RestOfArgs) -> Result<()> {
    if names.is_empty() {
        fail!("no names given");
    }
    let names = names.get(&ctx).await?;

    match ctx
        .storage()
        .hdel(
            format!("vars:{}", ctx.shared.owner),
            names.iter().collect::<Vec<_>>(), // ugh
        )
        .await?
    {
        // 0 => fail!("no vars deleted"),
        0 | 1 => {}
        n => ctx.reply(format!("{n} vars deleted")).await?,
    }
    let mut vars = ctx.vars.write().await;
    for name in names {
        vars.remove(&name);
    }

    Ok(())
}

/// Lists all of your variables.
#[command(sender_gate=5s)]
async fn list_vars(ctx: CommandContext) -> Result<()> {
    let keys: Vec<String> = ctx
        .storage()
        .hkeys(format!("vars:{}", ctx.shared.owner))
        .await?;
    ctx.reply(keys.join(", ")).await
}

/// A debug command that replies with the value of the given variable.
#[command(sender_gate=5s)]
async fn get(ctx: CommandContext, name: String) -> Result<()> {
    ctx.reply(match ctx.vars.read().await.get(&name) {
        Some(value) => format!("{name} = {value}"),
        None => format!("no variable named `{name}`"),
    })
    .await
}

/// Clears all of your variables.
#[command]
async fn clear(ctx: CommandContext) -> Result<()> {
    ctx.storage()
        .del(format!("vars:{}", ctx.shared.owner))
        .await?;

    ctx.vars.write().await.clear();

    Ok(())
}
