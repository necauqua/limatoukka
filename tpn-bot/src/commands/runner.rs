use std::{any::Any, sync::Arc, time::Duration};

use anyhow::Result;
use maud::html;
use opentelemetry::trace::Status;
use rustis::commands::StringCommands;
use thiserror::Error;
use tokio::task::JoinSet;
use tracing::{Instrument, Span, debug_span, field::Empty};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::services::messaging::Message;

use super::{
    CommandContext, CommandFuture,
    args::{Args, ExtractorError},
    context::{AppContext, CommandDescriptor, MessageContext},
    parsing::CommandMessage,
};

#[derive(Debug, Error)]
pub enum PreconditionFail {
    #[error("command {0}{1:?} does not exist")]
    UnknownCommand(String, (usize, usize)),
    #[error("command {0}{1:?}: {2}")]
    BadArgs(String, (usize, usize), ExtractorError),
}

#[tracing::instrument(
    name = "message",
    skip(app_ctx, msg),
    fields(
        msg.id = msg.id,
        msg.sender = msg.sender.login,
        msg.sender.id = msg.sender.id,
        msg.text = msg.text,
        run.id = Empty,
    )
)]
pub async fn receive_message(app_ctx: AppContext, msg: Message) {
    // app_ctx.noita.get_seed()

    match app_ctx
        .gate(
            &format!("message:{}", msg.sender.id),
            Duration::from_millis(500),
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => {
            tracing::error!(?e, "gate error");
            return;
        }
    };

    let error_key = format!("last-error:{}", msg.sender.id);
    let msg_id = msg.id.clone();

    tracing::trace!("processing message");

    let ctx = MessageContext::new(app_ctx.clone(), Arc::new(msg));

    let (last_error, status) = match prepare_commands(&ctx) {
        Err(errors) => {
            tracing::debug!(?errors, "precondition fail");
            let msg = errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            (Some(msg), Status::Ok)
        }
        Ok(prepared) => match run_commands(&ctx, prepared).await {
            Ok(()) => (None, Status::Ok),
            Err(()) => (
                Some(format!("internal error, msg id: {msg_id}")),
                Status::error("command error"),
            ),
        },
    };

    Span::current().set_status(status);

    if let Some(last_error) = last_error {
        if let Err(error) = app_ctx.storage.set(error_key, &last_error).await {
            tracing::error!(?error, "failed to set last error");
        }
    };
}

type PreparedCommands = Vec<Vec<(CommandDescriptor, CommandFuture)>>;

fn prepare_commands(context: &MessageContext) -> Result<PreparedCommands, Vec<PreconditionFail>> {
    let command_msg = CommandMessage::parse(&context.message.text);

    let mut errors = vec![];
    let mut groups = vec![];

    for (group_idx, group) in command_msg.parallel.into_iter().enumerate() {
        let mut prepared = vec![];
        for (cmd_idx, cmd) in group.into_iter().enumerate() {
            let Some(handler) = super::find_command(&cmd.name) else {
                errors.push(PreconditionFail::UnknownCommand(
                    cmd.name,
                    (group_idx, cmd_idx),
                ));
                continue;
            };

            let cmd_desc = CommandDescriptor {
                name: cmd.name.clone(),
                tpe: cmd.tpe,
                group: group_idx,
                idx: cmd_idx,
            };
            let cmd_ctx = CommandContext::new(context.clone(), cmd_desc.clone());

            match handler(cmd_ctx, Args::new(cmd.args.into(), cmd.rest)) {
                Ok(fut) => prepared.push((cmd_desc, fut)),
                Err(error) => {
                    let err = PreconditionFail::BadArgs(cmd.name, (group_idx, cmd_idx), error);
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

async fn run_commands(ctx: &MessageContext, commands: PreparedCommands) -> Result<(), ()> {
    if commands.is_empty() {
        return Ok(());
    }

    tracing::trace!("running commands");

    let mut parallel = JoinSet::new();
    for group in commands {
        parallel.spawn(run_command_sequence(ctx.clone(), group).in_current_span());
    }

    let mut result = Ok(());
    while let Some(r) = parallel.join_next().await {
        match r {
            Ok(_) => {}
            Err(_) => {
                result = Err(());
            }
        }
    }

    result
}

async fn run_command_sequence(
    ctx: MessageContext,
    sequence: Vec<(CommandDescriptor, CommandFuture)>,
) -> Result<(), ()> {
    let mut result = Ok(());
    for (cmd, fut) in sequence {
        // spawn a task for each command to catch panics
        let cmd_span = debug_span!("command", cmd.name, ?cmd.tpe, cmd.group, cmd.idx);
        let cmd_span_inner = cmd_span.clone();
        let ctx = ctx.clone();
        let handle = tokio::spawn(
            async move {
                match run_command(ctx, cmd, fut).await {
                    Ok(_) => {
                        cmd_span_inner.set_status(Status::Ok);
                        Ok(())
                    }
                    Err(error) => {
                        tracing::error!(?error);
                        cmd_span_inner.set_status(Status::error("error"));
                        Err(())
                    }
                }
            }
            .instrument(cmd_span.clone().or_current()),
        );
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(())) => {
                result = Err(());
            }
            Err(panic) => {
                cmd_span.set_status(Status::error("panic"));
                cmd_span.in_scope(|| {
                    tracing::error!(panic = panic_string(&panic.into_panic()), "task panic")
                });
                result = Err(());
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
    desc: CommandDescriptor,
    fut: CommandFuture,
) -> Result<()> {
    let status = html! {
        span style="color: rebeccapurple" { (ctx.message.sender.name)": "(desc) }
    };
    let _guard = ctx.status_wall.push_scoped(status.0).await;
    tracing::trace!("running command");
    fut.await
}
