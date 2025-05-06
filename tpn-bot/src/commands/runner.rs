use std::any::Any;

use anyhow::Result;
use humantime_serde::re::humantime;
use lazy_regex::regex_replace_all;
use maud::html;
use neca_cmd::{CommandExpr, CommandMessage};
use opentelemetry::trace::Status;
use rustis::{
    client::BatchPreparedCommand,
    commands::{GenericCommands, HashCommands, StringCommands},
};
use thiserror::Error;
use tokio::task::JoinSet;
use tracing::{Instrument, Span, debug_span};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::{
    context::{
        app::AppContext,
        cmd::{CommandDescriptor, CommandToken},
        eval::EvalContext,
        msg::MessageContext,
    },
    services::messaging::{Message, PermissionLevel},
};

use super::{
    CommandContext, CommandFuture, CommandRegistration,
    args::{Args, ExtractorError},
};

pub async fn receive_message(ctx: AppContext, message: Message) -> Result<()> {
    let s = &message.sender;

    if s.login == ctx.twitch().bot() {
        return Ok(());
    }

    if message.sender.level < PermissionLevel::Moderator {
        let stop_count = ctx
            .storage()
            .exists(["flags:full-stop", &format!("kick:begone:{}", s.id)])
            .await?;
        if stop_count != 0 {
            return Ok(());
        }
    }

    tracing::debug!("processing message");

    let command_msg = CommandMessage::parse(&message.text);
    if command_msg.is_empty() {
        tracing::trace!("no commands");
        return Ok(());
    }

    let ctx = EvalContext::new(MessageContext::new(ctx.clone(), message));

    let errors = eval(&ctx, command_msg).await;
    let error_key = format!("last-error:{}", ctx.message().sender.id);

    if errors.is_empty() {
        Span::current().set_status(Status::Ok);
        ctx.storage().del(error_key).await?;
        return Ok(());
    }

    Span::current().set_status(Status::error("error"));

    let mut err = errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    if errors.iter().any(|e| e.internal()) {
        err.push_str(" (msg-id: ");
        err.push_str(&ctx.message().id);
        err.push(')');
    }
    ctx.storage().set(error_key, &err).await?;

    Ok(())
}

type PreparedCommands = Vec<Vec<(CommandContext, CommandFuture)>>;

pub async fn prepare_commands(
    ctx: &EvalContext,
    command_msg: CommandMessage,
) -> Result<PreparedCommands, Vec<CommandError>> {
    let mut errors = vec![];
    let mut groups = vec![];

    for (group_idx, group) in command_msg.parallel.into_iter().enumerate() {
        let mut prepared = vec![];
        for (cmd_idx, cmd_expr) in group.into_iter().enumerate() {
            match prepare_command(ctx, cmd_expr, group_idx, cmd_idx).await {
                Ok(p) => prepared.push(p),
                Err(e) => errors.push(e),
            }
        }
        groups.push(prepared);
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(groups)
}

async fn find_command(
    ctx: &EvalContext,
    name: &str,
    expr: &mut CommandExpr,
) -> Option<&'static CommandRegistration> {
    tracing::trace!(name, "looking up command");

    if let Some(reg) = super::find(name) {
        return Some(reg);
    }

    let mut p = ctx.storage().create_pipeline();
    p.hexists(format!("macros:{}", ctx.macro_ctx.owner), name)
        .queue();
    p.hexists("macros:global", name).queue();

    match p.execute().await {
        Ok((personal, global)) if personal || global => {
            // empty string for current username, to allow macro args to immediately follow
            expr.args.push_front(String::new());
            expr.args.push_front(name.to_owned());
            Some(*super::MACRO)
        }
        _ => None,
    }
}

const STACK_LIMIT: u32 = 3;

async fn prepare_command(
    ctx: &EvalContext,
    mut cmd_expr: CommandExpr,
    group_idx: usize,
    cmd_idx: usize,
) -> Result<(CommandContext, CommandFuture), CommandError> {
    let token = CommandToken {
        name: cmd_expr.name.to_ascii_lowercase().into(),
        tpe: cmd_expr.tpe,
        group: group_idx,
        idx: cmd_idx,
    };

    let found = match find_command(ctx, &token.name, &mut cmd_expr).await {
        Some(r) => Some(r),
        None => lazy_regex::regex_if!(r#"^(?<name>.*?)(?<n>\d+s?)$"#, &*token.name, {
            cmd_expr.args.push_front(n.to_owned());
            find_command(ctx, name, &mut cmd_expr).await
        })
        .flatten(),
    };
    let Some(registration) = found else {
        return Err(CommandError::UnknownCommand(token));
    };

    let desc = CommandDescriptor {
        registration,
        token,
    };
    if ctx.macro_depth > STACK_LIMIT {
        return Err(CommandError::RecursionLimit(desc));
    }
    if registration.permission > ctx.message().sender.level {
        return Err(CommandError::PermissionError(desc));
    }

    let cmd_ctx = CommandContext::new(ctx.clone(), desc.clone());

    // expand args
    let macro_args = &cmd_ctx.macro_ctx.args;
    for arg in &mut cmd_expr.args {
        *arg = regex_replace_all!(r#"(%?)%(\d+)"#, arg, |_, p: &str, num: &str| {
            if p.is_empty() {
                if let Some(arg) = num
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n != 0)
                    .and_then(|n| macro_args.get(n - 1))
                {
                    arg.clone()
                } else {
                    format!("%{num}")
                }
            } else {
                format!("%{num}")
            }
        })
        .into_owned();
    }

    match (registration.handler)(cmd_ctx.clone(), Args::new(cmd_expr.args)) {
        Ok(fut) => Ok((cmd_ctx, fut)),
        Err(error) => Err(CommandError::BadArgs(desc, error)),
    }
}

pub async fn eval(ctx: &EvalContext, command_msg: CommandMessage) -> Vec<CommandError> {
    let commands = match prepare_commands(ctx, command_msg).await {
        Ok(prepared) => prepared,
        Err(errors) => return errors,
    };

    tracing::trace!("running commands");

    let mut parallel = JoinSet::new();
    for group in commands {
        parallel.spawn(run_command_sequence(group).in_current_span());
    }

    let mut errors = Vec::new();
    while let Some(r) = parallel.join_next().await {
        match r {
            Ok(es) => errors.extend(es),
            Err(e) => errors.push(CommandError::SequencePanic(e.into_panic())),
        }
    }
    errors
}

async fn run_command_sequence(sequence: Vec<(CommandContext, CommandFuture)>) -> Vec<CommandError> {
    let mut result = Vec::new();
    for (ctx, fut) in sequence {
        // spawn a task for each command to catch panics
        let cmd = &ctx.command.token;
        let cmd_span = debug_span!("command", %cmd.name, ?cmd.tpe, cmd.group, cmd.idx);
        let cmd_span_inner = cmd_span.clone();
        let cmd = ctx.command.clone();
        let cmd_inner = cmd.clone();
        let handle = tokio::spawn(
            async move {
                match run_command(ctx, fut).await {
                    Ok(()) => {
                        cmd_span_inner.set_status(Status::Ok);
                        Ok(())
                    }
                    Err(error) => match error.downcast::<CommandFailure>() {
                        Ok(CommandFailure(failure)) => {
                            tracing::debug!(failure);
                            cmd_span_inner.set_status(Status::error("failure"));
                            Err(CommandError::Failure(cmd_inner, failure))
                        }
                        Err(error) => match error.downcast::<CommandInterrupt>() {
                            Ok(_) => {
                                tracing::debug!("interrupted");
                                Err(CommandError::Interrupt)
                            }
                            Err(error) => {
                                tracing::error!(?error);
                                cmd_span_inner.set_status(Status::error("error"));
                                Err(CommandError::Internal(cmd_inner, error))
                            }
                        },
                    },
                }
            }
            .instrument(cmd_span.clone().or_current()),
        );
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let interrupt = matches!(e, CommandError::Interrupt);
                result.push(e);
                if interrupt {
                    break;
                }
            }
            Err(e) => {
                cmd_span.set_status(Status::error("panic"));
                let panic = e.into_panic();
                cmd_span.in_scope(|| tracing::error!(panic = panic_string(&panic), "task panic"));
                result.push(CommandError::Panic(cmd, panic));
            }
        }
    }
    result
}

fn panic_string(payload: &Box<dyn Any + Send>) -> Option<&str> {
    payload.downcast_ref::<String>()?;
    payload.downcast_ref::<&'static str>()?;
    None
}

#[macro_export]
macro_rules! fail {
    ($($args:tt)*) => {
        ::anyhow::bail!($crate::commands::runner::CommandFailure::new(format!($($args)*)))
    };
}

async fn run_command(ctx: CommandContext, fut: CommandFuture) -> Result<()> {
    let r = ctx.command.registration;
    if let Some(global_gate) = &r.global_gate {
        if !ctx.gate(r.name, *global_gate).await? {
            fail!(
                "global timeout {}",
                humantime::format_duration(*global_gate)
            );
        }
    }
    if let Some(sender_gate) = &r.sender_gate {
        if !ctx.sender_gate(r.name, *sender_gate).await? {
            fail!(
                "sender timeout {}",
                humantime::format_duration(*sender_gate)
            );
        }
    }

    let status = html! {
        span style="color: #E38AF0" { (ctx.message().sender.name) } ": " (ctx.command) " " (ctx.nesting_str())
    };
    let _guard = if r.no_wall {
        None
    } else {
        Some(ctx.status_wall().push(status).await)
    };

    tracing::trace!("running command");
    fut.await
}

#[derive(Debug, Error)]
#[error("{0}")]
pub struct CommandFailure(String);

#[derive(Debug, Error)]
#[error("interrupt")]
pub struct CommandInterrupt;

impl CommandFailure {
    pub fn new(message: String) -> Self {
        Self(message)
    }
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("{0:#}: command does not exist")]
    UnknownCommand(CommandToken),
    #[error("{0:#}: no permission")]
    PermissionError(CommandDescriptor),
    #[error("{0:#}: {1}")]
    BadArgs(CommandDescriptor, ExtractorError),

    #[error("{0:#}: recursion limit")]
    RecursionLimit(CommandDescriptor),
    #[error("{0:#}: {1}")]
    Failure(CommandDescriptor, String),
    #[error("interrupted")]
    Interrupt,

    #[error("{0:#}: internal error")]
    Internal(CommandDescriptor, anyhow::Error),
    #[error("{0:#}: internal error")]
    Panic(CommandDescriptor, Box<dyn Any + Send + 'static>),
    #[error("internal error")] // should never happen 🤷
    SequencePanic(Box<dyn Any + Send + 'static>),
}

impl CommandError {
    pub fn internal(&self) -> bool {
        matches!(
            self,
            Self::Internal(_, _) | Self::Panic(_, _) | Self::SequencePanic(_)
        )
    }
}
