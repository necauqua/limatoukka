use anyhow::Result;
use async_trait::async_trait;
use rustis::{
    client::Client as ValkeyClient,
    commands::{CallBuilder, ScriptingCommands, StringCommands},
};

#[async_trait]
pub trait ChargesService: Send + Sync {
    async fn get(&self, user_id: &str) -> Result<i64>;
    async fn add(&self, user_id: &str, amount: i64) -> Result<i64>;
    async fn consume(&self, user_id: &str, amount: u64) -> Result<bool>;
    async fn transfer(&self, from_user_id: &str, to_user_id: &str, amount: i64) -> Result<bool>;
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
    async fn get(&self, user_id: &str) -> Result<i64> {
        Ok(self
            .client
            .get::<_, Option<i64>>(key(user_id))
            .await?
            .unwrap_or_default())
    }

    async fn add(&self, user_id: &str, amount: i64) -> Result<i64> {
        Ok(self.client.incrby(key(user_id), amount).await?)
    }

    async fn consume(&self, user_id: &str, amount: u64) -> Result<bool> {
        // this is so very atomic wohoo
        const SCRIPT: &str = r#"
            local user_id = KEYS[1]
            local amount = tonumber(ARGV[1])
            local current = tonumber(redis.call("GET", user_id))
            if not current or not amount or current < amount then
                return
            end
            redis.call("DECRBY", user_id, amount)
            return true
        "#;

        Ok(self
            .client
            .eval::<bool>(CallBuilder::script(SCRIPT).keys(key(user_id)).args(amount))
            .await?)
    }

    async fn transfer(&self, from_user_id: &str, to_user_id: &str, amount: i64) -> Result<bool> {
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
                    .args(amount),
            )
            .await?)
    }
}
