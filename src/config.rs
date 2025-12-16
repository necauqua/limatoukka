use std::{collections::HashMap, fmt::Display};

use anyhow::Result;
use config::File;
use serde::Deserialize;
use twitch_api::twitch_oauth2::{ClientId, ClientSecret};

#[derive(Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Env {
    #[default]
    Dev,
    Prod,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Twitch {
    pub client_id: ClientId,
    pub client_secret: ClientSecret,
    pub redirect_url: String,
    pub target_channel: String,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct Elastic {
    /// The URL of the Elastic instance
    pub url: String,
    /// The API key for the Elastic instance
    pub api_key: String,
    /// The index to use for chat logs
    pub index: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct YouTube {
    /// The API key for the YouTube Data API v3
    pub api_key: String,
    /// The country code to check for region restrictions
    pub country_code: String,
    /// The ID of the default playlist to use when no songs are requested
    pub playlist: String,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct Ntfy {
    /// The host of the ntfy instance
    pub host: String,
    /// The per-topic configuration
    pub topic: HashMap<String, NtfyTopicConfig>,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "kebab-case")]
pub struct NtfyTopicConfig {
    pub auth: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    /// The environment in which the bot is running
    pub env: Env,

    /// Twitch app credentials
    pub twitch: Twitch,

    pub valkey: String,
    pub elastic: Elastic,
    pub stats: Elastic,

    pub youtube: YouTube,
    pub ntfy: Ntfy,

    pub browser_source_bind: String,
    pub music_player_bind: String,
}

impl Display for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Env::Dev => write!(f, "dev"),
            Env::Prod => write!(f, "prod"),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let cfg = config::Config::builder()
            .add_source(File::with_name("conf/config"))
            .add_source(File::with_name("conf/config.private").required(false))
            .build()?
            .try_deserialize()?;
        Ok(cfg)
    }
}
