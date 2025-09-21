use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use elasticsearch::{
    CountParts, Elasticsearch, SearchParts, auth::Credentials, http::transport::Transport,
};
use serde_json::{Value, json};

use crate::injector_getter;

#[derive(Debug, Clone)]
pub enum Edge<'s> {
    First,
    Last { exclude_message_id: Option<&'s str> },
}

impl Edge<'_> {
    pub fn sort(&self) -> &str {
        match self {
            Edge::First => "asc",
            Edge::Last { .. } => "desc",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EdgeMessage {
    pub message: String,
    pub true_first: bool,
}

#[derive(Debug, Clone)]
pub enum Rank {
    Top1,
    Top1k { pos: usize, to_climb: i64 },
    Bottom,
}

#[async_trait]
pub trait ChatLogService: Send + Sync {
    async fn stat(&self, word: Option<&str>, user_id: Option<&str>) -> Result<i64>;
    async fn edge(&self, user_id: &str, edge: Edge<'_>) -> Result<Option<EdgeMessage>>;
    async fn top_n(&self, n: u64, exclude: &[&str]) -> Result<Vec<(String, i64)>>;
    async fn rank(&self, user_id: &str, exclude: &[&str]) -> Result<Rank>;
}

injector_getter!(ChatLogService::chat_logs);

pub struct ChatLogServiceElastic {
    client: Elasticsearch,
    index: String,
}

impl ChatLogServiceElastic {
    pub fn new(elastic_url: &str, elastic_api_key: &str, index: &str) -> Result<Self> {
        let transport = Transport::single_node(elastic_url)?;
        transport.set_auth(Credentials::EncodedApiKey(elastic_api_key.to_owned()));
        Ok(Self {
            client: Elasticsearch::new(transport),
            index: index.to_owned(),
        })
    }
}

#[async_trait]
impl ChatLogService for ChatLogServiceElastic {
    async fn stat(&self, word: Option<&str>, user_id: Option<&str>) -> Result<i64> {
        let mut must = vec![json!({ "term": { "irc.cmd": "PRIVMSG" } })];

        if let Some(user_id) = user_id {
            must.push(json!({ "term": { "tags.user-id": user_id } }));
        }
        if let Some(word) = word {
            must.push(json!({ "match": { "message": word } }));
        }

        #[derive(serde::Deserialize)]
        struct CountResponse {
            count: i64,
        }

        let response = self
            .client
            .count(CountParts::Index(&[&self.index]))
            .body(json!({ "query": { "bool": { "must": must } } }))
            .send()
            .await?
            .error_for_status_code()?
            .json::<CountResponse>()
            .await?
            .count;

        Ok(response)
    }

    #[tracing::instrument(skip(self))]
    async fn edge(&self, user_id: &str, edge: Edge<'_>) -> Result<Option<EdgeMessage>> {
        let mut bool = serde_json::Map::new();
        bool.insert(
            "must".into(),
            json!([
                { "term": { "irc.cmd": "PRIVMSG" } },
                { "term": { "tags.user-id": user_id } },
                // IF there's a source-room-id tag, check that it matches the room-id aka actually sent in our chat
                {
                    "bool": {
                        "should": [
                            { "bool": { "must_not": { "exists": { "field": "tags.source-room-id" } } } },
                            {
                                "script": {
                                    "script": {
                                        "source": r#"
                                            if (doc['tags.room-id'].size() > 0 && doc['tags.source-room-id'].size() > 0) {
                                                return doc['tags.room-id'].value == doc['tags.source-room-id'].value;
                                            }
                                            return false;
                                        "#,
                                    },
                                },
                            },
                        ],
                    },
                }
            ]),
        );

        if let Edge::Last {
            exclude_message_id: Some(id),
        } = &edge
        {
            bool.insert(
                "must_not".into(),
                json!([{
                    "term": { "_id": id }
                }]),
            );
        }

        let response = self
            .client
            .search(SearchParts::Index(&[&self.index]))
            .body(json!({
                "query": { "bool": bool },
                "sort": [{ "@timestamp": edge.sort() }],
                "size": 1,
            }))
            .send()
            .await?;
        let response = response.error_for_status_code()?.json::<Value>().await?;

        let Some(found) = response.pointer("/hits/hits/0/_source") else {
            return Ok(None);
        };
        let message = found
            .get("message")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned())
            .unwrap_or_default();

        let true_first = found.pointer("/tags/first-msg") == Some(&json!(1));

        Ok(Some(EdgeMessage {
            message,
            true_first,
        }))
    }

    #[tracing::instrument(skip(self))]
    async fn top_n(&self, n: u64, exclude: &[&str]) -> Result<Vec<(String, i64)>> {
        let response = self
            .client
            .search(SearchParts::Index(&[&self.index]))
            .body(json!({
                "size": 0,
                "query": {
                    "bool": { "must_not": { "terms": { "tags.user-id": exclude } } },
                },
                "aggs": {
                    "top": {
                        "terms": { "field": "tags.user-id", "size": n },
                        "aggs": {
                            "name": {
                                "top_hits": { "size": 1, "_source": ["name"] }
                            }
                        }
                    }
                }
            }))
            .send()
            .await?
            .error_for_status_code()?
            .json::<Value>()
            .await?;

        let buckets = response
            .pointer("/aggregations/top/buckets")
            .and_then(|v| v.as_array())
            .context("malformed aggregation reply")?;

        let results = buckets
            .iter()
            .filter_map(|b| {
                b.pointer("/name/hits/hits/0/_source/name")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_owned())
                    .zip(b.get("doc_count").and_then(|c| c.as_i64()))
            })
            .collect::<Vec<_>>();

        if results.is_empty() {
            bail!("malformed aggregation reply");
        }

        Ok(results)
    }

    #[tracing::instrument(skip(self))]
    async fn rank(&self, user_id: &str, exclude: &[&str]) -> Result<Rank> {
        let response = self
            .client
            .search(SearchParts::Index(&[&self.index]))
            .body(json!({
                "size": 0,
                "query": {
                    "bool": { "must_not": { "terms": { "tags.user-id": exclude } } },
                },
                "aggs": {
                    "top": {
                        "terms": { "field": "tags.user-id", "size": 999 },
                    }
                }
            }))
            .send()
            .await?
            .error_for_status_code()?
            .json::<Value>()
            .await?;

        let buckets = response
            .pointer("/aggregations/top/buckets")
            .and_then(|v| v.as_array())
            .context("malformed aggregation reply")?;

        let mut prev = None;
        let mut found = None;
        for (i, bucket) in buckets.iter().enumerate() {
            let count = bucket
                .get("doc_count")
                .and_then(|c| c.as_i64())
                .context("malformed aggregation reply")?;
            if bucket
                .get("key")
                .and_then(|k| k.as_str())
                .context("malformed aggregation reply")?
                == user_id
            {
                found = Some((i + 1, prev.map(|p| p - count)));
                break;
            }
            prev = Some(count)
        }
        Ok(match found {
            Some((_, None)) => Rank::Top1, // pos is always 1 here
            Some((pos, Some(to_climb))) => Rank::Top1k { pos, to_climb },
            None => Rank::Bottom,
        })
    }
}
