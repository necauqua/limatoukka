use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use twitch_api::twitch_oauth2::{ClientId, ClientSecret};

// This file is `include!`d into the buildscript to generate the schema for the configuration file.

#[derive(Clone, Copy, Deserialize, JsonSchema, Default)]
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

#[derive(Deserialize /*, JsonSchema*/, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct Voting {
    /// The amount of time to wait for enough people to trigger the vote
    #[serde(with = "humantime_serde")]
    pub trigger_interval: Duration,
    /// The amount of time for the vote itself
    #[serde(with = "humantime_serde")]
    pub vote_time: Duration,
}

#[derive(Deserialize /*, JsonSchema*/, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct Elastic {
    /// The URL of the Elastic instance
    pub url: String,
    /// The API key for the Elastic instance
    pub api_key: String,
}

#[derive(Deserialize /*, JsonSchema*/, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct Otel {
    /// The URL of the OTEL-compatible server
    pub url: String,
    /// Optional value for the Authorization header
    pub auth_header: Option<String>,
}

#[derive(Deserialize /*, JsonSchema*/)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    /// The environment in which the bot is running
    pub env: Env,

    /// X server display to which the bot should send inputs
    pub display: Option<String>,
    /// Twitch app credentials
    pub twitch: Twitch,

    pub valkey: String,
    pub elastic: Elastic,
    pub otel: Option<Otel>,

    pub browser_source_bind: String,

    pub kick_votes: Voting,

    pub restart_votes: Voting,
    pub reset_votes: Voting,

    /// Minimum number of people required to trigger the vote
    pub vote_trigger_people: usize,
    /// Minimum ratio of yes votes to total votes required to pass the vote
    pub vote_min_ratio: f32,
}
