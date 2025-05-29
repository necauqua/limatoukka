use std::{
    borrow::Cow,
    collections::VecDeque,
    fmt::{self, Debug, Display},
    ops::Deref,
    time::Duration,
};

use anyhow::anyhow;
use async_trait::async_trait;
use humantime_serde::re::humantime;
use rustis::commands::{SetCondition, SetExpiration, StringCommands};
use thiserror::Error;

use crate::context::cmd::CommandContext;

use neca_cmd::{
    CommandMessage,
    calc::{Calculator, CalculatorError},
};

#[derive(Debug, Error)]
pub enum ExtractorError {
    #[error("argument #{idx}: {1}", idx = .0 + 1)]
    BadArgument(usize, ArgError),
    #[error("unexpected argument #{idx}: {1}", idx = .0 + 1)]
    UnexpectedArgument(usize, String),
}

#[derive(Debug, Clone)]
pub enum Arg<T> {
    Static(T),
    Expandable(neca_cmd::sub::Arg),
}

impl<T> Arg<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Arg<U> {
        match self {
            Arg::Static(t) => Arg::Static(f(t)),
            Arg::Expandable(arg) => Arg::Expandable(arg),
        }
    }

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

impl<T: CommandArg> Arg<T> {
    pub async fn get(self, ctx: &CommandContext) -> ArgResult<T> {
        match self {
            Arg::Static(t) => Ok(t),
            Arg::Expandable(arg) => {
                let arg = Some(arg.expand(&mut ctx.arg_expander())).filter(|s| !s.is_empty());
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
    args: VecDeque<String>,
    len: usize,
}

impl Args {
    pub fn new(args: VecDeque<String>) -> Self {
        Self {
            len: args.len(),
            args,
        }
    }

    pub fn current_idx(&self) -> usize {
        self.len - self.args.len()
    }

    pub fn pop(&mut self) -> Option<String> {
        self.args.pop_front().filter(|s| !s.is_empty())
    }
}

#[async_trait]
impl<T: CommandArg> ArgExtractor for T {
    async fn extract(ctx: &CommandContext, args: &mut Args) -> ExtractorResult<Arg<Self>> {
        let idx = args.current_idx();

        let arg = match args.pop() {
            None => None,
            Some(arg) => {
                let parsed = neca_cmd::sub::Arg::parse(&arg);
                // make sure to expand escapes
                match parsed.expand_static() {
                    Some(arg) => Some(arg),
                    None => return Ok(Arg::Expandable(parsed)),
                }
            }
        };

        T::parse_opt(ctx, arg)
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

    pub async fn get(self, ctx: &CommandContext) -> ArgResult<VecDeque<String>> {
        let mut args = VecDeque::with_capacity(self.args.len());
        for arg in self.args {
            args.push_back(arg.get(ctx).await?.unwrap_or_default());
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
    pub commands: CommandMessage,
}

#[async_trait]
impl ArgExtractor for RawScript {
    async fn extract(_ctx: &CommandContext, args: &mut Args) -> ExtractorResult<Arg<Self>> {
        let idx = args.current_idx();
        let arg = args
            .pop()
            .ok_or_else(|| ExtractorError::BadArgument(idx, ArgError::Missing))?;

        let commands = CommandMessage::parse(&arg);
        if commands.is_empty() {
            return Err(ExtractorError::BadArgument(
                idx,
                ArgError::Precondition("no commands in script".into()),
            ));
        }

        Ok(Arg::Static(Self { commands }))
    }

    fn type_desc() -> Cow<'static, str> {
        "a script string, will not have it's vars expanded".into()
    }
}

#[derive(Debug, Clone)]
pub struct Script {
    pub commands: CommandMessage,
}

#[async_trait]
impl CommandArg for Script {
    async fn parse(_ctx: &CommandContext, input: String) -> ArgResult<Self> {
        let commands = CommandMessage::parse(&input);
        if commands.is_empty() {
            return Err(ArgError::Precondition("no commands in script".into()));
        }
        Ok(Self { commands })
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
    async fn parse(ctx: &CommandContext, input: String) -> ArgResult<Self> {
        Self::parse_opt(ctx, Some(input)).await
    }

    async fn parse_opt(ctx: &CommandContext, input: Option<String>) -> ArgResult<Self> {
        Self::parse(ctx, input.ok_or(ArgError::Missing)?).await
    }

    fn type_desc() -> Cow<'static, str>;

    fn optional_desc() -> Option<Cow<'static, str>> {
        None
    }
}

#[async_trait]
impl<T: CommandArg> CommandArg for Option<T> {
    async fn parse_opt(ctx: &CommandContext, input: Option<String>) -> ArgResult<Self> {
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
    async fn parse(_ctx: &CommandContext, input: String) -> ArgResult<Self> {
        Ok(input)
    }

    fn type_desc() -> Cow<'static, str> {
        "string".into()
    }
}

#[async_trait]
impl CommandArg for i32 {
    async fn parse(_ctx: &CommandContext, input: String) -> ArgResult<Self> {
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
    async fn parse(_ctx: &CommandContext, input: String) -> ArgResult<Self> {
        Ok(Calculator::eval(&input)?)
    }

    fn type_desc() -> Cow<'static, str> {
        "a number".into()
    }
}

#[async_trait]
impl CommandArg for u32 {
    async fn parse(_ctx: &CommandContext, input: String) -> ArgResult<Self> {
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
    async fn parse_opt(ctx: &CommandContext, arg: Option<String>) -> ArgResult<Self> {
        let Some(input) = arg else {
            return Ok(Self(Duration::from_millis(DEFAULT as _)));
        };

        let millis = u32::parse(ctx, input).await?;

        if millis <= MAX {
            Ok(Self(Duration::from_millis(millis as _)))
        } else {
            Err(ArgError::Precondition(format!(
                "duration must be at most {MAX}"
            )))
        }
    }

    fn type_desc() -> Cow<'static, str> {
        format!("duration in milliseconds, at most {MAX}").into()
    }

    fn optional_desc() -> Option<Cow<'static, str>> {
        Some(format!("defaults to {DEFAULT}").into())
    }
}

#[async_trait]
impl CommandArg for Duration {
    async fn parse(_ctx: &CommandContext, input: String) -> ArgResult<Self> {
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
    async fn parse(ctx: &CommandContext, input: String) -> ArgResult<Self> {
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
    async fn parse_opt(ctx: &CommandContext, arg: Option<String>) -> ArgResult<Self> {
        let Some(login) = arg else {
            return Ok(ctx.shared.owner.clone());
        };

        let key = format!("chatter:{login}");
        let cached: Option<String> = ctx.storage().get(&key).await.map_err(|e| anyhow!(e))?;
        if let Some(id) = cached {
            return Ok(Self { id, login });
        }

        let full = ctx
            .twitch()
            .call(async |t| t.helix.get_user_from_login(&login, &t.token).await)
            .await
            .map_err(|e| anyhow!(e))?;
        let Some(user) = full else {
            return Err(ArgError::Precondition("user does not exist".into()));
        };

        ctx.storage()
            .set_with_options(
                key,
                user.id.as_str(),
                SetCondition::None,
                SetExpiration::Ex(3600),
                false,
            )
            .await
            .map_err(|e| anyhow!(e))?;

        Ok(Self {
            id: user.id.take(),
            login,
        })
    }

    fn type_desc() -> Cow<'static, str> {
        "a user login, defaults to you".into()
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
    async fn parse(ctx: &CommandContext, arg: String) -> ArgResult<Self> {
        Ok(Self(T::parse(ctx, arg.clone()).await?))
    }

    fn type_desc() -> Cow<'static, str> {
        "a user login".into()
    }
}
