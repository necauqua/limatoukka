use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    fmt::{self, Debug, Display},
    ops::Deref,
    time::Duration,
};

use async_trait::async_trait;
use compact_str::{CompactString, ToCompactString as _};
use humantime_serde::re::humantime;
use thiserror::Error;
use tokio::task::JoinSet;

use crate::{
    context::{app::AppContext, cmd::CommandContext},
    services::{charges::ChargesService, twitch::TwitchService},
};

use neca_cmd::{
    Statement,
    calc::{Calculator, CalculatorError},
    param::{Param, ParamType},
};

#[derive(Debug, Error)]
pub enum ExtractorError {
    #[error("argument #{idx}: {1}", idx = .0 + 1)]
    BadArgument(usize, ArgError),
    #[error("unexpected argument #{idx}: {1}", idx = .0 + 1)]
    UnexpectedArgument(usize, Param),
}

#[derive(Debug, Clone)]
pub enum Arg<T> {
    Static(T),
    Expandable(Param),
}

impl<T> Arg<T> {
    pub fn unwrap_static(self) -> T {
        match self {
            Arg::Static(t) => t,
            Arg::Expandable(_) => panic!("expected static arg"),
        }
    }
}

// no specialization :(
impl Arg<RestOfArgs> {
    pub async fn get(self, _ctx: &CommandContext) -> ArgResult<RestOfArgs> {
        Ok(self.unwrap_static())
    }
}

impl Arg<RawScript> {
    pub async fn get(self, _ctx: &CommandContext) -> ArgResult<RawScript> {
        Ok(self.unwrap_static())
    }
}

async fn var_resolvers(ctx: &CommandContext, name: &str) -> anyhow::Result<Option<String>> {
    match name {
        "i" => {
            if let Some(i) = ctx.repeat_i {
                return Ok(Some(i.to_string()));
            }
        }
        "self" => return Ok(Some(ctx.shared.owner.login.clone())),
        "rand" => return Ok(Some(rand::random_range(0..100_i32).to_string())),
        "volume" => {
            let volume = AppContext::just("music-volume-get", &[])?.check().await?;
            return Ok(Some(volume));
        }
        "balance" => {
            let balance = ctx
                .service::<dyn ChargesService>()
                .get(ctx.sender())
                .await?;
            return Ok(Some(balance.as_i64().to_string()));
        }
        _ => {}
    }
    if let Some(arg) = name.parse::<u32>().ok().filter(|n| *n != 0).and_then(|n| {
        ctx.shared
            .macro_args
            .get((n - 1) as _)
            .and_then(|opt| opt.as_ref())
    }) {
        return Ok(Some(arg.clone()));
    }
    Ok(ctx.vars.get(name).map(|v| v.clone()))
}

async fn expand(ctx: &CommandContext, param: Param) -> anyhow::Result<CompactString> {
    let refs = param.references();

    Ok(match &refs[..] {
        [] => param.expand(|_| None),
        [name] => {
            let resolved = var_resolvers(ctx, name).await?;
            param.expand(|_| resolved.clone())
        }
        _ => {
            let mut join_set = JoinSet::new();
            for name in refs {
                let ctx = ctx.clone();
                join_set.spawn(async move {
                    let resolved = var_resolvers(&ctx, &name).await;
                    (name, resolved)
                });
            }
            let mut results = HashMap::new();
            while let Some(res) = join_set.join_next().await {
                let (k, v) = res?;
                if let Some(v) = v? {
                    results.insert(k, v);
                }
            }
            param.expand(|k| results.get(k).cloned())
        }
    })
}

impl<T: CommandArg> Arg<T> {
    pub async fn get(self, ctx: &CommandContext) -> ArgResult<T> {
        match self {
            Arg::Static(t) => Ok(t),
            Arg::Expandable(param) => {
                let tpe = param.tpe();
                let arg = Some(expand(ctx, param).await?)
                    .filter(|s| !s.is_empty())
                    .map(|s| match tpe {
                        ParamType::Math => Calculator::eval(&s).map(|n| n.to_compact_string()),
                        _ => Ok(s),
                    })
                    .transpose()?;
                Ok(CommandArg::parse_opt(ctx, arg).await?)
            }
        }
    }
}

pub type ExtractorResult<T> = Result<T, ExtractorError>;

#[async_trait]
pub trait ArgExtractor: Sized {
    async fn extract(ctx: &CommandContext, args: &mut Args) -> ExtractorResult<Arg<Self>>;

    fn type_desc() -> Cow<'static, str>;

    fn optional_desc() -> Option<Cow<'static, str>> {
        None
    }
}

pub struct Args {
    args: VecDeque<Param>,
    len: usize,
}

impl Args {
    pub fn new(args: VecDeque<Param>) -> Self {
        Self {
            len: args.len(),
            args,
        }
    }

    pub fn current_idx(&self) -> usize {
        self.len - self.args.len()
    }

    pub fn pop(&mut self) -> Option<Param> {
        self.args.pop_front()
    }
}

#[async_trait]
impl<T: CommandArg> ArgExtractor for T {
    async fn extract(ctx: &CommandContext, args: &mut Args) -> ExtractorResult<Arg<Self>> {
        let idx = args.current_idx();

        let arg = match args.pop() {
            None => None,
            Some(arg) => match arg.expand_static() {
                Some(text) => Some(match arg.tpe() {
                    ParamType::Math => match Calculator::eval(&text) {
                        Ok(n) => n.to_compact_string(),
                        Err(e) => return Err(ExtractorError::BadArgument(idx, e.into())),
                    },
                    _ => text,
                }),
                None => return Ok(Arg::Expandable(arg)),
            },
        };

        T::parse_opt(ctx, arg.filter(|s| !s.is_empty()))
            .await
            .map_err(|e| ExtractorError::BadArgument(idx, e))
            .map(Arg::Static)
    }

    fn type_desc() -> Cow<'static, str> {
        <T as CommandArg>::type_desc()
    }

    fn optional_desc() -> Option<Cow<'static, str>> {
        <T as CommandArg>::optional_desc()
    }
}

#[derive(Debug, Clone)]
pub struct RestOfArgs {
    args: Vec<Arg<Option<String>>>,
}

impl RestOfArgs {
    pub fn is_empty(&self) -> bool {
        self.args.is_empty()
    }

    pub async fn get(self, ctx: &CommandContext) -> ExtractorResult<VecDeque<Option<String>>> {
        let mut args = VecDeque::with_capacity(self.args.len());
        for (i, arg) in self.args.into_iter().enumerate() {
            args.push_back(
                arg.get(ctx)
                    .await
                    .map_err(|e| ExtractorError::BadArgument(i, e))?,
            );
        }
        Ok(args)
    }
}

#[async_trait]
impl ArgExtractor for RestOfArgs {
    async fn extract(ctx: &CommandContext, args: &mut Args) -> ExtractorResult<Arg<Self>> {
        let mut rest = Vec::with_capacity(args.args.len());
        while !args.args.is_empty() {
            rest.push(ArgExtractor::extract(ctx, args).await?);
        }
        Ok(Arg::Static(Self { args: rest }))
    }

    fn type_desc() -> Cow<'static, str> {
        "the rest of the arguments".into()
    }
}

#[derive(Debug, Clone)]
pub struct RawScript {
    pub stmt: Statement,
}

#[async_trait]
impl ArgExtractor for RawScript {
    async fn extract(_ctx: &CommandContext, args: &mut Args) -> ExtractorResult<Arg<Self>> {
        let idx = args.current_idx();
        let arg = args
            .pop()
            .ok_or_else(|| ExtractorError::BadArgument(idx, ArgError::Missing))?;

        let stmt = Statement::parse(arg.text());
        if stmt.is_noop() {
            return Err(ExtractorError::BadArgument(
                idx,
                ArgError::Precondition("no commands in script".into()),
            ));
        }

        Ok(Arg::Static(Self { stmt }))
    }

    fn type_desc() -> Cow<'static, str> {
        "a script string that _will not have it's vars expanded_".into()
    }
}

#[derive(Debug, Clone)]
pub struct Script {
    pub stmt: Statement,
}

#[async_trait]
impl CommandArg for Script {
    async fn parse(_ctx: &CommandContext, input: CompactString) -> ArgResult<Self> {
        let stmt = Statement::parse(&input);
        if stmt.is_noop() {
            return Err(ArgError::Precondition("no commands in script".into()));
        }
        Ok(Self { stmt })
    }

    fn type_desc() -> Cow<'static, str> {
        "a script string".into()
    }
}

#[derive(Debug, Error)]
pub enum ArgError {
    #[error("missing")]
    Missing,
    #[error("wrong type, expected {0}")]
    WrongType(&'static str),
    #[error("{0}")]
    Math(#[from] CalculatorError),
    #[error("{0}")]
    Precondition(String),
    #[error("internal")]
    Internal(#[from] anyhow::Error),
}

pub type ArgResult<T> = Result<T, ArgError>;

#[async_trait]
pub trait CommandArg: Send + Sized {
    async fn parse(ctx: &CommandContext, input: CompactString) -> ArgResult<Self> {
        Self::parse_opt(ctx, Some(input)).await
    }

    async fn parse_opt(ctx: &CommandContext, input: Option<CompactString>) -> ArgResult<Self> {
        Self::parse(ctx, input.ok_or(ArgError::Missing)?).await
    }

    fn type_desc() -> Cow<'static, str>;

    fn optional_desc() -> Option<Cow<'static, str>> {
        None
    }
}

#[async_trait]
impl<T: CommandArg> CommandArg for Option<T> {
    async fn parse_opt(ctx: &CommandContext, input: Option<CompactString>) -> ArgResult<Self> {
        Ok(match input {
            Some(input) => Some(T::parse(ctx, input).await?),
            None => None,
        })
    }

    fn type_desc() -> Cow<'static, str> {
        T::type_desc()
    }

    fn optional_desc() -> Option<Cow<'static, str>> {
        Some("optional".into())
    }
}

#[async_trait]
impl CommandArg for String {
    async fn parse(_ctx: &CommandContext, input: CompactString) -> ArgResult<Self> {
        Ok(input.into()) // todo maybe change String to CompactString in defs
    }

    fn type_desc() -> Cow<'static, str> {
        "string".into()
    }
}

#[async_trait]
impl CommandArg for bool {
    async fn parse(_ctx: &CommandContext, input: CompactString) -> ArgResult<Self> {
        match input.to_lowercase().trim() {
            "true" | "t" | "yes" | "y" | "1" => Ok(true),
            "false" | "f" | "no" | "n" | "0" => Ok(false),
            _ => Err(ArgError::WrongType(
                "a boolean value (true/t/yes/y/1 or false/f/no/n/0)",
            )),
        }
    }

    fn type_desc() -> Cow<'static, str> {
        "a boolean value (true/t/yes/y/1 or false/f/no/n/0)".into()
    }
}

#[async_trait]
impl CommandArg for i32 {
    async fn parse(_ctx: &CommandContext, input: CompactString) -> ArgResult<Self> {
        Calculator::eval(&input)?
            .try_into()
            .map_err(|_| ArgError::WrongType("a number"))
    }

    fn type_desc() -> Cow<'static, str> {
        "a number".into()
    }
}

#[async_trait]
impl CommandArg for i64 {
    async fn parse(_ctx: &CommandContext, input: CompactString) -> ArgResult<Self> {
        Ok(Calculator::eval(&input)?)
    }

    fn type_desc() -> Cow<'static, str> {
        "a number".into()
    }
}

#[async_trait]
impl CommandArg for u32 {
    async fn parse(_ctx: &CommandContext, input: CompactString) -> ArgResult<Self> {
        Calculator::eval(&input)?
            .try_into()
            .map_err(|_| ArgError::WrongType("a non-negative number"))
    }

    fn type_desc() -> Cow<'static, str> {
        "a non-negative number".into()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HoldTime<const DEFAULT: u32 = 500, const MAX: u32 = 15_000>(Duration);

impl<const DEFAULT: u32, const MAX: u32> HoldTime<DEFAULT, MAX> {
    pub fn get(self) -> Duration {
        self.0
    }
}

#[async_trait]
impl<const DEFAULT: u32, const MAX: u32> CommandArg for HoldTime<DEFAULT, MAX> {
    async fn parse_opt(ctx: &CommandContext, arg: Option<CompactString>) -> ArgResult<Self> {
        let Some(input) = arg else {
            return Ok(Self(Duration::from_millis(DEFAULT as _)));
        };
        let millis = u32::parse(ctx, input).await?;
        Ok(Self(Duration::from_millis(millis.min(MAX) as _)))
    }

    fn type_desc() -> Cow<'static, str> {
        format!("duration in milliseconds, will be limited to at most {MAX}").into()
    }

    fn optional_desc() -> Option<Cow<'static, str>> {
        Some(format!("defaults to {DEFAULT}").into())
    }
}

#[async_trait]
impl CommandArg for Duration {
    async fn parse(_ctx: &CommandContext, input: CompactString) -> ArgResult<Self> {
        humantime::parse_duration(&input).map_err(|e| ArgError::Precondition(e.to_string()))
    }

    fn type_desc() -> Cow<'static, str> {
        "duration in freeform format (using `humantime` Rust library), so `10s`, or `2 minutes` or `1h30m` etc".into()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct InRange<const A: u32, const B: u32>(u32);

impl<const A: u32, const B: u32> InRange<A, B> {
    pub fn get(self) -> u32 {
        self.0
    }
}

#[async_trait]
impl<const A: u32, const B: u32> CommandArg for InRange<A, B> {
    async fn parse(ctx: &CommandContext, input: CompactString) -> ArgResult<Self> {
        let n = u32::parse(ctx, input).await?;
        if n >= A && n <= B {
            Ok(Self(n))
        } else {
            Err(ArgError::Precondition(format!(
                "can be at least {A} and at most {B}"
            )))
        }
    }

    fn type_desc() -> Cow<'static, str> {
        format!("a number in range from {A} to {B}").into()
    }
}

#[derive(Debug, Clone)]
pub struct Chatter {
    pub id: String,
    pub login: String,
}

// most often used as part of a redis key
impl Display for Chatter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.id)
    }
}

#[async_trait]
impl CommandArg for Chatter {
    async fn parse_opt(ctx: &CommandContext, arg: Option<CompactString>) -> ArgResult<Self> {
        let Some(login) = arg else {
            return Ok(ctx.shared.owner.clone());
        };
        let login = login.trim().to_lowercase();

        let id = ctx
            .storage()
            .cache(Duration::from_secs(24 * 60 * 60), "twitch-id")?
            .get(&login, async || {
                let user_id = ctx
                    .service::<dyn TwitchService>()
                    .get_user_id(&login)
                    .await?;
                let Some(user_id) = user_id else {
                    return Err(ArgError::Precondition(format!("user {login} not found")));
                };
                Ok(user_id)
            })
            .await?;

        Ok(Self { id, login })
    }

    fn type_desc() -> Cow<'static, str> {
        "a user login".into()
    }

    fn optional_desc() -> Option<Cow<'static, str>> {
        Some("defaults to you".into())
    }
}

#[derive(Debug, Clone)]
pub struct Required<T>(T);

impl<T> Deref for Required<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: Display> Display for Required<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[async_trait]
impl<T: CommandArg> CommandArg for Required<T> {
    async fn parse(ctx: &CommandContext, arg: CompactString) -> ArgResult<Self> {
        Ok(Self(T::parse(ctx, arg).await?))
    }

    fn type_desc() -> Cow<'static, str> {
        T::type_desc()
    }
}
