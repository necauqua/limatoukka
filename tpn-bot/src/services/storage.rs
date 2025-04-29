use std::ops::Deref;

use anyhow::Result;
use rustis::client::{Client, IntoConfig};

pub struct Storage {
    client: Client,
}

impl Storage {
    pub async fn new(config: impl IntoConfig) -> Result<Self> {
        Ok(Self {
            client: Client::connect(config).await?,
        })
    }
}

impl Deref for Storage {
    type Target = Client;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}
