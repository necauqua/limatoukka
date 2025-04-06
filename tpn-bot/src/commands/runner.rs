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
    parsing::CommandMessage,
};

pub async fn receive_message(ctx: AppContext, message: Message) -> Result<()> {
    let s = &message.sender;

    // if !ctx
    //     .gate(&format!("message:{}", s.id), Duration::from_millis(500))
    //     .await?
    // {
    //     return Ok(());
    // }

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

    let ctx = MessageContext::new(ctx.clone(), Arc::new(message));
    let errors = match prepare_commands(&ctx) {
        Ok(prepared) => run_commands(&ctx, prepared).await,
        Err(errors) => errors,
    };

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

fn prepare_commands(context: &MessageContext) -> Result<PreparedCommands, Vec<CommandError>> {
    let command_msg = CommandMessage::parse(&context.message.text);

    let mut errors = vec![];
    let mut groups = vec![];

    for (group_idx, group) in command_msg.parallel.into_iter().enumerate() {
        let mut prepared = vec![];
        for (cmd_idx, cmd_expr) in group.into_iter().enumerate() {
            let token = CommandToken {
                name: cmd_expr.name.to_ascii_lowercase().into(),
                tpe: cmd_expr.tpe,
                group: group_idx,
                idx: cmd_idx,
            };
            let Some(reg) = super::find(&token.name) else {
                errors.push(CommandError::UnknownCommand(token));
                continue;
            };
            let desc = CommandDescriptor {
                registration: reg,
                token,
            };
            if reg.permission > context.message.sender.level {
                errors.push(CommandError::PermissionError(desc));
                continue;
            }

            let cmd_ctx = CommandContext::new(context.clone(), desc.clone());

            match (reg.handler)(cmd_ctx, Args::new(cmd_expr.args, cmd_expr.rest)) {
                Ok(fut) => prepared.push((desc, fut)),
                Err(error) => {
                    let err = CommandError::BadArgs(desc, error);
                    errors.push(err);
                }
            }
        }
        groups.push(prepared);
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(groups)
}

async fn run_commands(ctx: &MessageContext, commands: PreparedCommands) -> Vec<CommandError> {
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
                        Err(error) => {
                            tracing::error!(?error);
                            cmd_span_inner.set_status(Status::error("error"));
                            Err(CommandError::Internal(cmd_inner, error))
                        }
                    },
                }
            }
            .instrument(cmd_span.clone().or_current()),
        );
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                result.push(e);
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

async fn run_command(
    ctx: MessageContext,
    cmd: &CommandDescriptor,
    fut: CommandFuture,
) -> Result<()> {
    if let Some(global_gate) = &cmd.registration.global_gate {
        if !ctx.gate(&cmd.registration.name, *global_gate).await? {
            return Ok(());
        }
    }
    if let Some(sender_gate) = &cmd.registration.sender_gate {
        if !ctx
            .sender_gate(&cmd.registration.name, *sender_gate)
            .await?
        {
            return Ok(());
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

impl CommandFailure {
    pub fn new(message: String) -> Self {
        Self(message)
    }
}

#[macro_export]
macro_rules! fail {
    ($($args:tt)*) => {
        ::anyhow::bail!($crate::commands::runner::CommandFailure::new(format!($($args)*)))
    };
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("{0:#}: command does not exist")]
    UnknownCommand(CommandToken),
    #[error("{0:#}: no permission")]
    PermissionError(CommandDescriptor),
    #[error("{0:#}: {1}")]
    BadArgs(CommandDescriptor, ExtractorError),
    #[error("{0:#}: {1}")]
    Failure(CommandDescriptor, String),
    #[error("{0:#}: internal error")]
    Internal(CommandDescriptor, anyhow::Error),
    #[error("{0:#}: internal error")]
    Panic(CommandDescriptor, Box<dyn Any + Send + 'static>),
    #[error("internal error")] // should never happen 🤷
    SequencePanic(Box<dyn Any + Send + 'static>),
}

impl CommandError {
    pub fn internal(&self) -> bool {
        match self {
            Self::UnknownCommand(_)
            | Self::PermissionError(_)
            | Self::BadArgs(_, _)
            | Self::Failure(_, _) => false,
            Self::Internal(_, _) | Self::Panic(_, _) | Self::SequencePanic(_) => true,
        }
    }
}
