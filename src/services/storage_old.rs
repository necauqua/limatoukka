use std::ops::{Deref, DerefMut};

use anyhow::Result;
use dashmap::DashMap;
use rustis::{
    client::{BatchPreparedCommand, Client, Transaction},
    commands::HashCommands,
};
use serde::de::DeserializeOwned;

use crate::commands::args::Chatter;

#[derive(Clone)]
pub struct Storage {
    client: Client,
}

impl Storage {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
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

impl Storage {
    pub async fn read_vars(&self, owner: &Chatter) -> Result<DashMap<String, String>> {
        let mut pp = self.create_pipeline();

        type Pairs = Vec<(String, String)>;

        pp.hgetall::<_, _, _, Pairs>("vars:global").queue();
        pp.hgetall::<_, _, _, Pairs>(format!("vars:{owner}"))
            .queue();

        let (globals, vars): (Pairs, Pairs) = pp.execute().await?;

        Ok(globals.into_iter().chain(vars.into_iter()).collect())
    }
}
