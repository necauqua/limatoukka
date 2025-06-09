use std::{
    ops::{Deref, DerefMut},
    time::Duration,
};

use anyhow::{Result, anyhow};
use rustis::{
    client::{BatchPreparedCommand, Client, Transaction},
    commands::{ExpireOption, GenericCommands, StringCommands},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::config::Config;

pub struct Storage {
    client: Client,
}

impl Deref for Storage {
    type Target = Client;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

pub struct StorageRef<'a> {
    storage: &'a Storage,
    _span: tracing::Span,
}

impl<'a> StorageRef<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self {
            storage,
            _span: tracing::debug_span!("storage", otel.name = "redis call"),
        }
    }

    pub fn create_transaction(&self) -> TransactionRef {
        TransactionRef {
            transaction: self.storage.create_transaction(),
            _span: self._span.clone(),
        }
    }
}

impl Deref for StorageRef<'_> {
    type Target = Storage;

    fn deref(&self) -> &Self::Target {
        self.storage
    }
}

pub struct TransactionRef {
    transaction: Transaction,
    _span: tracing::Span,
}

impl TransactionRef {
    pub async fn execute<T: DeserializeOwned>(self) -> rustis::Result<T> {
        self.transaction.execute().await
    }
}

impl Deref for TransactionRef {
    type Target = Transaction;

    fn deref(&self) -> &Self::Target {
        &self.transaction
    }
}

impl DerefMut for TransactionRef {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.transaction
    }
}

#[derive(Debug, Clone, Copy)]
pub enum BalanceMessage {
    Bless,
    Curse,
    Reset,
}

impl Storage {
    pub async fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            client: Client::connect(&*config.valkey).await?,
        })
    }
}

impl Storage {
    pub async fn modify_balance(&self, msg: BalanceMessage) -> Result<()> {
        match msg {
            BalanceMessage::Bless => {
                self.client.incr("balance:blesses").await?;
            }
            BalanceMessage::Curse => {
                self.client.incr("balance:curses").await?;
            }
            BalanceMessage::Reset => {
                self.client
                    .del(["balance:blesses", "balance:curses"])
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn get_balance(&self) -> Result<(i64, i64)> {
        let [blesses, curses]: [i64; 2] = self
            .client
            .mget(["balance:blesses", "balance:curses"])
            .await?;
        Ok((blesses, curses))
    }
}

pub struct Hold<'s> {
    storage: &'s Storage,
    key: String,
}

impl Hold<'_> {
    pub async fn down(&self) -> Result<bool> {
        let mut tx = self.storage.client.create_transaction();
        tx.incr(&self.key).queue();

        // the unstuck logic, just in case
        tx.pexpire(&self.key, 60_000, ExpireOption::Nx).forget();

        Ok(tx.execute::<i64>().await? == 1)
    }

    pub async fn up(&self) -> Result<bool> {
        let counter = self.storage.decr(&self.key).await?;
        if counter <= 0 {
            if counter != 0 {
                // this means there was an oopsie
                self.storage.del(&self.key).await?;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

impl Storage {
    pub fn hold(&self, key: &str) -> Result<Hold> {
        Ok(Hold {
            storage: self,
            key: format!("holds:{key}"),
        })
    }
}

pub struct Cache<'s> {
    storage: &'s Storage,
    ttl: Duration,
    key: &'static str,
}

impl Cache<'_> {
    pub async fn get<T, E>(
        &self,
        key: &str,
        compute: impl AsyncFnOnce() -> Result<T, E>,
    ) -> Result<T, E>
    where
        T: Serialize + DeserializeOwned,
        E: From<anyhow::Error>,
    {
        let full_key = format!("caches:{}:{key}", self.key);
        match self
            .storage
            .client
            .get::<_, Option<String>>(&full_key)
            .await
            .map_err(|e| anyhow!(e))?
        {
            Some(value) => Ok(serde_json::from_str(&value).map_err(|e| anyhow!(e))?),
            None => Ok({
                let value = compute().await?;
                self.storage
                    .client
                    .psetex(
                        &full_key,
                        self.ttl.as_millis() as _,
                        serde_json::to_string(&value).map_err(|e| anyhow!(e))?,
                    )
                    .await
                    .map_err(|e| anyhow!(e))?;
                value
            }),
        }
    }
}

impl Storage {
    pub fn cache(&self, ttl: Duration, key: &'static str) -> Result<Cache> {
        Ok(Cache {
            storage: self,
            ttl,
            key,
        })
    }
}
