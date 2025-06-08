use std::ops::{Deref, DerefMut};

use anyhow::Result;
use rustis::client::{Client, Transaction};
use serde::de::DeserializeOwned;

use crate::config::Config;

pub struct Storage {
    client: Client,
}

impl Storage {
    pub async fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            client: Client::connect(&*config.valkey).await?,
        })
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

#[macro_export]
macro_rules! storage {
    ($ctx:expr, $call:ident, $($args:tt),*) => {
        {
            #[allow(unused_imports)]
            use rustis::commands::{GenericCommands as _, StringCommands as _};
            $ctx.storage().$call($(storage!(_ $args)),*).await
        }
    };
    (_ $key:literal) => {
        format!($key)
    };
    (_ $other:tt) => {
        #[allow(unused_braces)]
        $other
    }
}
