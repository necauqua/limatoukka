use std::{
    any::Any,
    collections::HashMap,
    fmt::{self, Display},
    sync::Arc,
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
    services::{
        charges::{Charges, ChargesService},
        gates::GateService,
        messaging::{Message, PermissionLevel},
        status_wall::StatusService,
    },
};

use super::{
    CommandContext, CommandFuture, NativeCommand,
    args::{Args, ExtractorError},
};

#[macro_export]
macro_rules! fail {
    ($($args:tt)*) => {
        return Err($crate::commands::runner::CommandError::PreconditionFail(format!($($args)*)))
    };
}

struct Inner {
    commands: HashMap<String, Arc<NativeCommand>>,
}

#[derive(Clone)]
pub struct Runner {
    inner: Arc<Inner>,
}

impl Runner {
    pub fn new(commands: HashMap<String, Arc<NativeCommand>>) -> Self {
        Self {
            inner: Arc::new(Inner { commands }),
        }
    }
}

impl Runner {
    pub async fn process_message(&self, ctx: AppContext, message: Message) -> Result<()> {
        let s = &message.sender;

        if ctx.bot_id() == Some(&s.id) {
            return Ok(());
        }

        // tidolar hehe
        if s.id == "506202997" && ctx.gate("tidolar-plink", Duration::from_secs(600)).await? {
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
        }
        tracing::debug!("processing message: {}", message.text);

        let error_key = format!("last-error:{}", message.sender.id);
        let eval_ctx =
            EvalContext::new(MessageContext::new(ctx.clone(), self.clone(), message)).await?;

        match self.eval(eval_ctx, stmt).await {
            Ok(_) | Err(EvalError::Interrupt) => {
                Span::current().set_status(Status::Ok);
                ctx.storage().del(error_key).await?;
            }
            Err(EvalError::RecursionLimit) => unreachable!(),
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

    const STACK_LIMIT: u32 = 3;

    pub async fn eval(&self, ctx: EvalContext, stmt: Statement) -> Result<(), EvalError> {
        if ctx.macro_depth > Self::STACK_LIMIT {
            return Err(EvalError::RecursionLimit);
        }

        let mut errors = vec![];
        let mut groups = vec![];

        for (seq, group) in stmt.parallel.iter().enumerate() {
            let mut prepared = vec![];
            for (idx, cmd_expr) in group.iter().enumerate() {
                match self
                    .prepare_command(&ctx, cmd_expr.clone(), Location::new(seq, idx))
                    .await
                {
                    Ok(p) => prepared.push(p),
                    Err(e) => errors.push(e),
                }
            }
            groups.push(prepared);
        }

        if !errors.is_empty() {
            tracing::trace!("prepare fail: {errors:?}");
            return Err(EvalError::CommandErrors(errors));
        }

        let mut parallel = JoinSet::new();
        for group in groups {
            parallel.spawn(Self::run_command_sequence(group).in_current_span());
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

    async fn prepare_command(
        &self,
        ctx: &EvalContext,
        mut command: Command,
        pos: Location,
    ) -> Result<(CommandContext, CommandFuture), ContextualCommandError> {
        let mut token = command.token.clone();
        token.name.make_ascii_lowercase();

        let found = match self.lookup(ctx, &token.name, &mut command).await {
            Some(r) => Some(r),
            None => match token.split_inline_number() {
                Some((name, n)) => {
                    command.params.push_front(Param::simple(n.to_owned()));
                    self.lookup(ctx, name, &mut command).await
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

        let cmd_ctx = CommandContext::new(ctx.clone(), meta.clone(), token, pos.clone());

        let fut = (meta.action)(cmd_ctx.clone(), Args::new(command.params));
        Ok((cmd_ctx, fut))
    }

    pub fn get_command_meta(&self, name: &str) -> Option<&NativeCommand> {
        self.inner.commands.get(name).map(|meta| meta.as_ref())
    }

    async fn lookup(
        &self,
        ctx: &EvalContext,
        name: &str,
        expr: &mut Command,
    ) -> Option<Arc<NativeCommand>> {
        if let Some(reg) = self.inner.commands.get(name).cloned() {
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

        if let Ok((personal, global)) = res
            && (personal || global)
            && let Some(meta) = self.inner.commands.get("macro").cloned()
        {
            // empty string for current username, to allow macro params to immediately follow
            expr.params.push_front(Param::default());
            expr.params.push_front(Param::simple(name.to_owned()));
            return Some(meta);
        }
        None
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
                    let error = match Self::run_command(ctx_inner, fut).await {
                        Ok(()) => {
                            cmd_span_inner.set_status(Status::Ok);
                            return Ok(());
                        }
                        Err(e) => e,
                    };
                    if matches!(error, CommandError::Interrupt) {
                        cmd_span_inner.set_status(Status::Ok);
                        tracing::debug!("interrupted");
                    } else if error.is_internal() {
                        cmd_span_inner.set_status(Status::error("error"));
                        tracing::error!(?error);
                    } else {
                        cmd_span_inner.set_status(Status::error("failure"));
                        tracing::debug!(?error, "command failure");
                    }
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

    async fn run_command(ctx: CommandContext, fut: CommandFuture) -> Result<(), CommandError> {
        let gate_service = ctx.service::<dyn GateService>();
        let charges_service = ctx.service::<dyn ChargesService>();

        let level = ctx.message().sender.level;
        let exempt = ctx.in_global_macro && ctx.meta.is(CommandTag::GlobalMacroExempt);

        if !exempt && level < ctx.meta.permission {
            return Err(CommandError::Permission);
        }

        if level != PermissionLevel::Caster {
            if let Some(global_gate) = &ctx.meta.global_gate
                && !gate_service
                    .gate("global", ctx.meta.name, *global_gate)
                    .await?
            {
                return Err(CommandError::GlobalTimeout(*global_gate));
            }
            if let Some(sender_gate) = &ctx.meta.sender_gate
                && !gate_service
                    .gate(ctx.sender(), ctx.meta.name, *sender_gate)
                    .await?
            {
                return Err(CommandError::SenderTimeout(*sender_gate));
            }
        }

        if !exempt
            && level < ctx.meta.free_for
            && let Some(cost) = ctx.meta.cost
        {
            let cost = cost.into();
            if !charges_service.consume(ctx.sender(), cost).await? {
                return Err(CommandError::NotEnoughCharges { cost });
            }
        }

        let _guard = if !ctx.meta.is(CommandTag::NoWall) {
            let wall = ctx.service::<dyn StatusService>();
            let guard = wall.push(html! {
                span style="color: #E38AF0" { (ctx.message().sender.name) } ": " (ctx.token) " " (ctx.nesting_str())
            }).await;
            Some(guard)
        } else {
            None
        };

        tracing::trace!("running: {}", ctx.token);

        let res = fut.await;

        if level != PermissionLevel::Caster && res.is_err() {
            if ctx.meta.global_gate.is_some() {
                gate_service.ungate("global", ctx.meta.name).await?
            }
            if ctx.meta.sender_gate.is_some() {
                gate_service.ungate(ctx.sender(), ctx.meta.name).await?
            }
        }

        res
    }
}

fn panic_string(payload: &Box<dyn Any + Send>) -> &str {
    payload
        .downcast_ref::<String>()
        .map(|s| &**s)
        .or_else(|| payload.downcast_ref::<&'static str>().copied())
        .unwrap_or("<no panic message>")
}

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("interrupted")]
    Interrupt,
    #[error("recursion limit")]
    RecursionLimit,
    #[error("{{ {} }}", .0.iter().map(|e| e.to_string()).collect::<Vec<_>>().join(", "))]
    CommandErrors(Vec<ContextualCommandError>),
}

impl From<EvalError> for CommandError {
    fn from(e: EvalError) -> Self {
        match e {
            EvalError::Interrupt => CommandError::Interrupt,
            EvalError::RecursionLimit => CommandError::RecursionLimit,
            EvalError::CommandErrors(_) => {
                CommandError::PreconditionFail(format!("eval errors: {e}"))
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("command does not exist")]
    NotFound,
    #[error("bad arguments: {0}")]
    BadArgs(#[from] ExtractorError),
    #[error("no permission")]
    Permission,
    #[error("global timeout {}", humantime::format_duration(*.0))]
    GlobalTimeout(Duration),
    #[error("sender timeout {}", humantime::format_duration(*.0))]
    SenderTimeout(Duration),
    #[error("poor (command costs {cost})")]
    NotEnoughCharges { cost: Charges },
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
