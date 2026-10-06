use std::{fmt::Write as _, time::Duration};

use crate::{
    commands::{
        CommandResult, CommandTag,
        args::{Chatter, HoldTime, Required, RestOfArgs},
        command,
    },
    context::{app::InterruptKind, cmd::CommandContext},
    fail,
    integration::justfile::just,
    services::{
        charges::Charges,
        chat_log::{ChatLine, ChatLogServiceExt},
        jev::{Answer, JevService, JevServiceExt, Question},
        messaging::PermissionLevel,
        status_wall::StatusServiceExt,
        storage::StorageServiceExt,
        variables::{VarResolution, VarType, VariableStorageExt},
    },
};
use humantime_serde::re::humantime;
use indexmap::IndexMap;
use maud::html;
use serde_json::{Value, json};
use tokio::time::sleep;

/// A building block for basic static text commands.
///
/// This just makes the bot print the given text, but non-moderators can only call it through global macros.
///
/// So you can call a global macro `discord~` which will resolve to `echo:"discord link etc"~` and print it.
#[command(sender_gate = 5s, permission = Caster, GlobalMacroExempt)]
async fn echo(ctx: CommandContext, text: String) -> CommandResult {
    ctx.send(text).await?;
    Ok(())
}

/// Always errors with the given text.
#[command(permission = Caster, GlobalMacroExempt)]
async fn error(_: CommandContext, text: String) -> CommandResult {
    fail!("{text}")
}

/// Respond with the last error message for user.
///
/// Last error message is set when you run invalid commands, or if the command
/// execution managed to crash somehow. In the latter case, you'll be given the
/// message id - please send it to me to look at logs and fix the issue.
#[command(sender_gate = 3s, shortcode=le)]
async fn last_error(ctx: CommandContext, chatter: Chatter) -> CommandResult {
    ctx.reply(
        ctx.storage()
            .get(&format!("last-error:{chatter}"))
            .await?
            .unwrap_or_else(|| {
                let whom = chatter.them(ctx.owner(), "your", "their");
                format!("No errors in {whom} last message")
            }),
    )
    .await?;
    Ok(())
}

/// Check if noita.exe process is present, aka not dead.
#[command(sender_gate = 1m, NoitaData)]
async fn is_game_running(ctx: CommandContext) -> CommandResult {
    ctx.reply(
        if just("is-game-running", &[])?.get().await?.is_ok() {
            "It is running currently, yes"
        } else {
            "The game is NOT running"
        }
        .into(),
    )
    .await?;
    Ok(())
}

/// Wait for a specified duration milliseconds.
///
/// Very useful for multi-command messages.
#[command(shortcode=w, NoWall)]
async fn wait(ctx: CommandContext, duration: HoldTime<500, 300_000>) -> CommandResult {
    let duration = duration.get();
    tracing::debug!(
        duration.ms = duration.as_millis(),
        "waiting for {}",
        humantime::format_duration(duration)
    );

    let wall = ctx.status();
    let entry = wall.allocate();
    let inner_ctx = ctx.clone();
    let wall_task = tokio::spawn(async move {
        let name = &inner_ctx.message().sender.name;
        let nesting = inner_ctx.nesting_str();
        for i in (1..=duration.as_secs()).rev() {
            let status = html! {
                span style="color: #E38AF0" { (name) } ": wait:" (i) "s " (nesting)
            };
            entry.set(status.into());
            sleep(Duration::from_secs(1)).await;
        }
    });

    ctx.interruptible(sleep(duration))
        .await
        .inspect_err(|_| wall_task.abort())?;

    wall_task.abort();

    Ok(())
}

/// Caster-only uninterruptible wait for a specified duration milliseconds.
#[command(shortcode=fw, NoWall, permission = Caster)]
async fn force_wait(ctx: CommandContext, duration: HoldTime<500, 300_000>) -> CommandResult {
    let duration = duration.get();
    tracing::debug!(
        duration.ms = duration.as_millis(),
        "waiting for {}",
        humantime::format_duration(duration)
    );

    let wall = ctx.status();
    let entry = wall.allocate();
    let inner_ctx = ctx.clone();
    let _wall_task = tokio::spawn(async move {
        let name = &inner_ctx.message().sender.name;
        let nesting = inner_ctx.nesting_str();
        for i in (1..=duration.as_secs()).rev() {
            let status = html! {
                span style="color: #E38AF0" { (name) } ": force-wait:" (i) "s " (nesting)
            };
            entry.set(status.into());
            sleep(Duration::from_secs(1)).await;
        }
    });

    sleep(duration).await;

    // _wall_task.abort();

    Ok(())
}

/// Complete all current holds immediately.
///
/// "Holds" here are referring to all "non-instantaneous" commands that are
/// currently being executed **right now** - so movement, `hold~` and `wait~`.
///
/// This is kind of a niche thing, most likely you need `interrupt~`.
#[command]
async fn r#break(ctx: CommandContext, chatter: Option<Required<Chatter>>) -> CommandResult {
    ctx.interrupt(chatter.as_ref().map(|c| &*c.id), InterruptKind::Break);
    Ok(())
}

/// Stop running all current commands.
///
/// This is similar to `break~`, except the commands following the holds that
/// get completed do not run.
#[command(shortcode=int)]
async fn interrupt(ctx: CommandContext, chatter: Option<Required<Chatter>>) -> CommandResult {
    ctx.interrupt(chatter.as_ref().map(|c| &*c.id), InterruptKind::Interrupt);
    Ok(())
}

/// Interrupt all commands *originating from the current message*.
///
/// On it's own this does nothing, but it can be used to limit the duration of
/// the current message or similar.
#[command]
async fn discard(ctx: CommandContext) -> CommandResult {
    ctx.local_interrupt();
    Ok(())
}

/// Set a bot setting.
///
/// Settings that currently do things are:
///   - `full-stop`: disables processing any commands from non-mods
///   - `nosr`: disables song requests
///
/// If the key argument is prefixed with `-` the setting is removed (value is
/// ignored).
#[command(permission = Moderator)]
async fn setting(ctx: CommandContext, key: String, value: Option<String>) -> CommandResult {
    if let Some(key) = key.strip_prefix("-") {
        ctx.storage().del(&format!("setting:{key}")).await?;
        ctx.reply(format!("Bot setting '{key}' removed")).await?;
    } else {
        ctx.storage()
            .set(&format!("setting:{key}"), value.as_deref().unwrap_or("1"))
            .await?;
        ctx.reply(format!("Bot setting '{key}' set")).await?;
    }
    Ok(())
}

/// Get information about a macro or command.
///
/// If you see someone running some weird command, run
/// `what-is:command:their-name~` to figure out what it was.
#[command(sender_gate = 3s)]
async fn what_is(ctx: CommandContext, name: String, to: Chatter) -> CommandResult {
    let name = name.to_lowercase();

    if let Some(meta) = ctx.runner().get_command(&name)
        && !meta.is(CommandTag::Hidden)
    {
        let mut s = String::new();

        match meta.shortcode {
            Some(shortcode) => {
                write!(&mut s, "`{}` (shortcode `{shortcode}`)", meta.name)
            }
            None => write!(&mut s, "`{}`", meta.name),
        }
        .unwrap();

        s.push_str(" is a ");
        if meta.permission != PermissionLevel::Viewer {
            s.push_str(&format!("{:?}", meta.permission).to_lowercase());
            s.push_str("-level ");
        }
        s.push_str("command");

        if let Some(gate) = meta.sender_gate {
            write!(&mut s, ", sender gate {}", humantime::format_duration(gate)).unwrap();
        }
        if let Some(gate) = meta.global_gate {
            write!(&mut s, ", global gate {}", humantime::format_duration(gate)).unwrap();
        }
        if let Some(cost) = meta.cost {
            write!(&mut s, ", costs {}", Charges::from(cost)).unwrap();
            if meta.free_for != PermissionLevel::Caster {
                write!(&mut s, " (free for {:?}-level and above)", meta.free_for).unwrap();
            }
            if meta.free_if.is_some() {
                s.push_str(" (can be free under some conditions, see the docs)");
            }
        }

        let required = meta
            .args
            .iter()
            .filter(|a| (a.optional)().is_none())
            .count();
        let all = meta.args.len();

        if all == 0 {
            s.push_str(". Takes no arguments");
        } else if required == all {
            if required == 1 {
                write!(&mut s, ". Takes 1 argument").unwrap();
            } else {
                write!(&mut s, ". Takes {all} arguments").unwrap();
            }
        } else {
            write!(&mut s, ". Takes {required}-{all} arguments").unwrap();
        }

        write!(&mut s, " (uq.rs/help#{})", meta.name).unwrap();

        ctx.reply(s).await?;
        return Ok(());
    }

    match ctx.vars().resolve(VarType::Macro, &to.id, &name).await? {
        VarResolution::Personal(script) => {
            let whom = to.them(ctx.owner(), "your", "their");
            ctx.reply(format!("`{name}` is one of {whom} macros: {script}"))
                .await?;
        }
        VarResolution::Global(script) => {
            ctx.reply(format!("`{name}` is a global macro: {script}"))
                .await?;
        }
        _ => {
            ctx.reply(format!("`{name}` is not a macro or command"))
                .await?;
        }
    }

    Ok(())
}

/// The max number of chat messages to give to `ask` as context.
const ASK_CONTEXT_SIZE: u64 = 150;
/// The max age of the chat messages to give to `ask` as context.
const ASK_CONTEXT_AGE: Duration = Duration::from_secs(30 * 60);

/// The `ask` answers, in the order of the reply, with descriptions for the
/// model.
const ASK_ANSWERS: [(&str, &str); 3] = [
    ("Yes", "the chat shows that the answer is yes"),
    (
        "Undetermined",
        "the chat does not give enough information to answer, \
         the chat shows both answers equally, \
         or the question is not a yes/no question",
    ),
    ("No", "the chat shows that the answer is no"),
];

/// The last chat messages before the current one, without the command-only
/// messages and the messages of the bot, oldest first.
async fn ask_chat_context(ctx: &CommandContext) -> anyhow::Result<Vec<ChatLine>> {
    let exclude: &[&str] = match ctx.bot_id() {
        Some(bot_id) => &[bot_id],
        None => &[],
    };
    ctx.chat_logs()
        .recent(
            ASK_CONTEXT_SIZE,
            ASK_CONTEXT_AGE,
            Some(&ctx.message().id),
            exclude,
        )
        .await
}

/// Ask a yes/no question about what is going on in the chat.
#[command(sender_gate = 3s, cost = 0.25)]
async fn ask(ctx: CommandContext, question: String, extra: RestOfArgs) -> CommandResult {
    let question = if extra.is_empty() {
        question
    } else {
        std::iter::once(question)
            .chain(extra.get(&ctx).await?.into_iter().flatten())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let question = question.trim();
    if question.is_empty() {
        fail!("you forgot the question Stare");
    }

    let chat = ask_chat_context(&ctx).await?;
    let answer = answer_question(&*ctx.jev(), &chat, &ctx.message().sender.name, question).await?;

    ctx.reply(answer).await?;
    Ok(())
}

/// Ask Jev to pick one of the [`ASK_ANSWERS`] for the question, and format
/// the reply.
async fn answer_question(
    jev: &dyn JevService,
    chat: &[ChatLine],
    asked_by: &str,
    question: &str,
) -> anyhow::Result<String> {
    let instructions = json!({
        "question": question,
        "asked_by": asked_by,
        "task": "You analyze a Twitch chat. \
            `asked_by` asks you `question` about the chat. \
            The state is the recent chat before the question, oldest message first. \
            Answer the question only from what the chat shows.",
    });
    let criteria: IndexMap<String, Value> = ASK_ANSWERS
        .iter()
        .map(|(answer, desc)| ((*answer).to_owned(), (*desc).into()))
        .collect();

    let answer = jev
        .ask(
            json!(chat),
            Question::Choice {
                instructions,
                criteria,
            },
        )
        .await?;

    let Answer::Choice {
        choice,
        probabilities,
        confidence,
    } = answer
    else {
        anyhow::bail!("unexpected answer type: {answer:?}");
    };
    tracing::debug!(choice, confidence, ?probabilities, "ask answer");

    let percentages = ASK_ANSWERS
        .iter()
        .map(|(answer, _)| {
            let p = probabilities.get(*answer).copied().unwrap_or(0.0);
            format!("{:.0}%", p * 100.0)
        })
        .collect::<Vec<_>>()
        .join(" · ");

    Ok(format!("{choice} ({percentages})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asks Jev with the real recent chat; set `ASK_QUESTION` to change
    /// the question.
    #[tokio::test]
    #[ignore = "manual test"]
    async fn ask_real() -> anyhow::Result<()> {
        use crate::{
            config::Config,
            services::{
                chat_log::{ChatLogService, ChatLogServiceElastic},
                jev::JevServiceTypeSafe,
            },
        };

        let config = Config::load()?;
        let chat_logs = ChatLogServiceElastic::new(
            &config.elastic.url,
            &config.elastic.api_key,
            &config.elastic.index,
        )?;
        let jev = JevServiceTypeSafe::new(&config.jev);

        let chat = chat_logs
            .recent(ASK_CONTEXT_SIZE, Duration::from_secs(30 * 86400), None, &[])
            .await?;
        for line in &chat {
            println!("{}: {}", line.name, line.message);
        }

        let question = std::env::var("ASK_QUESTION").unwrap_or_else(|_| "is chat happy?".into());
        let answer = answer_question(&jev, &chat, "necauqua", &question).await?;
        println!("Q: {question}\nA: {answer}");
        Ok(())
    }
}
