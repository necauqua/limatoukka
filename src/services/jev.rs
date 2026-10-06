//! Client for the TypeSafe evaluation API (the Jev model).
//!
//! See <https://docs.typesafe.ai/api.md>.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::time::sleep;

use crate::{config::Jev, injector_getter, services::Service};

const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

/// The number of retries on 429 (rate limit) and 529 (overloaded) replies.
const MAX_RETRIES: u32 = 3;
const FIRST_BACKOFF: Duration = Duration::from_millis(500);

/// A typed question. The `instructions` and the criteria can be a string,
/// an object or an array.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// A yes/no question; the answer is the probability of yes.
    Noul {
        instructions: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// Pick one of the options; the keys are the options, the values are
    /// their descriptions (or null).
    Choice {
        instructions: Value,
        criteria: IndexMap<String, Value>,
    },
    /// Rate along an ordered list of 2-10 levels.
    Score {
        instructions: Value,
        criteria: Vec<Value>,
    },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NoulCriteria {
    /// What a yes means
    #[serde(rename = "true", skip_serializing_if = "Option::is_none")]
    pub yes: Option<Value>,
    /// What a no means
    #[serde(rename = "false", skip_serializing_if = "Option::is_none")]
    pub no: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul {
        /// From 0 (no) to 1 (yes)
        noul: f64,
    },
    Choice {
        /// The option with the highest probability
        choice: String,
        probabilities: IndexMap<String, f64>,
        confidence: f64,
    },
    Score {
        /// The probability-weighted level, can be between levels
        score: f64,
        legend: IndexMap<String, String>,
        probabilities: IndexMap<String, f64>,
        confidence: f64,
    },
}

#[async_trait]
pub trait JevService: Service {
    /// Evaluate the `state` against the `questions`; the answers have the
    /// same keys as the questions.
    async fn evaluate(
        &self,
        state: Value,
        questions: IndexMap<String, Question>,
    ) -> Result<IndexMap<String, Answer>>;
}

injector_getter!(JevService::jev);

impl dyn JevService {
    /// Evaluate the `state` against one question.
    pub async fn ask(&self, state: Value, question: Question) -> Result<Answer> {
        let mut questions = IndexMap::new();
        questions.insert("q".to_owned(), question);
        self.evaluate(state, questions)
            .await?
            .swap_remove("q")
            .context("no answer in the reply")
    }
}

pub struct JevServiceTypeSafe {
    client: reqwest::Client,
    api_key: String,
    model: String,
}

impl JevServiceTypeSafe {
    pub fn new(config: &Jev) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
        }
    }
}

#[derive(Serialize)]
struct Request<'a> {
    state: &'a Value,
    model: &'a str,
    questions: &'a IndexMap<String, Question>,
}

#[derive(Deserialize)]
struct Response {
    model: String,
    answers: IndexMap<String, Answer>,
    usage: Usage,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}

#[async_trait]
impl JevService for JevServiceTypeSafe {
    #[tracing::instrument(skip_all)]
    async fn evaluate(
        &self,
        state: Value,
        questions: IndexMap<String, Question>,
    ) -> Result<IndexMap<String, Answer>> {
        let request = Request {
            state: &state,
            model: &self.model,
            questions: &questions,
        };

        let mut backoff = FIRST_BACKOFF;
        let mut retries = 0;
        let response = loop {
            let response = self
                .client
                .post(ENDPOINT)
                .bearer_auth(&self.api_key)
                .json(&request)
                .send()
                .await?;

            let status = response.status();
            if status.is_success() {
                break response;
            }
            // 429 = rate limit, 529 = overloaded
            if matches!(status.as_u16(), 429 | 529) && retries < MAX_RETRIES {
                tracing::warn!(%status, retries, "jev is busy, retrying");
                sleep(backoff).await;
                backoff *= 2;
                retries += 1;
                continue;
            }
            let body = response.text().await.unwrap_or_default();
            bail!("TypeSafe API error {status}: {body}");
        };

        let response = response.json::<Response>().await?;

        tracing::debug!(
            model = response.model,
            input_tokens = response.usage.input_tokens,
            output_tokens = response.usage.output_tokens,
            "jev evaluation"
        );

        Ok(response.answers)
    }
}
