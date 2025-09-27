use std::{
    borrow::Cow,
    collections::HashMap,
    mem,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
use async_trait::async_trait;
use elasticsearch::{
    Elasticsearch,
    auth::Credentials,
    http::{request::JsonBody, transport::Transport},
};
use serde::Deserialize;
use serde_json::json;

use crate::{injector_getter, services::Service};

#[async_trait]
pub trait StatsService: Service {
    fn record(
        self: Arc<Self>,
        user_id: &str,
        name: Option<&str>,
        event: &str,
        data: &[(&str, &str)],
    ) -> Result<()>;

    async fn count(&self, user_id: &str, event: &str, data: &[(&str, &str)]) -> Result<u64>;

    async fn total_count(&self, event: &str, data: &[(&str, &str)]) -> Result<u64>;
}

injector_getter!(StatsService::stats);

pub struct StatsServiceElastic {
    client: Elasticsearch,
    index: String,
    buffer: Mutex<Vec<serde_json::Value>>,
    batch_period: Duration,
}

impl StatsServiceElastic {
    pub fn new(elastic_url: &str, elastic_api_key: &str, index: &str) -> Result<Self> {
        let transport = Transport::single_node(elastic_url).unwrap();
        transport.set_auth(Credentials::EncodedApiKey(elastic_api_key.to_owned()));
        Ok(Self {
            client: Elasticsearch::new(transport),
            index: index.to_owned(),
            buffer: Default::default(),
            batch_period: Duration::from_secs(1),
        })
    }
}

impl StatsServiceElastic {
    async fn count_impl(&self, clauses: Vec<(Cow<'_, str>, &str)>) -> Result<u64> {
        let mut terms = vec![];
        for (k, v) in clauses {
            terms.push(json!({ "term": { k: v } }));
        }
        let query = json!({
            "query": { "bool": { "must": terms } }
        });

        #[derive(Deserialize)]
        struct CountResponse {
            count: u64,
        }

        let response = self
            .client
            .count(elasticsearch::CountParts::Index(&[&self.index]))
            .body(query)
            .send()
            .await?;

        let CountResponse { count } = response.json().await?;

        Ok(count)
    }
}

#[async_trait]
impl StatsService for StatsServiceElastic {
    fn record(
        self: Arc<Self>,
        user_id: &str,
        name: Option<&str>,
        event: &str,
        data: &[(&str, &str)],
    ) -> Result<()> {
        let timestamp = std::time::UNIX_EPOCH.elapsed().unwrap().as_millis();

        let mut data_map: HashMap<_, Vec<_>> = HashMap::new();
        for (k, v) in data {
            data_map.entry(*k).or_default().push(*v);
        }

        let document = json!({
            "@timestamp": timestamp,
            "uid": user_id,
            "name": name,
            "event": event,
            "data": data_map,
        });

        {
            let mut buffer = self.buffer.lock().unwrap();
            if buffer.is_empty() {
                tracing::trace!("new stat buffer");
            }
            buffer.push(document);
            // meh
            if buffer.len() > 1 {
                return Ok(());
            }
        }

        let s = self.clone();

        tokio::spawn(async move {
            tokio::time::sleep(s.batch_period).await;

            let documents = mem::take(&mut *s.buffer.lock().unwrap());

            let count = documents.len();

            let res = s
                .client
                .bulk(elasticsearch::BulkParts::Index(&s.index))
                .body(
                    documents
                        .into_iter()
                        .flat_map(|doc| {
                            [JsonBody::new(json!({ "create": {} })), JsonBody::new(doc)]
                        })
                        .collect(),
                )
                .send()
                .await;

            match res {
                Ok(_) => tracing::debug!(count, "recorded stats"),
                Err(e) => tracing::error!("failed to record stats: {e}"),
            }
        });

        Ok(())
    }

    async fn count(&self, user_id: &str, event: &str, data: &[(&str, &str)]) -> Result<u64> {
        let mut terms = vec![("uid".into(), user_id), ("event".into(), event)];
        for (k, v) in data {
            terms.push((format!("data.{k}").into(), v));
        }
        self.count_impl(terms).await
    }

    async fn total_count(&self, event: &str, data: &[(&str, &str)]) -> Result<u64> {
        let mut terms = vec![("event".into(), event)];
        for (k, v) in data {
            terms.push((format!("data.{k}").into(), v));
        }
        self.count_impl(terms).await
    }
}

pub struct StatsServiceNoop;

#[async_trait]
impl StatsService for StatsServiceNoop {
    fn record(
        self: Arc<Self>,
        user_id: &str,
        name: Option<&str>,
        event: &str,
        data: &[(&str, &str)],
    ) -> Result<()> {
        tracing::info!(user_id, ?name, event, ?data, "record");
        Ok(())
    }

    async fn count(&self, user_id: &str, event: &str, data: &[(&str, &str)]) -> Result<u64> {
        tracing::info!(user_id, event, ?data, "count");
        Ok(0)
    }

    async fn total_count(&self, event: &str, data: &[(&str, &str)]) -> Result<u64> {
        tracing::info!(event, ?data, "total_count");
        Ok(0)
    }
}
