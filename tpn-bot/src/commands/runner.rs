use std::{any::Any, sync::Arc};

use anyhow::Result;
use maud::html;
use opentelemetry::trace::Status;
use rustis::commands::{GenericCommands, StringCommands};
use thiserror::Error;
use tokio::task::JoinSet;
use tracing::{Instrument, Span, debug_span};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::services::messaging::{Message, PermissionLevel};

use super::{
    CommandContext, CommandFuture,
    args::{Args, ExtractorError},
    context::{AppContext, CommandDescriptor, CommandToken, MessageContext},
    parsing::{CommandExpr, CommandMessage},
};

pub async fn receive_message(ctx: AppContext, message: Message) -> Result<()> {
    let s = &message.sender;

    if ctx.config.bot.as_ref().is_some_and(|b| s.login == b.login) {
        return Ok(());
    }

    let stop_count = ctx
        .storage
        .exists(["full-stop", &format!("kick:begone:{}", s.id)])
        .await?;
    if stop_count != 0 && message.sender.level < PermissionLevel::Moderator {
        return Ok(());
    }

    // ughh, just keep a login -> id mapping to avoid having to hook up twitch
    // api just to make votekick work with usernames *and* prevent them from
    // changing the username to avoid the shadow realm once
    //
    // well, as a bonus this allows us to check if user being voteckicked ever
    // typed in chat
    ctx.storage
        .set(format!("twitch-users:{}", s.login), &s.id)
        .await?;

    tracing::debug!("processing message");

    let command_msg = CommandMessage::parse(&message.text);
    let ctx = MessageContext::new(ctx.clone(), Arc::new(message));

    let errors = eval(&ctx, command_msg, 0).await;

    let error_key = format!("last-error:{}", ctx.message.sender.id);

    if errors.is_empty() {
        Span::current().set_status(Status::Ok);
        ctx.storage.del(error_key).await?;
        return Ok(());
    }

    Span::current().set_status(Status::error("error"));

    let mut err = errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    if errors.iter().any(|e| e.internal()) {
        err.push_str("\n(msg-id: ");
        err.push_str(&ctx.message.id);
        err.push(')');
    }
    ctx.storage.set(error_key, &err).await?;

    Ok(())
}

type PreparedCommands = Vec<Vec<(CommandDescriptor, CommandFuture)>>;

async fn prepare_commands(
    context: &MessageContext,
    command_msg: CommandMessage,
    depth: u32,
) -> Result<PreparedCommands, Vec<CommandError>> {
    let mut errors = vec![];
    let mut groups = vec![];

    for (group_idx, group) in command_msg.parallel.into_iter().enumerate() {
        let mut prepared = vec![];
        for (cmd_idx, cmd_expr) in group.into_iter().enumerate() {
            match prepare_command(context, cmd_expr, group_idx, cmd_idx, depth).await {
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

const RECURSION_LIMIT: u32 = 3;

async fn prepare_command(
    ctx: &MessageContext,
    cmd_expr: CommandExpr,
    group_idx: usize,
    cmd_idx: usize,
    depth: u32,
) -> Result<(CommandDescriptor, CommandFuture), CommandError> {
    let token = CommandToken {
        name: cmd_expr.name.to_ascii_lowercase().into(),
        tpe: cmd_expr.tpe,
        group: group_idx,
        idx: cmd_idx,
    };
    let registration = match super::find(&token.name) {
        Some(r) => r,
        None => {
            // let mut p = ctx.storage.create_pipeline();
            // p.hexists(format!("macros:{}", ctx.message.sender.id), &*token.name)
            //     .queue();
            // p.hexists("macros:global", &*token.name).queue();

            // if let Ok(r) = p.execute().await {
            //     let (personal, global): (bool, bool) = r;
            //     if personal || global {
            //         *super::MACRO
            //     }
            // }
            return Err(CommandError::UnknownCommand(token));
        }
    };
    let desc = CommandDescriptor {
        registration,
        token,
    };
    if depth > RECURSION_LIMIT {
        return Err(CommandError::RecursionLimit(desc));
    }
    if registration.permission > ctx.message.sender.level {
        return Err(CommandError::PermissionError(desc));
    }

    let cmd_ctx = CommandContext::new(ctx.clone(), desc.clone(), depth);

    match (registration.handler)(cmd_ctx, Args::new(cmd_expr.args, cmd_expr.rest)) {
        Ok(fut) => Ok((desc, fut)),
        Err(error) => Err(CommandError::BadArgs(desc, error)),
    }
}

pub async fn eval(
    ctx: &MessageContext,
    command_msg: CommandMessage,
    depth: u32,
) -> Vec<CommandError> {
    let commands = match prepare_commands(ctx, command_msg, depth).await {
        Ok(prepared) => prepared,
        Err(errors) => return errors,
    };

    if commands.is_empty() {
        return Vec::new();
    }

    tracing::trace!("running commands");

    let mut parallel = JoinSet::new();
    for group in commands {
        parallel.spawn(run_command_sequence(ctx.clone(), group).in_current_span());
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

async fn run_command_sequence(
    ctx: MessageContext,
    sequence: Vec<(CommandDescriptor, CommandFuture)>,
) -> Vec<CommandError> {
    let mut result = Vec::new();
    for (cmd, fut) in sequence {
        // spawn a task for each command to catch panics
        let tok = &cmd.token;
        let cmd_span = debug_span!("command", %tok.name, ?tok.tpe, tok.group, tok.idx);
        let cmd_span_inner = cmd_span.clone();
        let ctx = ctx.clone();
        let cmd_inner = cmd.clone();
        let handle = tokio::spawn(
            async move {
                match run_command(ctx, &cmd_inner, fut).await {
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

async fn run_command(
    ctx: MessageContext,
    cmd: &CommandDescriptor,
    fut: CommandFuture,
) -> Result<()> {
    if let Some(global_gate) = &cmd.registration.global_gate {
        if !ctx.gate(cmd.registration.name, *global_gate).await? {
            fail!("global timeout {global_gate:?}");
        }
    }
    if let Some(sender_gate) = &cmd.registration.sender_gate {
        if !ctx.sender_gate(cmd.registration.name, *sender_gate).await? {
            fail!("sender timeout {sender_gate:?}");
        }
    }

    let _guard = if cmd.registration.long {
        let status = html! {
            span style="color: rebeccapurple" { (ctx.message.sender.name) } ": "(cmd)
        };
        Some(ctx.status_wall.push(status.0).await)
    } else {
        None
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
