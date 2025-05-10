use std::{borrow::Cow, collections::VecDeque, fmt::Debug, time::Duration};

use thiserror::Error;

use super::calculator::{Calculator, CalculatorError};

#[derive(Debug, Error)]
pub enum ExtractorError {
    #[error("missing argument #{}", .0 + 1)]
    MissingArgument(usize),
    #[error("missing rest argument (last arg or rest of message)")]
    MissingRestArgument,
    #[error("argument #{idx}: {1}", idx = .0 + 1)]
    BadArgument(usize, ArgError),
    #[error("unexpected argument #{idx}: {1}", idx = .0 + 1)]
    UnexpectedArgument(usize, String),
}

pub type ExtractorResult<T> = Result<T, ExtractorError>;

pub trait ArgExtractor: Sized {
    fn type_desc() -> Cow<'static, str>;

    fn extract(args: &mut Args) -> ExtractorResult<Self>;

    const OPTIONAL: bool = false;
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

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn pop(&mut self) -> Option<(usize, String)> {
        let idx = self.len - self.args.len();
        self.args.pop_front().map(|s| (idx, s))
    }

    pub fn extract<T: ArgExtractor>(&mut self) -> ExtractorResult<T> {
        T::extract(self)
    }
}

impl<T: CommandArg> ArgExtractor for T {
    fn extract(args: &mut Args) -> ExtractorResult<Self> {
        args.pop()
            .filter(|(_, s)| !s.is_empty())
            .ok_or(args.len())
            .map_err(ExtractorError::MissingArgument)
            .and_then(|(i, s)| T::parse(s).map_err(|e| ExtractorError::BadArgument(i, e)))
    }

    fn type_desc() -> Cow<'static, str> {
        <T as CommandArg>::type_desc()
    }
}

impl<T: ArgExtractor> ArgExtractor for Option<T> {
    fn extract(args: &mut Args) -> ExtractorResult<Self> {
        match T::extract(args) {
            Ok(t) => Ok(Some(t)),
            Err(ExtractorError::MissingArgument(_) | ExtractorError::MissingRestArgument) => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn type_desc() -> Cow<'static, str> {
        format!("{}, optional", T::type_desc()).into()
    }

    const OPTIONAL: bool = true;
}

impl ArgExtractor for VecDeque<String> {
    fn extract(args: &mut Args) -> ExtractorResult<Self> {
        Ok(std::mem::take(&mut args.args))
    }

    fn type_desc() -> Cow<'static, str> {
        "the rest of the arguments".into()
    }
}

#[derive(Debug, Error)]
pub enum ArgError {
    #[error("wrong argument type, expected {0}")]
    WrongType(&'static str),
    #[error("{0}")]
    Precondition(String),
}

impl From<CalculatorError> for ArgError {
    fn from(value: CalculatorError) -> Self {
        Self::Precondition(value.to_string())
    }
}

pub type ArgResult<T> = Result<T, ArgError>;

pub trait CommandArg: Sized {
    fn parse(input: String) -> ArgResult<Self>;

    fn type_desc() -> Cow<'static, str>;
}

impl CommandArg for String {
    fn parse(input: String) -> ArgResult<Self> {
        Ok(input)
    }

    fn type_desc() -> Cow<'static, str> {
        "string".into()
    }
}

impl CommandArg for i32 {
    fn parse(input: String) -> ArgResult<Self> {
        Calculator::eval(&input)?
            .try_into()
            .map_err(|_| ArgError::WrongType("a number"))
    }

    fn type_desc() -> Cow<'static, str> {
        "a number".into()
    }
}

impl CommandArg for u32 {
    fn parse(input: String) -> ArgResult<Self> {
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

impl<const DEFAULT: u32, const MAX: u32> ArgExtractor for HoldTime<DEFAULT, MAX> {
    fn extract(args: &mut Args) -> ExtractorResult<Self> {
        let Some((pos, input)) = args.pop().filter(|(_, s)| !s.is_empty()) else {
            return Ok(Self(Duration::from_millis(DEFAULT as _)));
        };

        let millis = u32::parse(input).map_err(|e| ExtractorError::BadArgument(pos, e))?;

        if millis > MAX {
            Err(ExtractorError::BadArgument(
                pos,
                ArgError::Precondition(format!("duration must be at most {MAX}")),
            ))
        } else {
            Ok(Self(Duration::from_millis(millis as _)))
        }
    }

    fn type_desc() -> Cow<'static, str> {
        format!("duration in milliseconds, at most {MAX}, defaults to {DEFAULT}. You can also specify whole seconds by appending 's'").into()
    }

    const OPTIONAL: bool = true;
}

#[derive(Debug, Clone, Copy)]
pub struct InRange<const A: u32, const B: u32>(u32);

impl<const A: u32, const B: u32> InRange<A, B> {
    pub fn get(self) -> u32 {
        self.0
    }
}

impl<const A: u32, const B: u32> CommandArg for InRange<A, B> {
    fn parse(input: String) -> ArgResult<Self> {
        let n = u32::parse(input)?;
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
