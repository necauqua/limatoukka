//! The event list browser source: the most recent follows, subs, cheers,
//! raids, donations etc. The entries are in Valkey, so that a reload of the
//! page or a restart of the bot does not clear the list.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use axum::{
    Router,
    extract::State,
    response::{Sse, sse::Event},
    routing::get,
};
use jiff::{Timestamp, tz::TimeZone};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use rustis::{
    client::{BatchPreparedCommand, Client as ValkeyClient},
    commands::ListCommands,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::{self, error::RecvError};
use twitch_api::types::SubscriptionTier;

use crate::{injector_getter, services::Service};

const VALKEY_KEY: &str = "event-list";
/// The number of entries that Valkey keeps.
const KEPT: usize = 50;
/// The number of entries on the page.
const SHOWN: usize = 3;

/// The name that is shown for anonymous gifts and cheers.
const ANONYMOUS: &str = "Anonymous";

#[async_trait]
pub trait EventListService: Service {
    async fn push(&self, event: ListedEvent) -> Result<()>;
}

injector_getter!(EventListService::event_list { EventList::in_memory() });

/// A Power-up that bits were used on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PowerUp {
    MessageEffect,
    Celebration,
    GigantifyEmote,
    /// A custom Power-up of the channel
    Custom {
        title: String,
    },
    /// A Power-up that Twitch added after this code was written
    Other {
        name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KofiKind {
    Donation,
    Sub,
    Resub,
    ShopOrder,
}

/// An event in the list. The user fields are display names; `None` is an
/// anonymous user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ListedEvent {
    Follow {
        user: String,
    },
    Sub {
        user: String,
        tier: SubscriptionTier,
    },
    Resub {
        user: String,
        months: i64,
    },
    SubGift {
        user: Option<String>,
        count: i64,
    },
    Cheer {
        user: Option<String>,
        bits: i64,
    },
    PowerUp {
        user: String,
        bits: i64,
        power_up: PowerUp,
    },
    Raid {
        user: String,
        viewers: i64,
    },
    Charity {
        user: String,
        /// Formatted, e.g. `$5` or `4.50 EUR`
        amount: String,
        charity: String,
    },
    /// Only public ones
    Kofi {
        user: String,
        /// Formatted, e.g. `$5` or `4.50 EUR`
        amount: String,
        kind: KofiKind,
    },
}

/// Formats an amount of money: `$5`, `$4.50`, `5 EUR`.
pub fn format_money(amount: &str, currency: &str) -> String {
    let amount = match amount.split_once('.') {
        Some((whole, fraction)) if fraction.bytes().all(|b| b == b'0') => whole,
        _ => amount,
    };
    match currency {
        "USD" => format!("${amount}"),
        _ => format!("{amount} {currency}"),
    }
}

fn plural(n: i64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

impl ListedEvent {
    /// The CSS class of the entry.
    fn class(&self) -> &'static str {
        match self {
            Self::Follow { .. } => "follow",
            Self::Sub { .. } => "sub",
            Self::Resub { .. } => "resub",
            Self::SubGift { .. } => "sub-gift",
            Self::Cheer { .. } => "cheer",
            Self::PowerUp { .. } => "power-up",
            Self::Raid { .. } => "raid",
            Self::Charity { .. } => "charity",
            Self::Kofi { .. } => "kofi",
        }
    }

    fn user(&self) -> &str {
        match self {
            Self::Follow { user }
            | Self::Sub { user, .. }
            | Self::Resub { user, .. }
            | Self::PowerUp { user, .. }
            | Self::Raid { user, .. }
            | Self::Charity { user, .. }
            | Self::Kofi { user, .. } => user,
            Self::SubGift { user, .. } | Self::Cheer { user, .. } => {
                user.as_deref().unwrap_or(ANONYMOUS)
            }
        }
    }

    /// The text after the user name.
    fn text(&self) -> String {
        match self {
            Self::Follow { .. } => "followed the channel".into(),
            Self::Sub { tier, .. } => match tier {
                SubscriptionTier::Tier2 => "subscribed to the channel at Tier 2!".into(),
                SubscriptionTier::Tier3 => "subscribed to the channel at Tier 3!".into(),
                SubscriptionTier::Prime => "subscribed to the channel with Prime!".into(),
                _ => "subscribed to the channel!".into(),
            },
            Self::Resub { months, .. } => {
                format!("resubscribed for {months} month{}", plural(*months))
            }
            Self::SubGift { count: 1, .. } => "gifted a sub".into(),
            Self::SubGift { count, .. } => format!("gifted {count} subs"),
            Self::Cheer { bits, .. } => format!("cheered with {bits} bit{}", plural(*bits)),
            Self::PowerUp { bits, power_up, .. } => {
                let bits = format!("{bits} bit{}", plural(*bits));
                match power_up {
                    PowerUp::MessageEffect => format!("used {bits} on a message effect"),
                    PowerUp::Celebration => format!("used {bits} on an on-screen celebration"),
                    PowerUp::GigantifyEmote => format!("used {bits} to gigantify an emote"),
                    PowerUp::Custom { title } => format!("used {bits} on {title}"),
                    PowerUp::Other { name } => format!("used {bits} on a Power-up ({name})"),
                }
            }
            Self::Raid { viewers, .. } => {
                format!(
                    "raided the stream with {viewers} viewer{}",
                    plural(*viewers)
                )
            }
            Self::Charity {
                amount, charity, ..
            } => format!("donated {amount} to {charity}"),
            Self::Kofi { amount, kind, .. } => match kind {
                KofiKind::Donation => format!("donated {amount} on Ko-fi"),
                KofiKind::Sub => format!("subscribed on Ko-fi ({amount})"),
                KofiKind::Resub => format!("continued the Ko-fi sub ({amount})"),
                KofiKind::ShopOrder => "bought some merch".into(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    /// Unix time in milliseconds
    at: i64,
    #[serde(flatten)]
    event: ListedEvent,
}

impl Entry {
    fn render(&self) -> Markup {
        let date = Timestamp::from_millisecond(self.at)
            .map(|at| {
                at.to_zoned(TimeZone::system())
                    .strftime("1%Y-%m-%d/%H:%M:%S%.3f |")
                    .to_string()
            })
            .unwrap_or_default();
        html! {
            div class={ "entry " (self.event.class()) } data-id=(self.id) {
                span.date style="display:none" { (date) }
                span.message {
                    span.user { (self.event.user()) }
                    " " (self.event.text())
                }
            }
        }
    }
}

fn render_entries(entries: &[Entry]) -> Markup {
    html! {
        @for entry in entries {
            (entry.render())
        }
    }
}

enum Store {
    Valkey(ValkeyClient),
    /// Newest first
    InMemory(Mutex<VecDeque<Entry>>),
}

pub struct EventList {
    store: Store,
    /// Notifies the open pages that the list changed.
    updates: broadcast::Sender<()>,
}

impl EventList {
    fn new(store: Store) -> Self {
        Self {
            store,
            updates: broadcast::Sender::new(16),
        }
    }

    pub fn valkey(client: ValkeyClient) -> Self {
        Self::new(Store::Valkey(client))
    }

    pub fn in_memory() -> Self {
        Self::new(Store::InMemory(Default::default()))
    }

    /// The newest `count` entries, newest first.
    async fn recent(&self, count: usize) -> Result<Vec<Entry>> {
        match &self.store {
            Store::Valkey(client) => {
                let raw: Vec<String> = client.lrange(VALKEY_KEY, 0, count as isize - 1).await?;
                Ok(raw
                    .iter()
                    .filter_map(|raw| {
                        serde_json::from_str(raw)
                            .inspect_err(|error| {
                                tracing::warn!(?error, raw, "bad event list entry");
                            })
                            .ok()
                    })
                    .collect())
            }
            Store::InMemory(entries) => Ok(entries
                .lock()
                .unwrap()
                .iter()
                .take(count)
                .cloned()
                .collect()),
        }
    }

    /// The routes of the page, `/event-list` and its SSE stream
    /// `/event-list/events`. Each SSE message has all the shown entries.
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route(
                "/event-list",
                get(async |State(s): State<Arc<Self>>| {
                    let entries = s.recent(SHOWN).await.unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to read the event list");
                        Vec::new()
                    });
                    page(&entries)
                }),
            )
            .route(
                "/event-list/events",
                get(async |State(s): State<Arc<Self>>| {
                    let rx = s.updates.subscribe();
                    // the first message is the current list, so that a
                    // reconnected page gets what it missed
                    let stream =
                        futures::stream::try_unfold((s, rx, true), async |(s, mut rx, first)| {
                            if !first {
                                match rx.recv().await {
                                    Ok(()) | Err(RecvError::Lagged(_)) => {}
                                    Err(RecvError::Closed) => return anyhow::Ok(None),
                                }
                            }
                            let html = render_entries(&s.recent(SHOWN).await?);
                            Ok(Some((Event::default().data(html.0), (s, rx, false))))
                        });
                    Sse::new(stream).keep_alive(Default::default())
                }),
            )
            .with_state(self.clone())
    }
}

fn page(entries: &[Entry]) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                title { "Event list" }
                style { (PreEscaped(include_str!("event-list.css"))) }
            }
            body {
                div #list .widget-EventList { (render_entries(entries)) }
                script { (PreEscaped(include_str!("event-list.js"))) }
            }
        }
    }
}

#[async_trait]
impl EventListService for EventList {
    async fn push(&self, event: ListedEvent) -> Result<()> {
        let entry = Entry {
            id: uuid::Uuid::now_v7().to_string(),
            at: Timestamp::now().as_millisecond(),
            event,
        };
        match &self.store {
            Store::Valkey(client) => {
                let mut t = client.create_transaction();
                t.lpush(VALKEY_KEY, serde_json::to_string(&entry)?).forget();
                t.ltrim(VALKEY_KEY, 0, KEPT as isize - 1).forget();
                t.execute::<()>().await?;
            }
            Store::InMemory(entries) => {
                let mut entries = entries.lock().unwrap();
                entries.push_front(entry);
                entries.truncate(KEPT);
            }
        }
        _ = self.updates.send(());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money() {
        assert_eq!(format_money("5.00", "USD"), "$5");
        assert_eq!(format_money("4.50", "USD"), "$4.50");
        assert_eq!(format_money("3", "EUR"), "3 EUR");
        assert_eq!(format_money("3.10", "EUR"), "3.10 EUR");
    }

    #[test]
    fn entry_json() {
        let entry = Entry {
            id: "x".into(),
            at: 0,
            event: ListedEvent::PowerUp {
                user: "necauqua".into(),
                bits: 100,
                power_up: PowerUp::Custom {
                    title: "Hydrate".into(),
                },
            },
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: Entry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.event, entry.event);
    }

    #[tokio::test]
    async fn newest_first() -> Result<()> {
        let list = EventList::in_memory();
        for viewers in 0..(KEPT as i64 + 5) {
            list.push(ListedEvent::Raid {
                user: "a".into(),
                viewers,
            })
            .await?;
        }
        let recent = list.recent(SHOWN).await?;
        assert_eq!(recent.len(), SHOWN);
        assert_eq!(
            recent[0].event,
            ListedEvent::Raid {
                user: "a".into(),
                viewers: KEPT as i64 + 4
            }
        );
        let Store::InMemory(entries) = &list.store else {
            unreachable!()
        };
        assert_eq!(entries.lock().unwrap().len(), KEPT);
        Ok(())
    }

    #[tokio::test]
    async fn served_with_display_server() -> Result<()> {
        use anyhow::anyhow;
        use futures::StreamExt;
        use reqwest_sse::EventSource;

        use crate::services::display::DisplayServer;

        let list = Arc::new(EventList::in_memory());
        list.push(ListedEvent::Follow {
            user: "first".into(),
        })
        .await?;

        let bind_addr = "localhost:12313";
        let server = Arc::new(DisplayServer::default());
        tokio::spawn(server.start(bind_addr, list.router()));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let client = reqwest::Client::new();
        let url = format!("http://{bind_addr}/event-list");

        let body = client.get(&url).send().await?.text().await?;
        assert!(body.contains("widget-EventList"));
        assert!(body.contains("first"));

        let mut events = client
            .get(format!("{url}/events"))
            .send()
            .await?
            .events()
            .await
            .map_err(|e| anyhow!(e))?;
        let event = events.next().await.unwrap().map_err(|e| anyhow!(e))?;
        assert!(event.data.contains("first"));

        list.push(ListedEvent::Raid {
            user: "second".into(),
            viewers: 5,
        })
        .await?;
        let event = events.next().await.unwrap().map_err(|e| anyhow!(e))?;
        assert!(event.data.contains("second"));
        assert!(event.data.find("second") < event.data.find("first"));

        Ok(())
    }

    #[test]
    fn render_escapes() {
        let entry = Entry {
            id: "x".into(),
            at: 0,
            event: ListedEvent::Follow { user: "<b>".into() },
        };
        let html = entry.render().0;
        assert!(html.contains("&lt;b&gt;"));
        assert!(html.contains("followed the channel"));
    }
}
