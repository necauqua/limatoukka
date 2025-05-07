use std::ops::Deref;

use anyhow::Result;
use elasticsearch::{Elasticsearch, auth::Credentials, http::transport::Transport};
use rustis::client::Client;

use crate::config::Config;

pub struct Storage {
    client: Client,
    stats: Elasticsearch,
}

impl Storage {
    pub async fn new(config: &Config) -> Result<Self> {
        let transport = Transport::single_node("https://elastic.necauq.ua")?;
        transport.set_auth(Credentials::EncodedApiKey(config.elastic.api_key.clone()));

        Ok(Self {
            client: Client::connect(&*config.valkey).await?,
            stats: Elasticsearch::new(transport),
        })
    }

    pub fn stats(&self) -> &Elasticsearch {
        &self.stats
    }
}

impl Deref for Storage {
    type Target = Client;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

#[macro_export]
macro_rules! storage {
    ($ctx:expr, $call:ident, $($args:tt),*) => {
        {
            #[allow(unused_imports)]
            use rustis::commands::{GenericCommands as _, StringCommands as _};
            $ctx.storage().$call($(storage!(_ $args)),*).await?
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
