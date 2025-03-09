use std::{borrow::Cow, collections::VecDeque, fmt::Debug};

use thiserror::Error;

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
    rest: Option<String>,
    len: usize,
    rest_taken: bool,
}

impl Args {
    pub fn new(args: VecDeque<String>, rest: Option<String>) -> Self {
        Self {
            len: args.len(),
            args,
            rest,
            rest_taken: false,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
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

#[derive(Debug, Clone)]
pub struct RestArg(String);

impl RestArg {
    pub fn get(self) -> String {
        self.0
    }
}

impl ArgExtractor for RestArg {
    fn extract(args: &mut Args) -> ExtractorResult<Self> {
        if args.rest_taken {
            panic!("double RestArg")
        }
        args.rest_taken = true; // a separate bool cuz we allow using last arg as rest
        args.rest
            .take()
            .or_else(|| args.args.pop_back())
            .map(Self)
            .ok_or(ExtractorError::MissingRestArgument)
    }

    fn type_desc() -> Cow<'static, str> {
        "string or rest of message".into()
    }
}

#[derive(Debug, Error)]
pub enum ArgError {
    #[error("wrong argument type, expected {0}")]
    WrongType(&'static str),
    #[error("{0}")]
    Precondition(String),
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
        input
            .parse()
            .map_err(|_| ArgError::WrongType("non-negative number"))
    }

    fn type_desc() -> Cow<'static, str> {
        "number".into()
    }
}

impl CommandArg for u32 {
    fn parse(input: String) -> ArgResult<Self> {
        input.parse().map_err(|_| ArgError::WrongType("number"))
    }

    fn type_desc() -> Cow<'static, str> {
        "non-negative number".into()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AtMost<const N: u32>(u32);

impl<const N: u32> AtMost<N> {
    pub fn get(self) -> u32 {
        self.0
    }
}

impl<const N: u32> CommandArg for AtMost<N> {
    fn parse(input: String) -> ArgResult<Self> {
        let n = u32::parse(input)?;
        if n <= N {
            Ok(Self(n))
        } else {
            Err(ArgError::Precondition(format!("can be at most {N}")))
        }
    }

    fn type_desc() -> Cow<'static, str> {
        format!("non-negative number, at most {N}").into()
    }
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
