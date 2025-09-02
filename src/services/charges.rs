use std::{borrow::Cow, fmt::Display, str::FromStr};

use anyhow::Result;
use async_trait::async_trait;
use compact_str::CompactString;
use rustis::{
    client::Client as ValkeyClient,
    commands::{CallBuilder, ScriptingCommands, StringCommands},
};

use crate::{
    commands::args::{ArgError, ArgResult, CommandArg},
    context::cmd::CommandContext,
};

#[derive(Debug, Clone, Copy)]
pub struct Charges(i64);

impl Charges {
    pub const ONE: Self = Self(1000);

    pub fn new(whole: u32, fraction: u32) -> Self {
        Self(whole as i64 * 1000 + fraction as i64)
    }

    pub fn as_i64(&self) -> i64 {
        self.0
    }
}

impl From<i64> for Charges {
    fn from(value: i64) -> Self {
        Self(value)
    }
}

impl Display for Charges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let whole = self.0 / 1000;
        let fraction = self.0 % 1000;
        if fraction == 0 {
            write!(f, "{whole}⚡︎")
        } else if fraction < 10 {
            write!(f, "{whole}.00{fraction}⚡︎")
        } else if fraction < 100 {
            if fraction % 10 == 0 {
                write!(f, "{whole}.{}⚡︎", fraction / 10)
            } else {
                write!(f, "{whole}.{fraction}⚡︎")
            }
        } else if fraction % 100 == 0 {
            write!(f, "{whole}.{}⚡︎", fraction / 100)
        } else if fraction % 10 == 0 {
            write!(f, "{whole}.{}⚡︎", fraction / 10)
        } else {
            write!(f, "{whole}.{fraction}⚡︎")
        }
    }
}

impl FromStr for Charges {
    type Err = &'static str;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let mut parts = input.splitn(2, '.');
        let whole = parts.next().unwrap();
        let fraction = parts.next().unwrap_or("0");
        let whole: i64 = match whole.parse() {
            Ok(n) => n,
            _ => return Err("charge amount must be a number"),
        };
        let fraction: i64 = match (fraction.len(), fraction.parse()) {
            (1, Ok(n)) => n * 100,
            (2, Ok(n)) => n * 10,
            (3, Ok(n)) => n,
            (_, Err(_)) => return Err("charge amount must be a number"),
            _ => return Err("charge amount can have at most 3 decimal places"),
        };
        Ok(Self(whole * 1000 + fraction))
    }
}

#[async_trait]
impl CommandArg for Charges {
    async fn parse(_ctx: &CommandContext, input: CompactString) -> ArgResult<Self> {
        input
            .parse()
            .map_err(|e: &str| ArgError::Precondition(e.into()))
    }

    fn type_desc() -> Cow<'static, str> {
        "a number of charges, in form of a number with up to 3 decimal places".into()
    }
}

#[async_trait]
pub trait ChargesService: Send + Sync {
    async fn get(&self, user_id: &str) -> Result<Charges>;
    async fn set(&self, user_id: &str, amount: Charges) -> Result<()>;
    async fn add(&self, user_id: &str, amount: Charges) -> Result<Charges>;
    async fn consume(&self, user_id: &str, amount: Charges) -> Result<bool>;
    async fn transfer(&self, from_user_id: &str, to_user_id: &str, amount: Charges)
    -> Result<bool>;
}

pub struct ChargesServiceRedis {
    client: ValkeyClient,
}

impl ChargesServiceRedis {
    pub fn new(client: ValkeyClient) -> Self {
        Self { client }
    }
}

fn key(user_id: &str) -> String {
    format!("charges:{user_id}")
}

#[async_trait]
impl ChargesService for ChargesServiceRedis {
    async fn get(&self, user_id: &str) -> Result<Charges> {
        Ok(self
            .client
            .get::<_, Option<i64>>(key(user_id))
            .await?
            .unwrap_or_default()
            .into())
    }

    async fn set(&self, user_id: &str, amount: Charges) -> Result<()> {
        self.client.set(key(user_id), amount.as_i64()).await?;
        Ok(())
    }

    async fn add(&self, user_id: &str, amount: Charges) -> Result<Charges> {
        Ok(self
            .client
            .incrby(key(user_id), amount.as_i64())
            .await?
            .into())
    }

    async fn consume(&self, user_id: &str, amount: Charges) -> Result<bool> {
        // this is so very atomic wohoo
        const SCRIPT: &str = r#"
            local user_id = KEYS[1]
            local amount = tonumber(ARGV[1])
            local current = tonumber(redis.call("GET", user_id)) or 0
            if current < amount then
                return
            end
            redis.call("DECRBY", user_id, amount)
            return true
        "#;

        Ok(self
            .client
            .eval::<bool>(
                CallBuilder::script(SCRIPT)
                    .keys(key(user_id))
                    .args(amount.as_i64()),
            )
            .await?)
    }

    async fn transfer(
        &self,
        from_user_id: &str,
        to_user_id: &str,
        amount: Charges,
    ) -> Result<bool> {
        if amount.as_i64() < 0 {
            return Ok(false);
        }
        if amount.as_i64() == 0 {
            return Ok(true);
        }
        const SCRIPT: &str = r#"
            local from = KEYS[1]
            local to = KEYS[2]
            local amount = tonumber(ARGV[1])
            local current = tonumber(redis.call("GET", from))
            if not current or not amount or current < amount then
                return
            end
            redis.call("DECRBY", from, amount)
            redis.call("INCRBY", to, amount)
            return true
        "#;

        Ok(self
            .client
            .eval::<bool>(
                CallBuilder::script(SCRIPT)
                    .keys([key(from_user_id), key(to_user_id)])
                    .args(amount.as_i64()),
            )
            .await?)
    }
}
