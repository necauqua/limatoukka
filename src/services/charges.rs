use std::{borrow::Cow, fmt::Display, ops::Neg, str::FromStr};

use anyhow::Result;
use async_trait::async_trait;
use compact_str::CompactString;
use rustis::{
    client::Client as ValkeyClient,
    commands::{CallBuilder, ScriptingCommands, StringCommands},
};
use serde::{Deserialize, Serialize};

use crate::{
    commands::args::{ArgError, ArgResult, CommandArg},
    context::cmd::CommandContext,
    injector_getter,
    services::Service,
};

#[derive(Default, Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Charges(i64);

impl Charges {
    pub const ONE: Self = Self::whole(1);

    pub const fn is_zero(&self) -> bool {
        self.0 == 0
    }

    pub const fn non_zero(&self) -> bool {
        self.0 != 0
    }

    pub const fn whole(whole: i64) -> Self {
        Self(whole * 1000)
    }

    pub const fn as_i64(&self) -> i64 {
        self.0
    }

    pub const fn as_u64(&self) -> u64 {
        self.0 as _
    }
}

impl Neg for Charges {
    type Output = Self;

    fn neg(self) -> Self::Output {
        Self(-self.0)
    }
}

impl From<u64> for Charges {
    fn from(value: u64) -> Self {
        Self(value as _)
    }
}

impl From<i64> for Charges {
    fn from(value: i64) -> Self {
        Self(value)
    }
}

// bare numbers default to i32 in Rust apparently
impl From<i32> for Charges {
    fn from(value: i32) -> Self {
        Self(value as _)
    }
}

impl Display for Charges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sig = match self.0.cmp(&0) {
            std::cmp::Ordering::Less => "-",
            _ => "",
        };
        let whole = (self.0 / 1000).abs();
        let fraction = (self.0 % 1000).abs();
        if fraction == 0 {
            write!(f, "{sig}{whole}⚡︎")
        } else if fraction < 10 {
            write!(f, "{sig}{whole}.00{fraction}⚡︎")
        } else if fraction < 100 {
            if fraction % 10 == 0 {
                write!(f, "{sig}{whole}.0{}⚡︎", fraction / 10)
            } else {
                write!(f, "{sig}{whole}.0{fraction}⚡︎")
            }
        } else if fraction % 100 == 0 {
            write!(f, "{sig}{whole}.{}⚡︎", fraction / 100)
        } else if fraction % 10 == 0 {
            write!(f, "{sig}{whole}.{}⚡︎", fraction / 10)
        } else {
            write!(f, "{sig}{whole}.{fraction}⚡︎")
        }
    }
}

impl FromStr for Charges {
    type Err = &'static str;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let mut parts = input.splitn(2, '.');
        let whole = parts.next().unwrap();
        let (neg, whole) = match whole.strip_prefix("-") {
            Some(whole) => (true, whole),
            None => (false, whole),
        };
        let fraction = parts.next().unwrap_or("0");
        let Ok(whole) = whole.parse::<i64>() else {
            return Err("charge amount must be a number");
        };
        let fraction: i64 = match (fraction.len(), fraction.parse()) {
            (1, Ok(n)) => n * 100,
            (2, Ok(n)) => n * 10,
            (3, Ok(n)) => n,
            (_, Err(_)) => return Err("charge amount must be a number"),
            _ => return Err("charge amount can have at most 3 decimal places"),
        };
        Ok(Self(if neg { -1 } else { 1 } * (whole * 1000 + fraction)))
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
pub trait ChargesService: Service {
    async fn get(&self, user_id: &str) -> Result<Charges>;

    async fn set(&self, user_id: &str, amount: Charges) -> Result<()>;

    async fn add(&self, user_id: &str, amount: Charges) -> Result<Charges>;

    async fn consume(&self, user_id: &str, amount: Charges) -> Result<bool>;

    async fn transfer(&self, from_user_id: &str, to_user_id: &str, amount: Charges)
    -> Result<bool>;
}

injector_getter!(ChargesService::charges);

pub struct ChargesServiceValkey {
    client: ValkeyClient,
}

impl ChargesServiceValkey {
    pub fn new(client: ValkeyClient) -> Self {
        Self { client }
    }
}

fn key(user_id: &str) -> String {
    format!("charges:{user_id}")
}

#[async_trait]
impl ChargesService for ChargesServiceValkey {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charges_display() {
        assert_eq!(Charges::from(0).to_string(), "0⚡︎");
        assert_eq!(Charges::from(1).to_string(), "0.001⚡︎");
        assert_eq!(Charges::from(10).to_string(), "0.01⚡︎");
        assert_eq!(Charges::from(100).to_string(), "0.1⚡︎");
        assert_eq!(Charges::from(1000).to_string(), "1⚡︎");
        assert_eq!(Charges::from(1234).to_string(), "1.234⚡︎");
        assert_eq!(Charges::from(1200).to_string(), "1.2⚡︎");
        assert_eq!(Charges::from(1230).to_string(), "1.23⚡︎");

        assert_eq!(Charges::from(-1).to_string(), "-0.001⚡︎");
        assert_eq!(Charges::from(-10).to_string(), "-0.01⚡︎");
        assert_eq!(Charges::from(-100).to_string(), "-0.1⚡︎");
        assert_eq!(Charges::from(-1000).to_string(), "-1⚡︎");
        assert_eq!(Charges::from(-1234).to_string(), "-1.234⚡︎");
        assert_eq!(Charges::from(-1200).to_string(), "-1.2⚡︎");
        assert_eq!(Charges::from(-1230).to_string(), "-1.23⚡︎");

        assert_eq!(Charges::from(10000).to_string(), "10⚡︎");
        assert_eq!(Charges::from(-10000).to_string(), "-10⚡︎");

        assert_eq!(Charges::from(i64::MAX).to_string(), "9223372036854775.807⚡︎");
        assert_eq!(
            Charges::from(i64::MIN).to_string(),
            "-9223372036854775.808⚡︎"
        );

        assert_eq!(
            Charges::from(i64::MAX / 1000 * 1000).to_string(),
            "9223372036854775⚡︎"
        );
        assert_eq!(
            Charges::from(-i64::MAX / 1000 * 1000).to_string(),
            "-9223372036854775⚡︎"
        );
    }
}
