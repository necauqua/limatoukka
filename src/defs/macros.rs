use std::{
    num::NonZero,
    time::{Duration, Instant},
};

use maud::html;
use neca_cmd::Statement;
use rustis::{
    client::BatchPreparedCommand,
    commands::{GenericCommands, HashCommands},
};

use crate::{
    commands::{
        CommandResult,
        args::{Chatter, InRange, RawScript, RestOfArgs, Script},
        command,
        runner::{CommandError, EvalError},
    },
    context::cmd::CommandContext,
    fail,
};

/// Stores a string as a personal macro.
///
/// Commands can accept complicated strings if you put them in quotes like so:
/// ```tpn
/// macro-record:hop:"wait~ up~ wait~ up~ wait~ up~ wait~ up~"~
/// ```
#[command(shortcode=mr)]
async fn macro_record(ctx: CommandContext, name: String, script: RawScript) -> CommandResult {
    if name.len() > 8192 || script.stmt.original.len() > 8192 {
        fail!("name or script too long (max 8192 chars)");
    }
    let name = name.to_lowercase();

    let mut tx = ctx.storage().create_transaction();
    let key = format!("macros:{}", ctx.shared.owner);
    tx.hset(&key, (&name, script.stmt.original)).forget();
    tx.hlen(&key).queue();
    let len: usize = tx.execute().await?;
    if len == 1000 {
        ctx.storage().hdel(key, name).await?;
        ctx.reply("too many macros brother, this incident will be investigated Stare".into())
            .await?;
    } else {
        ctx.reply_buffered(format!("recorded macro `{name}`"))
            .await?;
    }
    Ok(())
}

/// Deletes a macro created with `macro-record~`.
#[command(shortcode=md)]
async fn macro_delete(ctx: CommandContext, name: String) -> CommandResult {
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
async fn global_macro_record(
    ctx: CommandContext,
    name: String,
    script: RawScript,
) -> CommandResult {
    if name.len() > 8192 || script.stmt.original.len() > 8192 {
        fail!("name or script too long (max 8192 chars)");
    }
    let name = name.to_lowercase();
    ctx.storage()
        .hset("macros:global", (&name, &script.stmt.original))
        .await?;
    ctx.reply_buffered(format!("recorded global macro `{name}`"))
        .await?;
    Ok(())
}

/// Deletes a macro created with `global-macro-record~`.
#[command(permission=Moderator, shortcode=gmd)]
async fn global_macro_delete(ctx: CommandContext, name: String) -> CommandResult {
    if ctx.storage().hdel("macros:global", &name).await? == 0 {
        fail!("no macro named `{name}`");
    }
    ctx.reply_buffered(format!("deleted global macro `{name}`"))
        .await?;
    Ok(())
}

async fn macro_get(
    ctx: &CommandContext,
    name: &str,
    chatter: Chatter,
) -> Result<String, CommandError> {
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
async fn macro_print(ctx: CommandContext, name: String, chatter: Chatter) -> CommandResult {
    ctx.reply(macro_get(&ctx, &name, chatter).await?).await?;
    Ok(())
}

/// Replies with the stored global macro.
#[command(sender_gate=5s, shortcode=gmp)]
async fn global_macro_print(ctx: CommandContext, name: String) -> CommandResult {
    let script: Option<String> = ctx.storage().hget("macros:global", &name).await?;
    let Some(script) = script else {
        fail!("no global macro named `{name}`");
    };
    ctx.reply(script).await?;
    Ok(())
}

/// List macros you/given chatter has recorded.
#[command(sender_gate=5s, shortcode=ml)]
async fn macro_list(ctx: CommandContext, chatter: Chatter) -> CommandResult {
    let keys: Vec<String> = ctx.storage().hkeys(format!("macros:{chatter}")).await?;
    ctx.reply(keys.join(", ")).await?;
    Ok(())
}

/// List global macros recorded.
#[command(sender_gate=5s, shortcode=gml)]
async fn global_macro_list(ctx: CommandContext) -> CommandResult {
    let mut keys: Vec<(String, String)> = ctx.storage().hgetall("macros:global").await?;
    keys.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));

    // we assume the global macro list will always overflow the single message length
    // and be rendered as HTML lmao
    let reply = html! {
        style {
            "td { padding: 0.5rem; }"
            "td:first-child { white-space: nowrap; }"
        }
        table {
            tr {
                th { "Global Macro" }
                th { "What it does" }
            }
            @for (name, script) in keys {
                tr {
                    td { (name) }
                    td { (script) }
                }
            }
        }
    };
    ctx.reply(reply.0).await?;
    Ok(())
}

/// Copy someones macro to yourself.
#[command]
async fn yoink(
    ctx: CommandContext,
    name: String,
    chatter: Chatter,
    rename: Option<String>,
) -> CommandResult {
    let script = macro_get(&ctx, &name, chatter).await?;
    macro_record(
        ctx,
        rename.unwrap_or(name),
        RawScript {
            stmt: Statement::parse(&script),
        },
    )
    .await
}

/// Run the macro.
///
/// Macros can call other macros, but there is a recursion limit!
///
/// Also you can run a macro recorded by someone else by appending their login
/// as the second argument.
#[command(shortcode=q, NoWall)]
async fn r#macro(
    ctx: CommandContext,
    name: String,
    chatter: Chatter,
    rest: RestOfArgs,
) -> CommandResult {
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
    let stmt = Statement::parse(&script);

    let status = html! {
        span style="color: #E38AF0" { (ctx.message().sender.name) } ": macro:" (name) " " (ctx.nesting_str())
    };
    let _guard = ctx.status_wall().push(status).await;

    ctx.runner()
        .eval(ctx.nest_macro(chatter, global, args).await?, stmt)
        .await?;

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
#[command(NoWall)]
async fn repeat(ctx: CommandContext, times: InRange<0, 1000>, script: RawScript) -> CommandResult {
    let times = times.get();

    let entry = ctx.status_wall().allocate().await;

    for i in (1..=times).rev() {
        if ctx.inc_repeats() > REPEAT_LIMIT {
            fail!("total repeat limit exceeded");
        }
        entry.set(html! {
            span style="color: #E38AF0" { (ctx.message().sender.name) } ": repeat:" (i) " " (ctx.nesting_str())
        }).await;

        ctx.runner()
            .eval(
                ctx.nest_repeat(NonZero::new(times - i + 1).unwrap()),
                script.stmt.clone(),
            )
            .await?;
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
#[command(shortcode=g, NoWall)]
async fn group(
    ctx: CommandContext,
    script: Script,
    custom_status_name: Option<String>,
) -> CommandResult {
    let name = match custom_status_name {
        Some(text) => html! { "group:" span style="color: #CCCCFF" { (text) } },
        None => html! { (ctx.token) },
    };
    let status = html! {
        span style="color: #E38AF0" { (ctx.message().sender.name) } ": " (name) " " (ctx.nesting_str())
    };
    let _guard = ctx.status_wall().push(status).await;

    ctx.runner().eval(ctx.nest(), script.stmt).await?;

    Ok(())
}

/// Similarly to `group`, executes a given string without counting towards
/// limits.
///
/// The difference is that any script errors are ignored, and this command
/// always succeeds, without preventing the repeats from continuing or setting
/// last-error.
#[command(NoWall)]
async fn r#try(ctx: CommandContext, script: Script) -> CommandResult {
    let status = html! {
        span style="color: #E38AF0" { (ctx.message().sender.name) } ": " (ctx.token) " " (ctx.nesting_str())
    };
    let _guard = ctx.status_wall().push(status).await;

    match ctx.runner().eval(ctx.nest(), script.stmt).await {
        Ok(()) => {}
        Err(e @ (EvalError::Interrupt | EvalError::RecursionLimit)) => return Err(e.into()),
        Err(ref e @ EvalError::CommandErrors(ref errors)) => {
            if errors.iter().any(|e| e.error.is_internal()) {
                fail!("script had internal errors: {e}")
            }
        }
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
#[command(permission=Subscriber, NoWall)]
async fn r#loop(ctx: CommandContext, script: RawScript) -> CommandResult {
    let entry = ctx.status_wall().allocate().await;

    let mut i = 0;
    loop {
        i += 1;
        ctx.reset_repeats();
        entry.set(html! {
            span style="color: #E38AF0" { (ctx.message().sender.name) } ": loop:" (i) " " (ctx.nesting_str())
        }).await;
        let start = Instant::now();

        ctx.runner()
            .eval(
                ctx.nest_repeat(NonZero::new(i).unwrap()),
                script.stmt.clone(),
            )
            .await?;

        if start.elapsed() < Duration::from_millis(100) {
            fail!("loop iteration took less than 100ms");
        }
    }
}

/// Evaluates and prints a given math expression. Mostly useful for debugging
/// my calculator bugs :(
#[command(sender_gate=3s)]
async fn math(ctx: CommandContext, value: i64) -> CommandResult {
    ctx.reply(format!("= {value}")).await?;
    Ok(())
}

/// Stores text that will be replaced in any commands you
/// call (including macros) if you reference it as `%name`.
///
/// Note that while this command accepts text, an argument wrapped in
/// parenthesis - without quotes or braces - will be passed through the
/// calculator first as if the command expected a number.
#[command]
async fn set(ctx: CommandContext, name: String, value: Option<String>) -> CommandResult {
    let value = value.unwrap_or_default();
    if name.len() > 8192 || value.len() > 8192 {
        fail!("name or value too long (max 8192 chars)");
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
            .await?;
    } else {
        ctx.vars.insert(name, value);
    }
    Ok(())
}

/// A moderator-only version of `set~` that sets a global variable instead of a
/// personal one.
///
/// Global variables will be replaced by anyone referencing them as `%name`,
/// unless they have their own personal variable with the same name, which
/// would take precedence.
#[command(permission = Moderator)]
async fn global_set(ctx: CommandContext, name: String, value: Option<String>) -> CommandResult {
    let value = value.unwrap_or_default();
    if name.len() > 8192 || value.len() > 8192 {
        fail!("name or value too long (max 8192 chars)");
    }
    let name = name.to_lowercase();

    ctx.storage().hset("vars:global", (&name, &value)).await?;

    // only insert into cache if was not set there before
    ctx.vars.entry(name).or_insert(value);

    Ok(())
}

/// Similar to `set~`, but the text is only stored for the chat message being executed.
#[command]
async fn r#let(ctx: CommandContext, name: String, value: Option<String>) -> CommandResult {
    let value = value.unwrap_or_default();
    if name.len() > 8192 || value.len() > 8192 {
        fail!("name or value too long (max 8192 chars)");
    }
    let name = name.to_lowercase();
    ctx.vars.insert(name, value);
    Ok(())
}

/// Deletes a variable. Can delete more than one at once.
#[command]
async fn del(ctx: CommandContext, names: RestOfArgs) -> CommandResult {
    if names.is_empty() {
        fail!("no names given");
    }
    let names = names.get(&ctx).await?;

    match ctx
        .storage()
        .hdel(
            format!("vars:{}", ctx.shared.owner),
            names
                .iter()
                .filter_map(|n| n.as_deref())
                .collect::<Vec<_>>(), // ugh
        )
        .await?
    {
        // 0 => fail!("no vars deleted"),
        0 | 1 => {}
        n => ctx.reply(format!("{n} vars deleted")).await?,
    }
    for name in names.into_iter().flatten() {
        ctx.vars.remove(&name);
    }

    Ok(())
}

/// Lists all of your variables.
#[command(sender_gate=5s)]
async fn list_vars(ctx: CommandContext) -> CommandResult {
    let keys: Vec<String> = ctx
        .storage()
        .hkeys(format!("vars:{}", ctx.shared.owner))
        .await?;
    ctx.reply(keys.join(", ")).await?;
    Ok(())
}

/// A debug command that replies with the value of the given variable.
#[command(sender_gate=5s)]
async fn get(ctx: CommandContext, name: String) -> CommandResult {
    ctx.reply(match ctx.vars.get(&name) {
        Some(value) => format!("{name} = {}", *value),
        None => format!("no variable named `{name}`"),
    })
    .await?;
    Ok(())
}

/// Clears all of your variables.
#[command]
async fn clear(ctx: CommandContext) -> CommandResult {
    ctx.storage()
        .del(format!("vars:{}", ctx.shared.owner))
        .await?;

    ctx.vars.clear();

    Ok(())
}

/// Runs the `then` script if given values are equal, otherwise runs the `else` script if given.
#[command]
async fn if_eq(
    ctx: CommandContext,
    a: String,
    b: String,
    then: Script,
    r#else: Option<Script>,
) -> CommandResult {
    if a == b {
        ctx.runner().eval(ctx.nest(), then.stmt).await?;
    } else if let Some(r#else) = r#else {
        ctx.runner().eval(ctx.nest(), r#else.stmt).await?;
    }
    Ok(())
}

/// Runs the `then` script if given values are not equal, otherwise runs the `else` script if given.
#[command]
async fn if_ne(
    ctx: CommandContext,
    a: String,
    b: String,
    then: Script,
    r#else: Option<Script>,
) -> CommandResult {
    if a != b {
        ctx.runner().eval(ctx.nest(), then.stmt).await?;
    } else if let Some(r#else) = r#else {
        ctx.runner().eval(ctx.nest(), r#else.stmt).await?;
    }
    Ok(())
}

macro_rules! numeric_if {
    ($(#[doc = $doc:literal] $name:ident, $cond:tt)*) => {
        $(
            #[doc = $doc]
            #[command]
            async fn $name(
                ctx: CommandContext,
                a: i64,
                b: i64,
                then: Script,
                r#else: Option<Script>,
            ) -> CommandResult {
                if a $cond b {
                    ctx.runner().eval(ctx.nest(), then.stmt).await?;
                } else if let Some(r#else) = r#else {
                    ctx.runner().eval(ctx.nest(), r#else.stmt).await?;
                }
                Ok(())
            }
        )*
    };
}

numeric_if! {
    /// Runs the `then` script if the first number is less than the second, otherwise runs the `else` script if given.
    if_lt, <
    /// Runs the `then` script if the first number is less than or equal to the second, otherwise runs the `else` script if given.
    if_le, <=
    /// Runs the `then` script if the first number is greater than the second, otherwise runs the `else` script if given.
    if_gt, >
    /// Runs the `then` script if the first number is greater than or equal to the second, otherwise runs the `else` script if given.
    if_ge, >=
}
