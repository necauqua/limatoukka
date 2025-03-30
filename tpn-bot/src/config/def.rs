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

#[derive(Deserialize, JsonSchema, Default)]
pub struct Voting {
    /// Minimum time between being able to try starting a vote for a person
    pub trigger_gate_secs: u64,
    /// The amount of time to wait for enough people to trigger a vote
    pub trigger_interval_secs: u64,
    /// Minimum number of people required to trigger a vote
    pub trigger_min_people: u32,
    /// Minimum ratio of yes votes to total votes required to pass a vote
    pub vote_min_ratio: f32,
    /// The amount of time for a kick vote
    pub kick_vote_time: u64,
    /// The amount of time for a restart vote
    pub restart_vote_time: u64,
    /// The amount of time for a reset vote
    pub reset_vote_time: u64,
}

#[derive(Deserialize, JsonSchema, Default)]
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

    pub voting: Voting,
    pub first_time_kick_secs: u64,
}
