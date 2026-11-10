//! The event list browser source: the most recent follows, subs, cheers,
//! raids, donations etc. The entries are in Valkey, so that a reload of the
//! page or a restart of the bot does not clear the list.
//!
//! Valkey keeps the entries as the page shows them. When a new event can
//! merge with a shown entry of the same user and kind (bits, gift subs,
//! Ko-fi and charity donations), that entry gets the sum and moves to the
//! top, and no new entry is added. Thus one user cannot fill the list.
//!
//! Twitch often sends a resub as a sub first, and then as a resub with the
//! months when the user shares the resub message. Thus a resub replaces a
//! shown sub of the same user in the same way, and a sub of a user with a
//! shown resub is not added.

use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex},
};

use anyhow::{Result, bail};
use async_trait::async_trait;
use axum::{
    Router,
    extract::{State, ws::WebSocketUpgrade},
    routing::get,
};
use jiff::{Timestamp, tz::TimeZone};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use rustis::{
    client::{BatchPreparedCommand, Client as ValkeyClient},
    commands::ListCommands,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use twitch_api::types::SubscriptionTier;

use crate::{
    injector_getter,
    services::{
        Service,
        live::{self, LiveEvent},
    },
};

const VALKEY_KEY: &str = "event-list";
/// The number of entries that Valkey keeps.
const KEPT: usize = 50;
/// The number of entries on the page. A new event merges only with these.
const SHOWN: usize = 3;

/// The name that is shown for anonymous gifts and cheers.
const ANONYMOUS: &str = "Anonymous";

#[async_trait]
pub trait EventListService: Service {
    async fn push(&self, event: ListedEvent) -> Result<()>;
}

injector_getter!(EventListService::event_list { EventList::in_memory() });

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KofiKind {
    Donation,
    Sub,
    Resub,
    ShopOrder,
}

/// An amount of money in one currency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    /// The amount in units of `10^-scale`
    units: i64,
    scale: u32,
    currency: String,
}

/// The maximum number of digits after the decimal point.
const MAX_SCALE: u32 = 9;

impl Money {
    pub fn new(units: i64, scale: u32, currency: impl Into<String>) -> Self {
        Self {
            units,
            scale,
            currency: currency.into(),
        }
    }

    /// Parses a decimal amount, e.g. `4.50`.
    pub fn parse(amount: &str, currency: &str) -> Result<Self> {
        let (whole, fraction) = amount.split_once('.').unwrap_or((amount, ""));
        let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if whole.is_empty() || !digits(whole) || !digits(fraction) {
            bail!("bad amount {amount:?}");
        }
        if fraction.len() > MAX_SCALE as usize {
            bail!("too many decimals in {amount:?}");
        }
        Ok(Self {
            units: format!("{whole}{fraction}").parse()?,
            scale: fraction.len() as u32,
            currency: currency.into(),
        })
    }

    fn rescaled(&self, scale: u32) -> Option<i64> {
        self.units.checked_mul(10i64.pow(scale - self.scale))
    }

    /// Adds `other` when it has the same currency. Returns `false` (and
    /// does not change `self`) when it cannot.
    fn add(&mut self, other: &Self) -> bool {
        if self.currency != other.currency {
            return false;
        }
        let scale = self.scale.max(other.scale);
        let sum = (self.rescaled(scale))
            .zip(other.rescaled(scale))
            .and_then(|(a, b)| a.checked_add(b));
        let Some(units) = sum else {
            return false;
        };
        self.units = units;
        self.scale = scale;
        true
    }

    /// The amount without the currency, without a fraction of zeroes:
    /// `5`, `4.50`.
    fn amount(&self) -> String {
        let divisor = 10i64.pow(self.scale);
        let (whole, fraction) = (self.units / divisor, self.units % divisor);
        if fraction == 0 {
            whole.to_string()
        } else {
            format!("{whole}.{fraction:0width$}", width = self.scale as usize)
        }
    }

    /// The amount is a number that the page counts up.
    fn render(&self) -> Markup {
        let amount = self.amount();
        let number = html! { span.number data-value=(amount) { (amount) } };
        html! {
            @if self.currency == "USD" {
                "$" (number)
            } @else {
                (number) " " (self.currency)
            }
        }
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.currency.as_str() {
            "USD" => write!(f, "${}", self.amount()),
            currency => write!(f, "{} {currency}", self.amount()),
        }
    }
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
    /// Bits: cheers, and bits used on Power-ups
    Cheer {
        user: Option<String>,
        bits: i64,
    },
    Raid {
        user: String,
        viewers: i64,
    },
    Charity {
        user: String,
        amount: Money,
        charity: String,
    },
    /// Only public ones
    Kofi {
        user: String,
        amount: Money,
        kind: KofiKind,
    },
}

fn plural(n: i64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The Twitch cheermote tier of an amount of bits; it sets the color.
fn bits_tier(bits: i64) -> i64 {
    [100_000, 10_000, 5_000, 1_000, 100]
        .into_iter()
        .find(|&tier| bits >= tier)
        .unwrap_or(1)
}

/// A number that the page counts up when it changes.
fn number(n: i64) -> Markup {
    html! { span.number data-value=(n) { (n) } }
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
            | Self::Raid { user, .. }
            | Self::Charity { user, .. }
            | Self::Kofi { user, .. } => user,
            Self::SubGift { user, .. } | Self::Cheer { user, .. } => {
                user.as_deref().unwrap_or(ANONYMOUS)
            }
        }
    }

    /// Adds `new` to this event when they are of the same user and kind
    /// and can have a sum: bits, gift subs, Ko-fi donations and charity
    /// donations (to the same charity) in the same currency. All the
    /// anonymous events are of one user. A resub replaces a sub of the
    /// same user. Returns `false` (and does not change `self`) when they do
    /// not merge.
    fn merge(&mut self, new: &Self) -> bool {
        match (&mut *self, new) {
            (Self::Sub { user, .. }, Self::Resub { user: u, .. }) if user == u => {
                *self = new.clone();
                true
            }
            (Self::Cheer { user, bits }, Self::Cheer { user: u, bits: b }) if user == u => {
                *bits += b;
                true
            }
            (Self::SubGift { user, count }, Self::SubGift { user: u, count: c }) if user == u => {
                *count += c;
                true
            }
            (
                Self::Kofi {
                    user,
                    amount,
                    kind: KofiKind::Donation,
                },
                Self::Kofi {
                    user: u,
                    amount: a,
                    kind: KofiKind::Donation,
                },
            ) if user == u => amount.add(a),
            (
                Self::Charity {
                    user,
                    amount,
                    charity,
                },
                Self::Charity {
                    user: u,
                    amount: a,
                    charity: c,
                },
            ) if user == u && charity == c => amount.add(a),
            _ => false,
        }
    }

    /// Whether `new` adds nothing to this event: a resub includes the sub
    /// of the same user that Twitch can send with it.
    fn covers(&self, new: &Self) -> bool {
        matches!(
            (self, new),
            (Self::Resub { user, .. }, Self::Sub { user: u, .. }) if user == u
        )
    }

    /// The text after the user name.
    fn render_text(&self) -> Markup {
        html! {
            @match self {
                Self::Follow { .. } => "followed the channel",
                Self::Sub { tier, .. } => {
                    @match tier {
                        SubscriptionTier::Tier2 => "subscribed to the channel at Tier 2!",
                        SubscriptionTier::Tier3 => "subscribed to the channel at Tier 3!",
                        SubscriptionTier::Prime => "subscribed to the channel with Prime!",
                        _ => "subscribed to the channel!",
                    }
                }
                Self::Resub { months, .. } => {
                    "resubscribed for " (months) " month" (plural(*months))
                }
                // "a" is 1 for the count-up when more gifts merge
                Self::SubGift { count: 1, .. } => {
                    "gifted " span.number data-value="1" { "a" } " sub"
                }
                Self::SubGift { count, .. } => { "gifted " (number(*count)) " subs" }
                Self::Cheer { bits, .. } => {
                    "cheered with "
                    span class={ "number bits-" (bits_tier(*bits)) } data-value=(bits) { (bits) }
                    " bit" (plural(*bits))
                }
                Self::Raid { viewers, .. } => {
                    "raided the stream with " (viewers) " viewer" (plural(*viewers))
                }
                Self::Charity { amount, charity, .. } => {
                    "donated " (amount.render()) " to " (charity)
                }
                Self::Kofi { amount, kind, .. } => {
                    @match kind {
                        KofiKind::Donation => { "donated " (amount.render()) " on Ko-fi" }
                        KofiKind::Sub => { "subscribed on Ko-fi (" (amount.render()) ")" }
                        KofiKind::Resub => { "continued the Ko-fi sub (" (amount.render()) ")" }
                        KofiKind::ShopOrder => "bought some merch",
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    /// Unix time in milliseconds of the last event
    at: i64,
    #[serde(flatten)]
    event: ListedEvent,
}

impl Entry {
    fn new(event: ListedEvent, at: i64) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            at,
            event,
        }
    }

    fn render(&self) -> Markup {
        let date = Timestamp::from_millisecond(self.at)
            .map(|at| {
                at.to_zoned(TimeZone::system())
                    .strftime("1%Y-%m-%d/%H:%M:%S%.3f |")
                    .to_string()
            })
            .unwrap_or_default();
        // a merge keeps the id and changes `at`
        html! {
            div class={ "entry " (self.event.class()) } data-id=(self.id) data-at=(self.at)
                data-kind=(self.event.class()) {
                span.date style="display:none" { (date) }
                span.message {
                    span.user { (self.event.user()) }
                    " " (self.event.render_text())
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
    /// A push reads and then writes the list, so only one push at a time.
    push_lock: tokio::sync::Mutex<()>,
    /// Notifies the open pages that the list changed.
    updates: broadcast::Sender<()>,
}

impl EventList {
    fn new(store: Store) -> Self {
        Self {
            store,
            push_lock: Default::default(),
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

    /// The routes of the page, `/event-list` and its live updates
    /// `/event-list/events`. Each `list` event has all the shown entries.
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
                get(async |State(s): State<Arc<Self>>, ws: WebSocketUpgrade| {
                    let rx = s.updates.subscribe();
                    // each update is the current list again
                    let list = move || {
                        let s = s.clone();
                        async move {
                            match s.recent(SHOWN).await {
                                Ok(entries) => {
                                    Some(LiveEvent::new("list", &render_entries(&entries).0))
                                }
                                Err(error) => {
                                    // the page connects again
                                    tracing::error!(?error, "failed to read the event list");
                                    None
                                }
                            }
                        }
                    };
                    live::serve(ws, live::with_snapshot(rx, list, |()| None))
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
                (live::script())
                script { (PreEscaped(include_str!("event-list.js"))) }
            }
        }
    }
}

#[async_trait]
impl EventListService for EventList {
    async fn push(&self, event: ListedEvent) -> Result<()> {
        let _lock = self.push_lock.lock().await;
        let now = Timestamp::now().as_millisecond();
        match &self.store {
            Store::Valkey(client) => {
                let shown: Vec<String> = client.lrange(VALKEY_KEY, 0, SHOWN as isize - 1).await?;
                let shown: Vec<(&String, Entry)> = (shown.iter())
                    .filter_map(|raw| Some((raw, serde_json::from_str(raw).ok()?)))
                    .collect();
                if shown.iter().any(|(_, entry)| entry.event.covers(&event)) {
                    return Ok(());
                }
                let merged = shown
                    .into_iter()
                    .find_map(|(raw, mut entry)| entry.event.merge(&event).then_some((raw, entry)));
                let mut t = client.create_transaction();
                let entry = match merged {
                    Some((raw, entry)) => {
                        t.lrem(VALKEY_KEY, 1, raw).forget();
                        Entry { at: now, ..entry }
                    }
                    None => Entry::new(event, now),
                };
                t.lpush(VALKEY_KEY, serde_json::to_string(&entry)?).forget();
                t.ltrim(VALKEY_KEY, 0, KEPT as isize - 1).forget();
                t.execute::<()>().await?;
            }
            Store::InMemory(entries) => {
                let mut entries = entries.lock().unwrap();
                if (entries.iter().take(SHOWN)).any(|entry| entry.event.covers(&event)) {
                    return Ok(());
                }
                let merged = (entries.iter_mut().take(SHOWN))
                    .position(|entry| entry.event.merge(&event));
                let entry = match merged.and_then(|index| entries.remove(index)) {
                    Some(entry) => Entry { at: now, ..entry },
                    None => Entry::new(event, now),
                };
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

    /// `$5` or `3 EUR`
    fn money(text: &str) -> Money {
        match text.strip_prefix('$') {
            Some(amount) => Money::parse(amount, "USD"),
            None => {
                let (amount, currency) = text.split_once(' ').unwrap();
                Money::parse(amount, currency)
            }
        }
        .unwrap()
    }

    #[test]
    fn money_text() {
        assert_eq!(Money::parse("5.00", "USD").unwrap().to_string(), "$5");
        assert_eq!(Money::parse("4.50", "USD").unwrap().to_string(), "$4.50");
        assert_eq!(Money::parse("3", "EUR").unwrap().to_string(), "3 EUR");
        assert_eq!(Money::parse("3.10", "EUR").unwrap().to_string(), "3.10 EUR");
        assert_eq!(Money::new(1005, 2, "USD").to_string(), "$10.05");
        assert!(Money::parse("-5", "USD").is_err());
        assert!(Money::parse(".5", "USD").is_err());
    }

    #[test]
    fn money_sum() {
        let mut sum = money("$5");
        assert!(sum.add(&money("$4.50")));
        assert_eq!(sum.to_string(), "$9.50");
        assert!(sum.add(&money("$0.50")));
        assert_eq!(sum.to_string(), "$10");
        assert!(!sum.add(&money("1 EUR")));
        assert_eq!(sum.to_string(), "$10");
    }

    #[test]
    fn entry_json() {
        let entry = Entry {
            id: "x".into(),
            at: 0,
            event: ListedEvent::Kofi {
                user: "necauqua".into(),
                amount: money("$4.50"),
                kind: KofiKind::Donation,
            },
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains(r#""amount":{"units":450,"scale":2,"currency":"USD"}"#));
        let back: Entry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.event, entry.event);
    }

    fn cheer(user: &str, bits: i64) -> ListedEvent {
        ListedEvent::Cheer {
            user: Some(user.into()),
            bits,
        }
    }

    fn follow(user: &str) -> ListedEvent {
        ListedEvent::Follow { user: user.into() }
    }

    #[test]
    fn merges() {
        let mut bits = cheer("a", 1);
        assert!(bits.merge(&cheer("a", 100)));
        assert_eq!(bits, cheer("a", 101));
        assert!(!bits.merge(&cheer("b", 1)));
        assert!(!bits.merge(&follow("a")));

        let mut anon = ListedEvent::SubGift {
            user: None,
            count: 1,
        };
        assert!(anon.merge(&ListedEvent::SubGift {
            user: None,
            count: 5
        }));
        assert_eq!(
            anon,
            ListedEvent::SubGift {
                user: None,
                count: 6
            }
        );

        let kofi = |amount, kind| ListedEvent::Kofi {
            user: "a".into(),
            amount: money(amount),
            kind,
        };
        let mut dono = kofi("$5", KofiKind::Donation);
        assert!(dono.merge(&kofi("$3", KofiKind::Donation)));
        assert_eq!(dono, kofi("$8", KofiKind::Donation));
        assert!(!dono.merge(&kofi("3 EUR", KofiKind::Donation)));
        assert!(!dono.merge(&kofi("$3", KofiKind::Resub)));
        assert!(!kofi("$3", KofiKind::Sub).merge(&kofi("$3", KofiKind::Sub)));

        let charity = |amount, charity: &str| ListedEvent::Charity {
            user: "a".into(),
            amount: money(amount),
            charity: charity.into(),
        };
        let mut dono = charity("$5", "MSF");
        assert!(dono.merge(&charity("$5", "MSF")));
        assert_eq!(dono, charity("$10", "MSF"));
        assert!(!dono.merge(&charity("$5", "other")));

        let mut entry = sub("a");
        assert!(!entry.merge(&resub("b", 5)));
        assert!(!entry.merge(&sub("a")));
        assert!(entry.merge(&resub("a", 5)));
        assert_eq!(entry, resub("a", 5));
        assert!(!entry.merge(&resub("a", 6)));
        assert!(!entry.merge(&sub("a")));
        assert!(entry.covers(&sub("a")));
        assert!(!entry.covers(&sub("b")));
        assert!(!sub("a").covers(&resub("a", 5)));
    }

    fn sub(user: &str) -> ListedEvent {
        ListedEvent::Sub {
            user: user.into(),
            tier: SubscriptionTier::Tier1,
        }
    }

    fn resub(user: &str, months: i64) -> ListedEvent {
        ListedEvent::Resub {
            user: user.into(),
            months,
        }
    }

    #[tokio::test]
    async fn resub_replaces_sub() -> Result<()> {
        let list = EventList::in_memory();
        list.push(sub("a")).await?;
        let id = list.recent(1).await?[0].id.clone();
        list.push(follow("b")).await?;
        list.push(resub("a", 5)).await?;
        assert_eq!(shown(&list).await, [resub("a", 5), follow("b")]);
        assert_eq!(list.recent(1).await?[0].id, id);

        // the sub after the resub is not added
        list.push(follow("c")).await?;
        list.push(sub("a")).await?;
        assert_eq!(
            shown(&list).await,
            [follow("c"), resub("a", 5), follow("b")]
        );
        Ok(())
    }

    async fn shown(list: &EventList) -> Vec<ListedEvent> {
        let entries = list.recent(SHOWN).await.unwrap();
        entries.into_iter().map(|entry| entry.event).collect()
    }

    #[tokio::test]
    async fn merged_entry_moves_to_top() -> Result<()> {
        let list = EventList::in_memory();
        list.push(cheer("a", 1)).await?;
        let id = list.recent(1).await?[0].id.clone();
        list.push(cheer("a", 1)).await?;
        assert_eq!(shown(&list).await, [cheer("a", 2)]);

        list.push(follow("b")).await?;
        list.push(follow("c")).await?;
        list.push(cheer("a", 100)).await?;
        assert_eq!(
            shown(&list).await,
            [cheer("a", 102), follow("c"), follow("b")]
        );
        assert_eq!(list.recent(1).await?[0].id, id);

        // not shown any more: a new entry
        list.push(follow("d")).await?;
        list.push(follow("e")).await?;
        list.push(follow("f")).await?;
        list.push(cheer("a", 5)).await?;
        assert_eq!(
            shown(&list).await,
            [cheer("a", 5), follow("f"), follow("e")]
        );
        Ok(())
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
        use crate::services::{
            display::{DisplayServer, wait_until_listening},
            live::TestClient,
        };

        let list = Arc::new(EventList::in_memory());
        list.push(ListedEvent::Follow {
            user: "first".into(),
        })
        .await?;

        let bind_addr = "localhost:12313";
        let server = Arc::new(DisplayServer::default());
        tokio::spawn(server.start(bind_addr, list.router()));
        wait_until_listening(bind_addr).await;

        let client = reqwest::Client::new();
        let url = format!("http://{bind_addr}/event-list");

        let body = client.get(&url).send().await?.text().await?;
        assert!(body.contains("widget-EventList"));
        assert!(body.contains("first"));

        let mut events = TestClient::connect(bind_addr, "/event-list/events").await?;
        let (event, data) = events.next().await?;
        assert_eq!(event, "list");
        assert!(data.contains("first"));

        list.push(ListedEvent::Raid {
            user: "second".into(),
            viewers: 5,
        })
        .await?;
        let (_, data) = events.next().await?;
        assert!(data.contains("second"));
        assert!(data.find("second") < data.find("first"));

        Ok(())
    }

    #[test]
    fn render() {
        let entry = Entry {
            id: "x".into(),
            at: 0,
            event: ListedEvent::Follow { user: "<b>".into() },
        };
        let html = entry.render().0;
        assert!(html.contains("&lt;b&gt;"));
        assert!(html.contains("followed the channel"));

        let html = Entry::new(cheer("a", 1500), 0).render().0;
        assert!(html.contains(
            r#"cheered with <span class="number bits-1000" data-value="1500">1500</span> bits"#
        ));
    }
}
