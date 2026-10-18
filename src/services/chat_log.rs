use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use elasticsearch::{
    CountParts, Elasticsearch, SearchParts, auth::Credentials, http::transport::Transport,
};
use serde_json::{Value, json};
use twitch_irc::message::{IRCMessage, IRCPrefix, IRCTags};

use crate::{injector_getter, services::Service};

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

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatLine {
    /// The display name of the sender
    pub name: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum Rank {
    Top1,
    Top1k { pos: usize, to_climb: i64 },
    Bottom,
}

#[async_trait]
pub trait ChatLogService: Service {
    async fn stat(&self, word: Option<&str>, user_id: Option<&str>) -> Result<i64>;

    async fn edge(&self, user_id: &str, edge: Edge<'_>) -> Result<Option<EdgeMessage>>;

    async fn top_n(&self, n: u64, exclude: &[&str]) -> Result<Vec<(String, i64)>>;

    async fn rank(&self, user_id: &str, exclude: &[&str]) -> Result<Rank>;

    /// The last `limit` chat messages of the last `max_age`, oldest first,
    /// without the given message and the messages of the given users.
    async fn recent(
        &self,
        limit: u64,
        max_age: Duration,
        exclude_message_id: Option<&str>,
        exclude_users: &[&str],
    ) -> Result<Vec<ChatLine>>;

    /// The last `limit` `PRIVMSG`, `USERNOTICE`, `CLEARMSG` and `CLEARCHAT`
    /// messages of a channel after `after_ms` (Unix time in milliseconds),
    /// oldest first, rebuilt from the log.
    async fn recent_irc(
        &self,
        channel: &str,
        after_ms: Option<i64>,
        limit: u64,
    ) -> Result<Vec<IRCMessage>>;
}

injector_getter!(ChatLogService::chat_logs);

/// A tag value as in IRC. The archiver stores numbers as numbers, and
/// `badges`/`badge-info` as objects.
fn tag_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => (if *b { "1" } else { "0" }).to_owned(),
        Value::Number(n) => n.to_string(),
        Value::Object(badges) => badges
            .iter()
            .filter(|(name, _)| !name.is_empty())
            .map(|(name, version)| format!("{name}/{}", tag_value(version)))
            .collect::<Vec<_>>()
            .join(","),
        Value::Array(_) => value.to_string(),
    }
}

/// Rebuilds an IRC message from a document of the archiver.
pub(crate) fn irc_from_doc(id: &str, doc: &Value) -> Option<IRCMessage> {
    let command = doc.get("irc.cmd")?.as_str()?;
    let channel = doc.get("channel")?.as_str()?;

    let mut tags = IRCTags::new();
    if let Some(stored) = doc.get("tags").and_then(|t| t.as_object()) {
        for (key, value) in stored {
            tags.0.insert(key.clone(), tag_value(value));
        }
    }
    tags.0.insert("id".into(), id.to_owned());
    if let Some(ts) = doc.get("@timestamp").and_then(|t| t.as_i64()) {
        tags.0.insert("tmi-sent-ts".into(), ts.to_string());
    }

    let name = doc.get("name").and_then(|n| n.as_str());
    if matches!(command, "PRIVMSG" | "USERNOTICE") {
        if let Some(name) = name {
            tags.0
                .entry("display-name".into())
                .or_insert_with(|| name.to_owned());
        }
        // the archiver leaves out the empty tags
        for tag in ["badges", "badge-info", "emotes", "color", "system-msg"] {
            tags.0.entry(tag.into()).or_default();
        }
    }

    let prefix = if command == "PRIVMSG" {
        let nick = doc.get("irc.nick")?.as_str()?.to_owned();
        IRCPrefix::Full {
            user: Some(nick.clone()),
            host: Some(format!("{nick}.tmi.twitch.tv")),
            nick,
        }
    } else {
        IRCPrefix::HostOnly {
            host: "tmi.twitch.tv".into(),
        }
    };

    let mut params = vec![format!("#{channel}")];
    // the archiver stores the system message of a notice without a message
    // as the message
    let system = tags.0.get("system-msg").map(String::as_str);
    let text = doc
        .get("message")
        .and_then(|m| m.as_str())
        .filter(|text| command != "USERNOTICE" || Some(*text) != system);
    if let Some(text) = text {
        if doc.get("action").and_then(|a| a.as_bool()) == Some(true) {
            params.push(format!("\u{1}ACTION {text}\u{1}"));
        } else {
            params.push(text.to_owned());
        }
    }
    if let Some(extras) = doc.get("irc.extras").and_then(|e| e.as_array()) {
        params.extend(extras.iter().filter_map(|e| Some(e.as_str()?.to_owned())));
    }

    Some(IRCMessage::new(
        tags,
        Some(prefix),
        command.to_owned(),
        params,
    ))
}

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

    #[tracing::instrument(skip(self))]
    async fn recent(
        &self,
        limit: u64,
        max_age: Duration,
        exclude_message_id: Option<&str>,
        exclude_users: &[&str],
    ) -> Result<Vec<ChatLine>> {
        let mut must_not = vec![json!({ "terms": { "tags.user-id": exclude_users } })];
        if let Some(id) = exclude_message_id {
            must_not.push(json!({ "term": { "_id": id } }));
        }

        let response = self
            .client
            .search(SearchParts::Index(&[&self.index]))
            .body(json!({
                "query": {
                    "bool": {
                        "must": [
                            { "term": { "irc.cmd": "PRIVMSG" } },
                            { "range": { "@timestamp": { "gte": format!("now-{}s", max_age.as_secs()) } } },
                        ],
                        "must_not": must_not,
                    },
                },
                "_source": ["name", "message"],
                "sort": [{ "@timestamp": "desc" }],
                "size": limit,
            }))
            .send()
            .await?
            .error_for_status_code()?
            .json::<Value>()
            .await?;

        let hits = response
            .pointer("/hits/hits")
            .and_then(|v| v.as_array())
            .context("malformed search reply")?;

        let mut lines = hits
            .iter()
            .filter_map(|hit| {
                let source = hit.get("_source")?;
                Some(ChatLine {
                    name: source.get("name")?.as_str()?.to_owned(),
                    message: source.get("message")?.as_str()?.to_owned(),
                })
            })
            .collect::<Vec<_>>();
        lines.reverse();

        Ok(lines)
    }

    #[tracing::instrument(skip(self))]
    async fn recent_irc(
        &self,
        channel: &str,
        after_ms: Option<i64>,
        limit: u64,
    ) -> Result<Vec<IRCMessage>> {
        let mut must = vec![
            json!({ "term": { "channel": channel } }),
            json!({ "terms": { "irc.cmd": ["PRIVMSG", "USERNOTICE", "CLEARMSG", "CLEARCHAT"] } }),
        ];
        if let Some(after) = after_ms {
            must.push(json!({
                "range": { "@timestamp": { "gt": after, "format": "epoch_millis" } }
            }));
        }

        let response = self
            .client
            .search(SearchParts::Index(&[&self.index]))
            .body(json!({
                "query": { "bool": { "must": must } },
                "sort": [{ "@timestamp": "desc" }],
                "size": limit,
            }))
            .send()
            .await?
            .error_for_status_code()?
            .json::<Value>()
            .await?;

        let hits = response
            .pointer("/hits/hits")
            .and_then(|v| v.as_array())
            .context("malformed search reply")?;

        let mut messages = hits
            .iter()
            .filter_map(|hit| irc_from_doc(hit.get("_id")?.as_str()?, hit.get("_source")?))
            .collect::<Vec<_>>();
        messages.reverse();

        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use twitch_irc::message::ServerMessage;

    use super::*;

    fn parse(id: &str, doc: Value) -> ServerMessage {
        ServerMessage::try_from(irc_from_doc(id, &doc).unwrap()).unwrap()
    }

    #[test]
    fn archived_privmsg() {
        let msg = parse(
            "6e5c80b3",
            json!({
                "@timestamp": 1792262400028i64,
                "channel": "necauqua",
                "name": "dargfox",
                "message": "Get well struck20Petpet necauqLove",
                "tags": {
                    "room-id": "160410682",
                    "color": "#DAA520",
                    "emotes": "emotesv2_fa:9-22/emotesv2_29:24-33",
                    "badges": { "moderator": 1, "subscriber": 9, "artist-badge": 1 },
                    "user-id": "264630271",
                    "mod": 1,
                },
                "irc.nick": "dargfox",
                "irc.cmd": "PRIVMSG",
            }),
        );
        let ServerMessage::Privmsg(msg) = msg else {
            panic!("not a PRIVMSG: {msg:?}");
        };
        assert_eq!(msg.message_id, "6e5c80b3");
        assert_eq!(msg.sender.login, "dargfox");
        assert_eq!(msg.sender.name, "dargfox");
        assert_eq!(msg.name_color.unwrap().to_string(), "#DAA520");
        let badges = msg
            .badges
            .iter()
            .map(|b| format!("{}/{}", b.name, b.version));
        assert_eq!(
            badges.collect::<Vec<_>>(),
            ["moderator/1", "subscriber/9", "artist-badge/1"]
        );
        assert_eq!(msg.emotes.len(), 2);
        assert_eq!(msg.emotes[0].code, "struck20Petpet");
        assert_eq!(msg.server_timestamp.timestamp_millis(), 1792262400028);
    }

    #[test]
    fn archived_action_without_optional_tags() {
        let msg = parse(
            "f430b093",
            json!({
                "@timestamp": 1791921916943i64,
                "channel": "necauqua",
                "name": "ZZStoner",
                "message": "hatmic98's first message was CUM !",
                "action": true,
                "tags": { "user-id": "263858367", "room-id": "160410682" },
                "irc.nick": "zzstoner",
                "irc.cmd": "PRIVMSG",
            }),
        );
        let ServerMessage::Privmsg(msg) = msg else {
            panic!("not a PRIVMSG: {msg:?}");
        };
        assert!(msg.is_action);
        assert_eq!(msg.message_text, "hatmic98's first message was CUM !");
        assert_eq!(msg.name_color, None);
        assert!(msg.badges.is_empty());
    }

    #[test]
    fn archived_notices() {
        let msg = parse(
            "n1",
            json!({
                "@timestamp": 1i64,
                "channel": "necauqua",
                "name": "mocro07",
                "message": "mocro07 subscribed at Tier 1. They've subscribed for 14 months!",
                "tags": {
                    "room-id": "160410682",
                    "user-id": "137847272",
                    "login": "mocro07",
                    "system-msg": "mocro07 subscribed at Tier 1. They've subscribed for 14 months!",
                    "msg-id": "resub",
                    "msg-param-months": 0,
                    "msg-param-should-share-streak": 0,
                    "msg-param-sub-plan-name": "Subscription (necauqua)",
                    "msg-param-sub-plan": 1000,
                    "msg-param-cumulative-months": 14,
                    "badges": { "founder": 0 },
                },
                "irc.nick": "tmi.twitch.tv",
                "irc.cmd": "USERNOTICE",
            }),
        );
        let ServerMessage::UserNotice(msg) = msg else {
            panic!("not a USERNOTICE: {msg:?}");
        };
        assert_eq!(msg.message_text, None);
        assert_eq!(msg.sender.name, "mocro07");

        // an announcement: no system message, a message
        let msg = parse(
            "n2",
            json!({
                "@timestamp": 1i64,
                "channel": "necauqua",
                "name": "necauqua",
                "message": "kek",
                "tags": {
                    "room-id": "160410682",
                    "user-id": "160410682",
                    "login": "necauqua",
                    "msg-id": "announcement",
                    "msg-param-color": "PRIMARY",
                },
                "irc.nick": "tmi.twitch.tv",
                "irc.cmd": "USERNOTICE",
            }),
        );
        let ServerMessage::UserNotice(msg) = msg else {
            panic!("not a USERNOTICE: {msg:?}");
        };
        assert_eq!(msg.message_text.as_deref(), Some("kek"));
    }

    #[test]
    fn archived_clears() {
        let msg = parse(
            "8a58d374",
            json!({
                "@timestamp": 1789405366818i64,
                "channel": "necauqua",
                "name": "tmi.twitch.tv",
                "message": "delete me",
                "tags": { "target-msg-id": "16a4bed2", "login": "man_e_bodies", "room-id": "160410682" },
                "irc.nick": "tmi.twitch.tv",
                "irc.cmd": "CLEARMSG",
            }),
        );
        assert!(matches!(msg, ServerMessage::ClearMsg(m) if m.message_id == "16a4bed2"));

        let msg = parse(
            "4fb70cfc",
            json!({
                "@timestamp": 1779476155329i64,
                "channel": "necauqua",
                "name": "tmi.twitch.tv",
                "message": "flexcoral",
                "tags": { "target-user-id": "473064207", "ban-duration": 3600, "room-id": "160410682" },
                "irc.nick": "tmi.twitch.tv",
                "irc.cmd": "CLEARCHAT",
            }),
        );
        let ServerMessage::ClearChat(msg) = msg else {
            panic!("not a CLEARCHAT: {msg:?}");
        };
        assert!(matches!(
            msg.action,
            twitch_irc::message::ClearChatAction::UserTimedOut { user_id, .. } if user_id == "473064207"
        ));
    }
}
