use anyhow::Result;
use async_trait::async_trait;
use rustis::{
    client::{BatchPreparedCommand, Client as ValkeyClient},
    commands::{GenericCommands, HashCommands},
};
use serde::{Deserialize, Serialize};

use crate::injector_getter;

pub struct Win {
    pub user_id: String,
    pub amount: i64,
}

pub struct BetResult {
    pub winners: Vec<Win>,
    pub total_pool: i64,
    pub losers: usize,
}

#[async_trait]
pub trait BetsService: Send + Sync {
    async fn place_bet(
        &self,
        bet_id: &str,
        user_id: &str,
        option: &str,
        amount: i64,
    ) -> Result<usize>;

    async fn remove_bet(&self, bet_id: &str, user_id: &str) -> Result<(String, i64, usize)>;

    async fn settle(&self, bet_id: &str, winning_option: &str) -> Result<BetResult>;
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

#[derive(Serialize, Deserialize)]
struct SingleBet {
    option: String,
    amount: i64,
}

#[async_trait]
impl BetsService for BetsServiceRedis {
    async fn place_bet(
        &self,
        bet_id: &str,
        user_id: &str,
        option: &str,
        amount: i64,
    ) -> Result<usize> {
        let key = format!("bet:{bet_id}");

        let bet = serde_json::to_string(&SingleBet {
            option: option.into(),
            amount,
        })?;

        let mut t = self.client.create_transaction();
        t.hset(&key, (user_id, bet)).forget();
        t.hlen(key).queue();

        Ok(t.execute().await?)
    }

    async fn remove_bet(&self, bet_id: &str, user_id: &str) -> Result<(String, i64, usize)> {
        let key = format!("bet:{bet_id}");

        let mut t = self.client.create_transaction();
        t.hget::<_, _, String>(&key, user_id).queue();
        t.hdel(&key, user_id).forget();
        t.hlen(key).queue();

        let (bet, len): (String, usize) = t.execute().await?;

        let bet: SingleBet = serde_json::from_str(&bet)?;
        Ok((bet.option, bet.amount, len))
    }

    async fn settle(&self, bet_id: &str, winning_option: &str) -> Result<BetResult> {
        let key = format!("bet:{bet_id}");

        let mut t = self.client.create_transaction();
        t.hgetall::<_, _, _, Vec<(String, String)>>(&key).queue();
        t.del(&key).forget();

        let bets: Vec<(String, String)> = t.execute().await?;

        let mut winners = Vec::new();
        let mut total_pool = 0;
        let mut winner_pool = 0;
        let mut losers = 0;
        for (user_id, bet_json) in bets {
            let bet: SingleBet = serde_json::from_str(&bet_json)?;
            total_pool += bet.amount;
            if bet.option.eq_ignore_ascii_case(winning_option) {
                winner_pool += bet.amount;
                winners.push(Win {
                    user_id,
                    amount: bet.amount,
                });
            } else {
                losers += 1;
            }
        }

        // huh
        if winner_pool != 0 {
            for win in &mut winners {
                win.amount = (win.amount * total_pool) / winner_pool;
            }
        }

        Ok(BetResult {
            winners,
            total_pool,
            losers,
        })
    }
}
