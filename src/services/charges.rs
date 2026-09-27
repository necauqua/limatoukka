use std::{
    borrow::Cow,
    fmt::Display,
    ops::{Add, Neg, Sub},
    str::FromStr,
};

use anyhow::Result;
use async_trait::async_trait;
use compact_str::CompactString;
use rustis::{
    client::Client as ValkeyClient,
    commands::{CallBuilder, GenericCommands, ScanOptions, ScriptingCommands, StringCommands},
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
    pub const ZERO: Self = Self::whole(0);
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

impl Add for Charges {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Sub for Charges {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
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
        let whole = match whole {
            "" => "0",
            _ => whole,
        };
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

/// A command argument for an amount of charges: an exact amount, or an
/// amount relative to a balance (all of it, or a percentage of it).
///
/// The command decides which balance to [resolve](Self::resolve) it against.
#[derive(Debug, Clone, Copy)]
pub enum ChargesAmount {
    Exact(Charges),
    All,
    Percent(f64),
}

impl ChargesAmount {
    pub fn is_relative(&self) -> bool {
        !matches!(self, Self::Exact(_))
    }

    pub fn resolve(self, balance: Charges) -> Charges {
        match self {
            Self::Exact(amount) => amount,
            Self::All => balance,
            // truncates towards zero, so it never goes above the balance
            Self::Percent(percent) => Charges((balance.as_i64() as f64 * percent / 100.0) as i64),
        }
    }

    /// Resolve against the current balance of the given user. The balance
    /// is only read if the amount is relative.
    pub async fn resolve_for(self, charges: &dyn ChargesService, user_id: &str) -> Result<Charges> {
        let balance = if self.is_relative() {
            charges.get(user_id).await?
        } else {
            Charges::ZERO
        };
        Ok(self.resolve(balance))
    }
}

impl FromStr for ChargesAmount {
    type Err = &'static str;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if ["*", "all", "allin", "everything"].contains(&input) {
            return Ok(Self::All);
        }
        if let Some(percent) = input.strip_suffix("%") {
            return match percent.parse::<f64>() {
                Ok(percent) if percent.is_finite() => Ok(Self::Percent(percent)),
                _ => Err("invalid percentage"),
            };
        }
        input.parse().map(Self::Exact)
    }
}

#[async_trait]
impl CommandArg for ChargesAmount {
    async fn parse(_ctx: &CommandContext, input: CompactString) -> ArgResult<Self> {
        input
            .parse()
            .map_err(|e: &str| ArgError::Precondition(e.into()))
    }

    fn type_desc() -> Cow<'static, str> {
        "a number of charges, in form of a number with up to 3 decimal places. Alternatively, `*`/`all`/`allin`/`everything` for your current balance, or `<n>%` (e.g. `50%` or `12.5%`) for a percentage of it.".into()
    }
}

pub enum ConsumeResult {
    Fail,
    Success { bankrupt: bool },
}

impl ConsumeResult {
    pub fn is_fail(&self) -> bool {
        matches!(self, Self::Fail)
    }
}

#[async_trait]
pub trait ChargesService: Service {
    async fn get(&self, user_id: &str) -> Result<Charges>;

    async fn set(&self, user_id: &str, amount: Charges) -> Result<()>;

    async fn add(&self, user_id: &str, amount: Charges) -> Result<Charges>;

    async fn consume(&self, user_id: &str, amount: Charges) -> Result<ConsumeResult>;

    async fn transfer(&self, from_user_id: &str, to_user_id: &str, amount: Charges)
    -> Result<bool>;

    async fn get_all(&self) -> Result<Vec<(String, Charges)>>;
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

    async fn consume(&self, user_id: &str, amount: Charges) -> Result<ConsumeResult> {
        // this is so very atomic wohoo
        const SCRIPT: &str = r#"
            local user_id = KEYS[1]
            local amount = tonumber(ARGV[1])
            local current = tonumber(redis.call("GET", user_id)) or 0
            if amount > 0 and current < amount then
                return 0
            elseif current == amount then
                redis.call("SET", user_id, 0)
                return 2
            end
            redis.call("DECRBY", user_id, amount)
            return 1
        "#;

        let opts = CallBuilder::script(SCRIPT)
            .keys(key(user_id))
            .args(amount.as_i64());
        let res = self.client.eval::<i64>(opts).await?;

        Ok(match res {
            0 => ConsumeResult::Fail,
            1 => ConsumeResult::Success { bankrupt: false },
            2 => ConsumeResult::Success { bankrupt: true },
            _ => unreachable!(),
        })
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

    async fn get_all(&self) -> Result<Vec<(String, Charges)>> {
        let mut result = Vec::new();

        let mut cursor = 0u64;
        loop {
            let (next, batch): (u64, Vec<String>) = self
                .client
                .scan(
                    cursor,
                    ScanOptions::default().match_pattern(key("*")).count(500),
                )
                .await?;
            if !batch.is_empty() {
                let values: Vec<Option<String>> = self.client.mget(&*batch).await?;
                for (k, v) in batch.into_iter().zip(values) {
                    result.push((
                        k.strip_prefix("charges:").unwrap_or(&k).to_owned(),
                        Charges::from(v.and_then(|s| s.parse::<i64>().ok()).unwrap_or_default()),
                    ));
                }
            }
            if next == 0 {
                break;
            }
            cursor = next;
        }
        Ok(result)
    }
}

#[derive(Default, Debug)]
pub struct ChargesServiceInMemory {
    balances: dashmap::DashMap<String, i64>,
}

#[async_trait]
impl ChargesService for ChargesServiceInMemory {
    async fn get(&self, user_id: &str) -> Result<Charges> {
        Ok(self.balances.get(user_id).map_or(0, |v| *v).into())
    }

    async fn set(&self, user_id: &str, amount: Charges) -> Result<()> {
        self.balances.insert(user_id.into(), amount.as_i64());
        Ok(())
    }

    async fn add(&self, user_id: &str, amount: Charges) -> Result<Charges> {
        let mut balance = self.balances.entry(user_id.into()).or_default();
        *balance += amount.as_i64();
        Ok((*balance).into())
    }

    async fn consume(&self, user_id: &str, amount: Charges) -> Result<ConsumeResult> {
        let mut balance = self.balances.entry(user_id.into()).or_default();
        let amount = amount.as_i64();
        if amount > 0 && *balance < amount {
            return Ok(ConsumeResult::Fail);
        }
        *balance -= amount;
        Ok(ConsumeResult::Success {
            bankrupt: *balance == 0,
        })
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
        if self.consume(from_user_id, amount).await?.is_fail() {
            return Ok(false);
        }
        self.add(to_user_id, amount).await?;
        Ok(true)
    }

    async fn get_all(&self) -> Result<Vec<(String, Charges)>> {
        Ok(self
            .balances
            .iter()
            .map(|e| (e.key().clone(), (*e.value()).into()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charges_parsing() {
        assert_eq!(".1".parse::<Charges>().unwrap().as_i64(), 100);
        assert_eq!(".01".parse::<Charges>().unwrap().as_i64(), 10);
        assert_eq!(".001".parse::<Charges>().unwrap().as_i64(), 1);

        assert_eq!("0".parse::<Charges>().unwrap().as_i64(), 0);
        assert_eq!("1".parse::<Charges>().unwrap().as_i64(), 1000);
        assert_eq!("10".parse::<Charges>().unwrap().as_i64(), 10000);
        assert_eq!("100".parse::<Charges>().unwrap().as_i64(), 100000);
        assert_eq!("1000".parse::<Charges>().unwrap().as_i64(), 1000000);
        assert_eq!("1234".parse::<Charges>().unwrap().as_i64(), 1234000);
        assert_eq!("0.1".parse::<Charges>().unwrap().as_i64(), 100);
        assert_eq!("0.12".parse::<Charges>().unwrap().as_i64(), 120);
        assert_eq!("0.123".parse::<Charges>().unwrap().as_i64(), 123);
        assert_eq!("1.1".parse::<Charges>().unwrap().as_i64(), 1100);
        assert_eq!("1.12".parse::<Charges>().unwrap().as_i64(), 1120);
        assert_eq!("1.123".parse::<Charges>().unwrap().as_i64(), 1123);
        assert_eq!("1234.123".parse::<Charges>().unwrap().as_i64(), 1234123);
        assert_eq!("-0".parse::<Charges>().unwrap().as_i64(), 0);
        assert_eq!("-1".parse::<Charges>().unwrap().as_i64(), -1000);
        assert_eq!("-10".parse::<Charges>().unwrap().as_i64(), -10000);
        assert_eq!("-100".parse::<Charges>().unwrap().as_i64(), -100000);
        assert_eq!("-1000".parse::<Charges>().unwrap().as_i64(), -1000000);
        assert_eq!("-1234".parse::<Charges>().unwrap().as_i64(), -1234000);
        assert_eq!("-0.1".parse::<Charges>().unwrap().as_i64(), -100);
        assert_eq!("-0.12".parse::<Charges>().unwrap().as_i64(), -120);
        assert_eq!("-0.123".parse::<Charges>().unwrap().as_i64(), -123);
        assert_eq!("-1.1".parse::<Charges>().unwrap().as_i64(), -1100);
        assert_eq!("-1.12".parse::<Charges>().unwrap().as_i64(), -1120);
        assert_eq!("-1.123".parse::<Charges>().unwrap().as_i64(), -1123);
        assert_eq!("-1234.123".parse::<Charges>().unwrap().as_i64(), -1234123);
    }

    #[test]
    fn charges_amount() {
        let resolve = |s: &str, balance: i64| {
            s.parse::<ChargesAmount>()
                .map(|a| a.resolve(balance.into()).as_i64())
        };
        assert_eq!(resolve("1.5", 10_000), Ok(1500));
        assert_eq!(resolve("-1", 10_000), Ok(-1000));
        assert_eq!(resolve("all", 10_000), Ok(10_000));
        assert_eq!(resolve("*", 10_000), Ok(10_000));
        assert_eq!(resolve("50%", 10_000), Ok(5000));
        assert_eq!(resolve("100%", 12_345), Ok(12_345));
        assert_eq!(resolve("12.5%", 10_000), Ok(1250));
        assert_eq!(resolve("33.3333%", 1000), Ok(333));
        assert_eq!(resolve(".5%", 10_000), Ok(50));
        assert_eq!(resolve("-50%", 10_000), Ok(-5000));
        assert!(resolve("abc%", 10_000).is_err());
        assert!(resolve("inf%", 10_000).is_err());
        assert!(resolve("NaN%", 10_000).is_err());
        assert!(resolve("1.2345", 10_000).is_err());
    }

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
