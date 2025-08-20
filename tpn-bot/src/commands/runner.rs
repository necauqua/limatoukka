use std::{
    any::Any,
    fmt::{self, Display},
    time::Duration,
};

use anyhow::Result;
use humantime_serde::re::humantime;
use maud::html;
use neca_cmd::{Command, Statement, Token, param::Param};
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
    commands::CommandTag,
    context::{app::AppContext, eval::EvalContext, msg::MessageContext},
    services::messaging::{Message, PermissionLevel},
};

use super::{
    CommandContext, CommandFuture, CommandMetadata,
    args::{Args, ExtractorError},
};

pub async fn receive_message(ctx: AppContext, message: Message) -> Result<()> {
    let s = &message.sender;

    if s.id == ctx.twitch().bot_id() {
        return Ok(());
    }

    // tidolar hehe
    if s.id == "506202997" && ctx.gate("tidolar-plink", Duration::from_secs(120)).await? {
        ctx.send("plink".into()).await?;
    }

    if s.level < PermissionLevel::Moderator {
        let stop_count = ctx
            .storage()
            .exists(["flags:full-stop", &format!("kick:begone:{}", s.id)])
            .await?;
        if stop_count != 0 {
            return Ok(());
        }
    }

    let stmt = Statement::parse(&message.text);
    if stmt.is_noop() {
        tracing::trace!("no commands in message: {}", message.text);
        return Ok(());
    } else {
        tracing::debug!("processing message: {}", message.text);
    }

    let error_key = format!("last-error:{}", message.sender.id);
    let eval_ctx = EvalContext::new(MessageContext::new(ctx.clone(), message)).await?;

    match eval(eval_ctx, stmt).await {
        Ok(_) | Err(EvalError::Interrupt) => {
            Span::current().set_status(Status::Ok);
            ctx.storage().del(error_key).await?;
        }
        Err(EvalError::CommandErrors(errors)) => {
            Span::current().set_status(Status::error("error"));

            let mut err = errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            if errors.iter().any(|e| e.error.is_internal()) {
                err.push_str(" (msg-id: ");
                err.push_str(&error_key[11..]); // meh
                err.push(')');
            }
            ctx.storage().set(error_key, &err).await?;
        }
    }
    Ok(())
}

type PreparedCommands = Vec<Vec<(CommandContext, CommandFuture)>>;

pub async fn prepare_commands(
    ctx: &EvalContext,
    stmt: &Statement,
) -> Result<PreparedCommands, Vec<ContextualCommandError>> {
    let mut errors = vec![];
    let mut groups = vec![];

    for (seq, group) in stmt.parallel.iter().enumerate() {
        let mut prepared = vec![];
        for (idx, cmd_expr) in group.iter().enumerate() {
            match prepare_command(ctx, cmd_expr.clone(), Location::new(seq, idx)).await {
                Ok(p) => prepared.push(p),
                Err(e) => errors.push(e),
            }
        }
        groups.push(prepared);
    }

    if !errors.is_empty() {
        tracing::trace!("prepare fail: {errors:?}");
        return Err(errors);
    }

    Ok(groups)
}

async fn find_command(
    ctx: &EvalContext,
    name: &str,
    expr: &mut Command,
) -> Option<&'static CommandMetadata> {
    if let Some(reg) = super::find(name) {
        return Some(reg);
    }

    let res = {
        let storage = ctx.storage();
        let mut p = storage.create_pipeline();
        p.hexists(format!("macros:{}", ctx.shared.owner), name)
            .queue();
        p.hexists("macros:global", name).queue();
        p.execute().await
    };

    match res {
        Ok((personal, global)) if personal || global => {
            // empty string for current username, to allow macro params to immediately follow
            expr.params.push_front(Param::default());
            expr.params.push_front(Param::simple(name.to_owned()));
            Some(*super::MACRO)
        }
        _ => None,
    }
}

const STACK_LIMIT: u32 = 3;

async fn prepare_command(
    ctx: &EvalContext,
    mut command: Command,
    pos: Location,
) -> Result<(CommandContext, CommandFuture), ContextualCommandError> {
    let mut token = command.token.clone();
    token.name.make_ascii_lowercase();

    let found = match find_command(ctx, &token.name, &mut command).await {
        Some(r) => Some(r),
        None => match token.split_inline_number() {
            Some((name, n)) => {
                command.params.push_front(Param::simple(n.to_owned()));
                find_command(ctx, name, &mut command).await
            }
            None => None,
        },
    };
    let Some(meta) = found else {
        return Err(ContextualCommandError::new(
            CommandError::NotFound,
            command.token,
            pos,
        ));
    };
    // meh just hardcode this for now
    if meta.is(CommandTag::NoitaControl) || meta.is(CommandTag::OBSControl) {
        return Err(ContextualCommandError::new(
            CommandError::Disabled,
            command.token,
            pos,
        ));
    }

    if ctx.macro_depth > STACK_LIMIT {
        return Err(ContextualCommandError::new(
            CommandError::RecursionLimit,
            command.token,
            pos,
        ));
    }
    if meta.permission > ctx.message().sender.level {
        return Err(ContextualCommandError::new(
            CommandError::Permission,
            command.token,
            pos,
        ));
    }

    let cmd_ctx = CommandContext::new(ctx.clone(), meta, token, pos.clone());

    match (meta.handler)(cmd_ctx.clone(), Args::new(command.params)).await {
        Ok(fut) => Ok((cmd_ctx, fut)),
        Err(error) => Err(ContextualCommandError::new(
            CommandError::BadArgs(error),
            command.token,
            pos,
        )),
    }
}

fn print_inner_errors(errors: &[ContextualCommandError]) -> String {
    errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("interrupted")]
    Interrupt,
    #[error("{{ {} }}", print_inner_errors(.0))]
    CommandErrors(Vec<ContextualCommandError>),
}

impl From<EvalError> for CommandError {
    fn from(e: EvalError) -> Self {
        match e {
            EvalError::Interrupt => CommandError::Interrupt,
            EvalError::CommandErrors(_) => {
                CommandError::PreconditionFail(format!("eval errors: {e}"))
            }
        }
    }
}

pub async fn eval(ctx: EvalContext, command_msg: Statement) -> Result<(), EvalError> {
    let commands = match prepare_commands(&ctx, &command_msg).await {
        Ok(prepared) => prepared,
        Err(errors) => return Err(EvalError::CommandErrors(errors)),
    };

    let mut parallel = JoinSet::new();
    for group in commands {
        parallel.spawn(run_command_sequence(group).in_current_span());
    }

    // join_all panics if any task panics, which would only happen in run_command_sequence panics,
    // which it shouldnt (and if it does it's ok for us to panic entirely),
    // as it only dispatches more tasks for commands itself
    let errors = parallel
        .join_all()
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    if ctx.interrupted() {
        Err(EvalError::Interrupt)
    } else if errors.is_empty() {
        Ok(())
    } else {
        Err(EvalError::CommandErrors(errors))
    }
}

async fn run_command_sequence(
    sequence: Vec<(CommandContext, CommandFuture)>,
) -> Vec<ContextualCommandError> {
    let mut result = Vec::new();
    for (ctx, fut) in sequence {
        // spawn a task for each command to catch panics
        let cmd_span = debug_span!("command", cmd.name=%ctx.token.name, cmd.tpe=?ctx.token.symbol, ?ctx.pos, otel.name=format!("{}", ctx.token));
        let cmd_span_inner = cmd_span.clone();
        let ctx_inner = ctx.clone();
        let handle = tokio::spawn(
            async move {
                let error = match run_command(ctx_inner, fut).await {
                    Ok(()) => {
                        cmd_span_inner.set_status(Status::Ok);
                        return Ok(());
                    }
                    Err(e) => e,
                };
                match &error {
                    CommandError::Interrupt => {
                        cmd_span_inner.set_status(Status::Ok);
                        tracing::debug!("interrupted");
                    }
                    CommandError::PreconditionFail(failure) => {
                        cmd_span_inner.set_status(Status::error("failure"));
                        tracing::debug!(failure, "command failure: {failure}");
                    }
                    error => {
                        cmd_span_inner.set_status(Status::error("error"));
                        tracing::error!(?error);
                    }
                };
                Err(error)
            }
            .instrument(cmd_span.clone().or_current()),
        );
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let interrupt = matches!(e, CommandError::Interrupt);
                result.push(ContextualCommandError::new(e, ctx.token, ctx.pos.clone()));
                if interrupt {
                    break;
                }
            }
            Err(e) => {
                cmd_span.set_status(Status::error("panic"));
                let panic = e.into_panic();
                cmd_span.in_scope(|| tracing::error!("task panic: {}", panic_string(&panic)));
                result.push(ContextualCommandError::new(
                    CommandError::Panic(panic),
                    ctx.token,
                    ctx.pos.clone(),
                ));
            }
        }
    }
    result
}

fn panic_string(payload: &Box<dyn Any + Send>) -> &str {
    payload
        .downcast_ref::<String>()
        .map(|s| &**s)
        .or_else(|| payload.downcast_ref::<&'static str>().copied())
        .unwrap_or("<no panic message>")
}

#[macro_export]
macro_rules! fail {
    ($($args:tt)*) => {
        return Err($crate::commands::runner::CommandError::PreconditionFail(format!($($args)*)))
    };
}

async fn run_command(ctx: CommandContext, fut: CommandFuture) -> Result<(), CommandError> {
    let r = ctx.meta;
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
        span style="color: #E38AF0" { (ctx.message().sender.name) } ": " (ctx.token) " " (ctx.nesting_str())
    };
    let _guard = if r.is(CommandTag::NoWall) {
        None
    } else {
        Some(ctx.status_wall().push(status).await)
    };

    tracing::trace!("running: {}", ctx.token);
    fut.await
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("command does not exist")]
    NotFound,
    #[error("command is disabled")]
    Disabled,
    #[error("no permission")]
    Permission,
    #[error("bad arguments: {0}")]
    BadArgs(#[from] ExtractorError),
    #[error("recursion limit")]
    RecursionLimit,
    #[error("cancelled")]
    Interrupt,
    /// A custom non-unexpected error returned by the command implementation
    #[error("{0}")]
    PreconditionFail(String),
    #[error("internal")]
    Internal(#[from] anyhow::Error),
    #[error("internal")]
    Panic(Box<dyn Any + Send + 'static>),
}

// temporary until we finally abstract over storage..
impl From<rustis::Error> for CommandError {
    fn from(e: rustis::Error) -> Self {
        CommandError::Internal(anyhow::anyhow!(e))
    }
}

// convenience
impl From<serde_json::Error> for CommandError {
    fn from(e: serde_json::Error) -> Self {
        CommandError::Internal(anyhow::anyhow!(e))
    }
}

impl CommandError {
    pub fn is_internal(&self) -> bool {
        matches!(self, CommandError::Internal(_) | CommandError::Panic(_))
    }
}

#[derive(Debug, Clone)]
pub struct Location {
    pub seq: usize,
    pub idx: usize,
}

impl Location {
    pub fn new(seq: usize, idx: usize) -> Self {
        Self { seq, idx }
    }
}

impl Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.seq, self.idx)
    }
}

#[derive(Debug, Error)]
pub struct ContextualCommandError {
    pub error: CommandError,
    pub token: Token,
    pub pos: Location,
}

impl ContextualCommandError {
    pub fn new(error: CommandError, token: Token, pos: Location) -> Self {
        Self { error, token, pos }
    }
}

impl Display for ContextualCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({}): {}", self.token, self.pos, self.error)
    }
}
