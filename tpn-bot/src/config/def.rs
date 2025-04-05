use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;

// This file is `include!`d into the buildscript to generate the schema for the configuration file.

#[derive(Clone, Copy, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum Env {
    #[default]
    Dev,
    Prod,
}

#[derive(Clone, Deserialize, JsonSchema)]
pub struct Bot {
    /// Bot login username
    pub login: String,
    /// Without the "oauth:" prefix
    pub token: String,
    /// The channel on which the bot operates
    pub target: String,
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

#[derive(Deserialize /*, JsonSchema*/, Default)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    /// The environment in which the bot is running
    pub env: Env,

    /// X server display to which the bot should send inputs
    pub display: Option<String>,
    /// Bot credentials
    pub bot: Option<Bot>,

    pub valkey: String,

    pub loki: Option<String>,
    pub otel: Option<String>,

    pub browser_source_bind: String,

    pub kick_votes: Voting,
    #[serde(with = "humantime_serde")]
    pub first_time_kick: Duration,

    pub restart_votes: Voting,
    pub reset_votes: Voting,

    /// Minimum number of people required to trigger the vote
    pub vote_trigger_people: usize,
    /// Minimum ratio of yes votes to total votes required to pass the vote
    pub vote_min_ratio: f32,
}
