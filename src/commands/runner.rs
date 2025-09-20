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
use thiserror::Error;
use tokio::task::JoinSet;
use tracing::{Instrument, debug_span};

use crate::{
    commands::CommandTag,
    context::{app::AppContext, eval::EvalContext, msg::MessageContext},
    services::{
        Injector,
        banishes::BanishService,
        charges::{Charges, ChargesServiceExt},
        gates::GateServiceExt,
        messaging::{Message, PermissionLevel},
        stats::StatsServiceExt,
        status_wall::StatusServiceExt,
        storage::StorageService,
        variables::{VarResolution, VarType, VariableStorage},
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

#[derive(Clone)]
pub struct Runner {
    commands: HashMap<String, Arc<NativeCommand>>,
    vars: Arc<dyn VariableStorage>,
    storage: Arc<dyn StorageService>,
    banishes: Arc<dyn BanishService>,
}

impl Runner {
    pub fn new(commands: HashMap<String, Arc<NativeCommand>>, injector: &Injector) -> Self {
        Self {
            commands,
            vars: injector.service(),
            storage: injector.service(),
            banishes: injector.service(),
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
            if self.storage.has("settings:stop").await? {
                return Ok(());
            }
            if self.banishes.status(&s.id).await?.is_banished() {
                return Ok(());
            }
        } else if self
            .storage
            .has(&format!("settings:turbo-banished:{}", s.login))
            .await?
        {
            return Ok(());
        }

        let stmt = Statement::parse(&message.text);
        if stmt.is_noop() {
            tracing::trace!("no commands in message: {}", message.text);
            return Ok(());
        }
        tracing::debug!("processing message: {}", message.text);

        let msg_id = message.id.clone();
        let error_key = format!("last-error:{}", message.sender.id);
        let eval_ctx =
            EvalContext::new(MessageContext::new(ctx.clone(), self.clone(), message)).await?;

        match self.eval(eval_ctx, stmt).await {
            Ok(_) | Err(EvalError::Interrupt) => {
                self.storage.del(&error_key).await?;
            }
            Err(EvalError::RecursionLimit) => unreachable!(),
            Err(EvalError::CommandErrors(errors)) => {
                let mut err = errors
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                if errors.iter().any(|e| e.error.is_internal()) {
                    err.push_str(" (msg-id: ");
                    err.push_str(&msg_id);
                    err.push(')');
                }
                self.storage.set(&error_key, &err).await?;
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

        for (seq, group) in stmt.parallel.into_iter().enumerate() {
            let mut prepared = vec![];
            for (idx, mut cmd_expr) in group.into_iter().enumerate() {
                let loc = Location::new(seq, idx);
                match self.prepare_command(&ctx, &mut cmd_expr, loc.clone()).await {
                    Ok(p) => prepared.push(p),
                    Err(error) => errors.push(ContextualCommandError {
                        error,
                        token: cmd_expr.token,
                        loc,
                    }),
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
        command: &mut Command,
        loc: Location,
    ) -> Result<(CommandContext, CommandFuture), CommandError> {
        let mut token = command.token.clone();
        token.name.make_ascii_lowercase();

        let owner = ctx.owner();

        let found = match self.lookup(&owner.id, &token.name, command).await? {
            Some(r) => Some(r),
            None => match token.split_inline_number() {
                Some((name, n)) => {
                    command.params.push_front(Param::simple(n.to_owned()));
                    self.lookup(&owner.id, name, command).await?
                }
                None => None,
            },
        };
        let Some(meta) = found else {
            return Err(CommandError::NotFound);
        };

        let cmd_ctx = CommandContext::new(ctx.clone(), meta.clone(), token, loc.clone());

        let fut = (meta.action)(
            cmd_ctx.clone(),
            Args::new(std::mem::take(&mut command.params)),
        );
        Ok((cmd_ctx, fut))
    }

    pub fn get_command(&self, name: &str) -> Option<Arc<NativeCommand>> {
        self.commands.get(name).cloned()
    }

    async fn lookup(
        &self,
        owner: &str,
        name: &str,
        expr: &mut Command,
    ) -> Result<Option<Arc<NativeCommand>>> {
        if let Some(meta) = self.get_command(name) {
            return Ok(Some(meta.clone()));
        }

        Ok(
            match self.vars.resolve(VarType::Macro, owner, name).await? {
                VarResolution::None => None,
                _ => {
                    if let Some(q) = self.get_command("macro") {
                        // empty string for current username, to allow macro params to immediately follow
                        expr.params.push_front(Param::default());
                        expr.params.push_front(Param::simple(name.to_owned()));
                        Some(q)
                    } else {
                        None
                    }
                }
            },
        )
    }

    async fn run_command_sequence(
        sequence: Vec<(CommandContext, CommandFuture)>,
    ) -> Vec<ContextualCommandError> {
        let mut result = Vec::new();
        for (ctx, fut) in sequence {
            // spawn a task for each command to catch panics
            let cmd_span = debug_span!("command", cmd.name=%ctx.token.name, cmd.tpe=?ctx.token.symbol, ?ctx.pos);
            let ctx_inner = ctx.clone();
            let handle = tokio::spawn(
                async move {
                    let error = match Self::run_command(ctx_inner, fut).await {
                        Ok(()) => {
                            return Ok(());
                        }
                        Err(e) => e,
                    };
                    if matches!(error, CommandError::Interrupt) {
                        tracing::debug!("interrupted");
                    } else if error.is_internal() {
                        tracing::error!(?error);
                    } else {
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
        let m = &ctx.meta;
        let level = ctx.message().sender.level;
        let exempt = ctx.in_global_macro && m.is(CommandTag::GlobalMacroExempt);

        if !exempt && level < m.permission {
            return Err(CommandError::Permission);
        }

        if level != PermissionLevel::Caster {
            ctx.gates()
                .command_gates(ctx.sender(), m.name, m.global_gate, m.sender_gate)
                .await?;
        }

        let mut refund = None;
        if !exempt
            && level < m.free_for
            && let Some(cost) = m.cost
        {
            let cost = cost.into();
            if !ctx.charges().consume(ctx.sender(), cost).await? {
                return Err(CommandError::NotEnoughCharges { cost });
            }
            refund = Some(cost);
        }

        let _guard = if !m.is(CommandTag::NoWall) {
            let wall = ctx.status();
            let guard = wall.push(html! {
                span style="color: #E38AF0" { (ctx.message().sender.name) } ": " (ctx.token) " " (ctx.nesting_str())
            }).await;
            Some(guard)
        } else {
            None
        };

        tracing::trace!("running: {}", ctx.token);
        let stats = ctx.stats();
        stats
            .record(ctx.sender(), &format!("command:{}", &ctx.token.name))
            .await?;
        stats.record(ctx.sender(), "commands").await?;
        stats
            .record(ctx.sender(), &format!("symbol:{:?}", ctx.token.symbol))
            .await?;

        let res = fut.await;

        // ungate and refund on errors
        if res
            .as_ref()
            .is_err_and(|e| !matches!(e, CommandError::Interrupt))
        {
            if level != PermissionLevel::Caster {
                ctx.gates()
                    .command_ungate(ctx.sender(), m.name, m.global_gate, m.sender_gate)
                    .await?;
            }
            if let Some(cost) = refund {
                ctx.charges().add(ctx.sender(), cost).await?;
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
    pub loc: Location,
}

impl ContextualCommandError {
    pub fn new(error: CommandError, token: Token, pos: Location) -> Self {
        Self {
            error,
            token,
            loc: pos,
        }
    }
}

impl Display for ContextualCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({}): {}", self.token, self.loc, self.error)
    }
}
