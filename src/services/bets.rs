use anyhow::Result;
use async_trait::async_trait;
use rustis::{
    client::{BatchPreparedCommand, Client as ValkeyClient},
    commands::{GenericCommands, HashCommands},
};
use serde::{Deserialize, Serialize};

use crate::injector_getter;

#[derive(Serialize, Deserialize)]
pub struct Bet {
    pub option: String,
    pub amount: u64,
}

#[async_trait]
pub trait BetsService: Send + Sync {
    async fn place_bet(&self, bet_id: &str, user_id: &str, bet: &Bet) -> Result<u64>;

    async fn remove_bet(&self, bet_id: &str, user_id: &str) -> Result<(Option<Bet>, u64)>;

    async fn bet_count(&self, bet_id: &str) -> Result<Option<u64>>;

    async fn finalize(&self, bet_id: &str) -> Result<Vec<(String, Bet)>>;
}

injector_getter!(BetsService::bets);

pub struct BetsServiceRedis {
    client: ValkeyClient,
}

impl BetsServiceRedis {
    pub fn new(client: ValkeyClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl BetsService for BetsServiceRedis {
    async fn place_bet(&self, bet_id: &str, user_id: &str, bet: &Bet) -> Result<u64> {
        let key = format!("bet:{bet_id}");

        let mut t = self.client.create_transaction();
        t.hset(&key, (user_id, serde_json::to_string(bet)?))
            .forget();
        t.hlen(key).queue();

        Ok(t.execute().await?)
    }

    async fn remove_bet(&self, bet_id: &str, user_id: &str) -> Result<(Option<Bet>, u64)> {
        let key = format!("bet:{bet_id}");

        let mut t = self.client.create_transaction();
        t.hget::<_, _, Option<String>>(&key, user_id).queue();
        t.hdel(&key, user_id).forget();
        t.hlen(key).queue();

        let (bet, len): (Option<String>, _) = t.execute().await?;
        let bet: Option<Bet> = bet.map(|b| serde_json::from_str(&b)).transpose()?;
        Ok((bet, len))
    }

    async fn bet_count(&self, bet_id: &str) -> Result<Option<u64>> {
        let key = format!("bet:{bet_id}");

        let mut t = self.client.create_transaction();
        t.exists(&key).queue();
        t.hlen(key).queue();

        let (exists, len): (u64, _) = t.execute().await?;
        match exists {
            0 => Ok(None),
            _ => Ok(Some(len)),
        }
    }

    async fn finalize(&self, bet_id: &str) -> Result<Vec<(String, Bet)>> {
        let key = format!("bet:{bet_id}");

        let mut t = self.client.create_transaction();
        t.hgetall::<_, _, _, Vec<(String, String)>>(&key).queue();
        t.del(&key).forget();
        let bets: Vec<(String, String)> = t.execute().await?;

        let bets = bets
            .into_iter()
            .try_fold(Vec::new(), |mut acc, (user_id, bet_json)| {
                acc.push((user_id, serde_json::from_str(&bet_json)?));
                anyhow::Ok(acc)
            })?;

        Ok(bets)
    }
}
