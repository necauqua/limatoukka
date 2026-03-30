use std::{
    num::NonZero,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use maud::html;
use neca_cmd::Statement;

use crate::{
    commands::{
        CommandResult,
        args::{Chatter, InRange, RawScript, RestOfArgs, Script},
        command,
        runner::{CommandError, EvalError},
    },
    context::cmd::CommandContext,
    fail,
    services::{
        status_wall::StatusServiceExt,
        variables::{SetVarError, VarResolution, VarScope, VarType, VariableStorageExt},
    },
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

    match ctx
        .vars()
        .set(
            VarType::Macro,
            VarScope::Personal(&ctx.owner().id),
            &name,
            &script.stmt.original,
        )
        .await
    {
        Ok(()) => {
            ctx.reply_buffered(format!("recorded macro `{name}`"))
                .await?;
            Ok(())
        }
        Err(SetVarError::TooManyVars) => {
            ctx.fail("too many macros brother, this incident will be investigated Stare")
                .await?;
            Ok(())
        }
        Err(SetVarError::Internal(e)) => Err(CommandError::Internal(e)),
    }
}

/// Deletes a macro created with `macro-record~`.
#[command(shortcode=md)]
async fn macro_delete(ctx: CommandContext, name: String) -> CommandResult {
    let name = name.to_lowercase();

    if ctx
        .vars()
        .delete(
            VarType::Macro,
            VarScope::Personal(&ctx.owner().id),
            &[&name],
        )
        .await?
        != 0
    {
        ctx.reply_buffered(format!("deleted macro `{name}`"))
            .await?;
    } else {
        fail!("no macro named `{name}`");
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
    match ctx
        .vars()
        .set(
            VarType::Macro,
            VarScope::Global,
            &name,
            &script.stmt.original,
        )
        .await
    {
        Ok(()) => {
            ctx.reply_buffered(format!("recorded global macro `{name}`"))
                .await?;
            Ok(())
        }
        Err(SetVarError::TooManyVars) => Err(CommandError::Internal(anyhow!(
            "too many vars at the global scope, this should not happen"
        ))),
        Err(SetVarError::Internal(e)) => Err(CommandError::Internal(e)),
    }
}

/// Deletes a macro created with `global-macro-record~`.
#[command(permission=Moderator, shortcode=gmd)]
async fn global_macro_delete(ctx: CommandContext, name: String) -> CommandResult {
    let name = name.to_lowercase();

    if ctx
        .vars()
        .delete(VarType::Macro, VarScope::Global, &[&name])
        .await?
        != 0
    {
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
    match ctx
        .vars()
        .get(VarType::Macro, VarScope::Personal(&chatter.id), name)
        .await?
    {
        Some(script) => Ok(script.replace('\n', "\\n")),
        None => fail!("no macro named `{name}`"),
    }
}

/// Replies with the stored macro.
///
/// Can peek at other users macros if their login is specified, they're all
/// public here.
#[command(sender_gate=5s, shortcode=mp)]
async fn macro_print(ctx: CommandContext, name: String, chatter: Chatter) -> CommandResult {
    let name = name.to_lowercase();
    ctx.reply(macro_get(&ctx, &name, chatter).await?).await?;
    Ok(())
}

/// Replies with the stored global macro.
#[command(sender_gate=5s, shortcode=gmp)]
async fn global_macro_print(ctx: CommandContext, name: String) -> CommandResult {
    let name = name.to_lowercase();

    match ctx
        .vars()
        .get(VarType::Macro, VarScope::Global, &name)
        .await?
    {
        Some(script) => Ok(ctx.reply(script).await?),
        None => fail!("no global macro named `{name}`"),
    }
}

/// List macros you/given chatter has recorded.
#[command(sender_gate=5s, shortcode=ml)]
async fn macro_list(ctx: CommandContext, chatter: Chatter) -> CommandResult {
    ctx.reply(
        ctx.vars()
            .list(VarType::Macro, VarScope::Personal(&chatter.id))
            .await?
            .into_iter()
            .map(|(k, _)| k) // todo: maybe render it like gml~ if it overflows a message
            .collect::<Vec<_>>()
            .join(", "),
    )
    .await?;
    Ok(())
}

/// List global macros recorded.
#[command(sender_gate=5s, shortcode=gml)]
async fn global_macro_list(ctx: CommandContext) -> CommandResult {
    let mut macros = ctx.vars().list(VarType::Macro, VarScope::Global).await?;
    macros.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));

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
            @for (name, script) in macros {
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
    let name = name.to_lowercase();
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
    let name = name.to_lowercase();

    let (script, global) = match ctx
        .vars()
        .resolve(VarType::Macro, &chatter.id, &name)
        .await?
    {
        VarResolution::Personal(script) => (script, false),
        VarResolution::Global(script) => (script, true),
        VarResolution::None => fail!("no macro named `{name}`"),
    };

    let args = rest.get(&ctx).await?;
    let stmt = Statement::parse(&script);

    let wall = ctx.status();
    let _guard = wall.push(html! {
        span style="color: #E38AF0" { (ctx.message().sender.name) } ": macro:" (name) " " (ctx.nesting_str())
    }).await;

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

    let wall = ctx.status();
    let entry = wall.allocate();

    for i in (1..=times).rev() {
        if ctx.inc_repeats() > REPEAT_LIMIT {
            fail!("total repeat limit exceeded");
        }
        entry.set(html! {
            span style="color: #E38AF0" { (ctx.message().sender.name) } ": repeat:" (i) " " (ctx.nesting_str())
        }.0);

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
        None => html! { (ctx.command.token) },
    };
    let wall = ctx.status();
    let _guard = wall.push(html! {
        span style="color: #E38AF0" { (ctx.message().sender.name) } ": " (name) " " (ctx.nesting_str())
    }).await;

    ctx.runner().eval(ctx.nest(), script.stmt).await?;

    Ok(())
}

/// Similarly to `group`, executes a given string without counting towards
/// limits.
///
/// The difference is that any script errors are ignored, and this command
/// always succeeds, without preventing the repeats from continuing or setting
/// last-error.
///
/// The `catch` script, if given, is executed only if the main script errors out.
#[command(NoWall)]
async fn r#try(ctx: CommandContext, script: Script, catch: Option<Script>) -> CommandResult {
    let wall = ctx.status();
    let _guard = wall.push(html! {
        span style="color: #E38AF0" { (ctx.message().sender.name) } ": " (ctx.command.token) " " (ctx.nesting_str())
    }).await;

    match ctx.runner().eval(ctx.nest(), script.stmt).await {
        Ok(()) => {}
        Err(e @ (EvalError::Interrupt | EvalError::RecursionLimit)) => return Err(e.into()),
        Err(ref e @ EvalError::CommandErrors(ref errors)) => {
            if errors.iter().any(|e| e.error.is_internal()) {
                fail!("script had internal errors: {e}")
            } else if let Some(catch) = catch {
                ctx.runner().eval(ctx.nest(), catch.stmt).await?;
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
    let status = ctx.status().allocate();

    let mut i = 0;
    loop {
        i += 1;
        ctx.reset_repeats();
        status.set(html! {
            span style="color: #E38AF0" { (ctx.message().sender.name) } ": loop:" (i) " " (ctx.nesting_str())
        }.0);
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

    match ctx
        .vars()
        .set(
            VarType::Var,
            VarScope::Personal(&ctx.owner().id),
            &name,
            &value,
        )
        .await
    {
        Ok(()) => Ok(()),
        Err(SetVarError::TooManyVars) => {
            ctx.fail("too many variables brother, this incident will be investigated Stare")
                .await?;
            Ok(())
        }
        Err(SetVarError::Internal(e)) => Err(CommandError::Internal(e)),
    }
}

/// A moderator-only version of `set~` that sets a global variable instead of a
/// personal one.
///
/// Global variables will be replaced by anyone referencing them as `%name`,
/// unless they have their own personal variable with the same name, which
/// would take precedence.
#[command(permission = Moderator, GlobalMacroExempt)]
async fn global_set(ctx: CommandContext, name: String, value: Option<String>) -> CommandResult {
    let value = value.unwrap_or_default();
    if name.len() > 8192 || value.len() > 8192 {
        fail!("name or value too long (max 8192 chars)");
    }
    let name = name.to_lowercase();

    match ctx
        .vars()
        .set(VarType::Var, VarScope::Global, &name, &value)
        .await
    {
        Ok(()) => Ok(()),
        Err(SetVarError::TooManyVars) => Err(CommandError::Internal(anyhow!(
            "too many vars at the global scope, this should not happen"
        ))),
        Err(SetVarError::Internal(e)) => Err(CommandError::Internal(e)),
    }
}

/// Similar to `set~`, but the text is only stored for the chat message being executed.
#[command]
async fn r#let(ctx: CommandContext, name: String, value: Option<String>) -> CommandResult {
    let value = value.unwrap_or_default();
    if name.len() > 8192 || value.len() > 8192 {
        fail!("name or value too long (max 8192 chars)");
    }
    ctx.set_local(name.to_lowercase(), value);
    Ok(())
}

/// Deletes a variable. Can delete more than one at once.
#[command]
async fn del(ctx: CommandContext, names: RestOfArgs) -> CommandResult {
    if names.is_empty() {
        fail!("no names given");
    }
    let names = names.get(&ctx).await?;
    let names = names
        .iter()
        .filter_map(|n| n.as_deref())
        .collect::<Vec<_>>(); // ugh

    match ctx
        .vars()
        .delete(VarType::Var, VarScope::Personal(&ctx.owner().id), &names)
        .await?
    {
        // 0 => fail!("no vars deleted"),
        0 | 1 => {}
        n => ctx.reply(format!("{n} vars deleted")).await?,
    }
    for name in names {
        ctx.remove_local(name);
    }

    Ok(())
}

/// Lists all of your variables.
#[command(sender_gate=5s)]
async fn list_vars(ctx: CommandContext) -> CommandResult {
    let keys = ctx
        .vars()
        .list(VarType::Var, VarScope::Personal(&ctx.owner().id))
        .await?
        .into_iter()
        .map(|(k, _)| k)
        .collect::<Vec<_>>();

    ctx.reply(keys.join(", ")).await?;

    Ok(())
}

/// A debug command that replies with the value of the given variable.
#[command(sender_gate=5s)]
async fn get(ctx: CommandContext, name: String) -> CommandResult {
    let name = name.to_lowercase();

    let var = match ctx.local_var(&name) {
        Some(value) => Some(value),
        None => ctx
            .vars()
            .resolve(VarType::Var, &ctx.owner().id, &name)
            .await?
            .into_option(),
    };

    ctx.reply(match var {
        Some(value) => format!(": {name} = {value}"),
        None => format!("no variable named `{name}`"),
    })
    .await?;
    Ok(())
}

/// Clears all of your variables.
#[command]
async fn clear(ctx: CommandContext) -> CommandResult {
    ctx.vars()
        .clear(VarType::Var, VarScope::Personal(&ctx.owner().id))
        .await?;

    ctx.clear_locals();

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
