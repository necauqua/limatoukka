use std::{borrow::Cow, collections::HashMap};

use anyhow::Result;
use async_trait::async_trait;
use elasticsearch::{Elasticsearch, auth::Credentials, http::transport::Transport};
use serde::Deserialize;

use crate::{injector_getter, services::Service};

#[async_trait]
pub trait StatsService: Service {
    async fn record(
        &self,
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
}

impl StatsServiceElastic {
    pub fn new(elastic_url: &str, elastic_api_key: &str, index: &str) -> Result<Self> {
        let transport = Transport::single_node(elastic_url).unwrap();
        transport.set_auth(Credentials::EncodedApiKey(elastic_api_key.to_owned()));
        Ok(Self {
            client: Elasticsearch::new(transport),
            index: index.to_owned(),
        })
    }
}

impl StatsServiceElastic {
    async fn count_impl(&self, clauses: Vec<(Cow<'_, str>, &str)>) -> Result<u64> {
        let mut terms = vec![];
        for (k, v) in clauses {
            terms.push(serde_json::json!({ "term": { k: v } }));
        }
        let query = serde_json::json!({
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
    async fn record(
        &self,
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

        let body = serde_json::json!({
            "@timestamp": timestamp,
            "uid": user_id,
            "name": name,
            "event": event,
            "data": data_map,
        });
        self.client
            .index(elasticsearch::IndexParts::Index(&self.index))
            .body(body)
            .send()
            .await?;
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
    async fn record(
        &self,
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
