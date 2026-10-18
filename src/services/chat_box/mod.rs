//! The chat box browser sources: `/chat` (the chat) and `/chat/last` (only
//! the last message). They replace a Streamlabs widget and use its HTML
//! structure and CSS.
//!
//! The messages come from an anonymous IRC connection, so that the bot's own
//! messages are there too. The last [`KEPT`] messages are in memory; at
//! start they come from the chat log, after the last [`clear`]. The pages
//! get the full list on each (re)connect, and then the changes.
//!
//! [`clear`]: ChatBoxService::clear

mod cache;
mod colors;
mod emoji;
pub mod images;
mod message;
mod providers;
mod render;
mod seventv;

use std::{
    collections::VecDeque,
    convert::Infallible,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use anyhow::Result;
use async_trait::async_trait;
use axum::{
    Router,
    extract::State,
    response::{Sse, sse::Event},
    routing::get,
};
use dashmap::DashMap;
use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use jiff::Timestamp;
use maud::{DOCTYPE, Markup, PreEscaped, html};
use tokio::sync::broadcast::{self, error::RecvError};
use twitch_irc::{
    ClientConfig, MetricsConfig, SecureTCPTransport, TwitchIRCClient,
    login::StaticLoginCredentials,
    message::{ClearChatAction, IRCMessage, RGBColor, ServerMessage},
};

pub use self::message::ChatMessage;
use self::{
    cache::FetchCache,
    images::ImageCache,
    providers::Providers,
    render::{Context, Room},
};
use crate::{
    injector_getter,
    integration::twitch_api::TwitchApi,
    services::{Service, chat_log::ChatLogService, storage::StorageService},
};

/// The number of messages that the pages get
const KEPT: usize = 100;
/// The number of chat log messages for the backlog; more than [`KEPT`],
/// because the deletions are in the log too
const BACKLOG_LOG_MESSAGES: u64 = 300;
/// The time of the last [`ChatBoxService::clear`], Unix milliseconds
const CLEARED_AT_KEY: &str = "chat-box:cleared-at";
/// All emote and badge data is fetched again this often
const REFRESH_PERIOD: Duration = Duration::from_secs(30 * 60);
/// How long the 7TV cosmetics of a chatter are fresh
const USER_TTL: Duration = Duration::from_secs(60 * 60);
/// How long the data of another channel in a shared chat is fresh
const ROOM_TTL: Duration = Duration::from_secs(6 * 60 * 60);

#[async_trait]
pub trait ChatBoxService: Service {
    /// Removes all the messages from the pages (at stream start). After a
    /// restart, the backlog starts here.
    async fn clear(&self) -> Result<()>;
}

injector_getter!(ChatBoxService::chat_box { ChatBoxNoop });

pub struct ChatBoxNoop;

#[async_trait]
impl ChatBoxService for ChatBoxNoop {
    async fn clear(&self) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
enum Update {
    Message(Arc<str>),
    /// A message again, with the data that was fetched after it was shown
    Replace(Arc<str>),
    Delete(String),
    DeleteUser(String),
    Clear,
}

/// EventSource does not dispatch events with empty data.
fn sse(event: &str, data: &str) -> Event {
    Event::default()
        .event(event)
        .data(if data.is_empty() { " " } else { data })
}

impl Update {
    fn event(&self) -> Event {
        match self {
            Update::Message(html) => sse("message", html),
            Update::Replace(html) => sse("replace", html),
            Update::Delete(id) => sse("delete", id),
            Update::DeleteUser(id) => sse("delete-user", id),
            Update::Clear => sse("clear", ""),
        }
    }
}

struct Entry {
    msg: ChatMessage,
    user: Option<Arc<seventv::User>>,
    room: Option<Arc<Room>>,
    deleted: bool,
}

pub struct ChatBox {
    channel_id: String,
    channel_login: String,
    /// `None` in tests
    twitch: Option<TwitchApi>,
    storage: Arc<dyn StorageService>,
    /// `None` in tests
    chat_log: Option<Arc<dyn ChatLogService>>,
    http: reqwest::Client,
    images: Arc<ImageCache>,
    providers: RwLock<Arc<Providers>>,
    /// Only one refresh at a time
    refreshing: tokio::sync::Mutex<()>,
    /// Asks [`ChatBox::keep_up_to_date`] for a refresh now
    refresh_now: tokio::sync::Notify,
    /// The 7TV cosmetics of the chatters, by Twitch id
    seventv_users: FetchCache<seventv::User>,
    /// The other channels of a shared chat, by id
    rooms: FetchCache<Room>,
    /// No background fetches of chatter and channel data (in tests)
    offline: bool,
    /// The name colors of the chatters (lowercase logins), for the mentions
    colors: DashMap<String, RGBColor>,
    /// Oldest first
    entries: Mutex<VecDeque<Entry>>,
    updates: broadcast::Sender<Update>,
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("limatoukka chat box (https://github.com/necauqua/limatoukka)")
        .timeout(Duration::from_secs(15))
        .build()
        .expect("a static reqwest client config")
}

impl ChatBox {
    pub fn new(
        twitch_api: &TwitchApi,
        storage: Arc<dyn StorageService>,
        chat_log: Arc<dyn ChatLogService>,
    ) -> Self {
        Self::create(
            twitch_api.caster_id(),
            twitch_api.caster_login(),
            Some(twitch_api.clone()),
            storage,
            Some(chat_log),
        )
    }

    fn create(
        channel_id: &str,
        channel_login: &str,
        twitch: Option<TwitchApi>,
        storage: Arc<dyn StorageService>,
        chat_log: Option<Arc<dyn ChatLogService>>,
    ) -> Self {
        let http = http_client();
        Self {
            channel_id: channel_id.to_owned(),
            channel_login: channel_login.to_owned(),
            twitch,
            storage,
            chat_log,
            images: Arc::new(ImageCache::new(images::default_dir(), http.clone())),
            http,
            providers: Default::default(),
            refreshing: Default::default(),
            refresh_now: Default::default(),
            seventv_users: FetchCache::new("the 7TV cosmetics", USER_TTL),
            rooms: FetchCache::new("a shared chat channel", ROOM_TTL),
            offline: false,
            colors: Default::default(),
            entries: Default::default(),
            updates: broadcast::Sender::new(64),
        }
    }

    fn providers(&self) -> Arc<Providers> {
        self.providers.read().unwrap().clone()
    }

    /// Gets all the emote and badge data again.
    async fn refresh(&self) {
        let _guard = self.refreshing.lock().await;
        let new = self
            .providers()
            .refreshed(&self.http, self.twitch.as_ref(), &self.channel_id)
            .await;
        tracing::info!(
            seventv = new.seventv_channel.emotes.len(),
            bttv = new.bttv_channel.len(),
            ffz = new.ffz_room.emotes.len(),
            "chat box: emote data refreshed"
        );
        *self.providers.write().unwrap() = Arc::new(new);
    }

    /// Gets the 7TV channel emotes again.
    async fn refresh_seventv(&self) {
        let _guard = self.refreshing.lock().await;
        match seventv::channel(&self.http, &self.channel_id).await {
            Ok(channel) => {
                let mut new = (*self.providers()).clone();
                new.seventv_channel = channel;
                *self.providers.write().unwrap() = Arc::new(new);
            }
            Err(error) => tracing::warn!(?error, "chat box: failed to get the 7TV emotes"),
        }
    }

    /// The cached 7TV cosmetics of a chatter. When they are not fresh, they
    /// are fetched in the background, and then the messages of the chatter
    /// are shown again with them.
    fn seventv_user(self: &Arc<Self>, user_id: &str) -> Option<Arc<seventv::User>> {
        let lookup = self.seventv_users.lookup(user_id);
        if lookup.fetch && !self.offline {
            tokio::spawn(self.clone().load_seventv_user(user_id.to_owned()));
        }
        lookup.value
    }

    async fn load_seventv_user(self: Arc<Self>, user_id: String) {
        self.seventv_users
            .fetch(&user_id, seventv::user(&self.http, &user_id))
            .await;
        self.update_entries(|msg| msg.user_id == user_id);
    }

    /// The cached data of another channel in a shared chat. When it is not
    /// fresh, it is fetched in the background, and then the messages from
    /// the channel are shown again with it.
    fn room(self: &Arc<Self>, room_id: &str) -> Option<Arc<Room>> {
        // no data without the Twitch API
        self.twitch.as_ref()?;
        let lookup = self.rooms.lookup(room_id);
        if lookup.fetch && !self.offline {
            tokio::spawn(self.clone().load_room(room_id.to_owned()));
        }
        lookup.value
    }

    async fn load_room(self: Arc<Self>, room_id: String) {
        let Some(api) = &self.twitch else {
            return;
        };
        let fetch = async {
            let badges = providers::twitch_channel_badges(api, &room_id).await?;
            let user = api
                .call(async |t| t.helix.get_user_from_id(&*room_id, &t.token).await)
                .await?;
            anyhow::Ok(Some(Room {
                name: user
                    .as_ref()
                    .map(|user| user.display_name.to_string())
                    .unwrap_or_default(),
                avatar: user.and_then(|user| user.profile_image_url),
                badges,
            }))
        };
        self.rooms.fetch(&room_id, fetch).await;
        self.update_entries(|msg| msg.source_room.as_deref() == Some(&*room_id));
    }

    /// Shows the matching messages again, when the cached data for them
    /// changed.
    fn update_entries(&self, matches: impl Fn(&ChatMessage) -> bool) {
        let providers = self.providers();
        let mut entries = self.entries.lock().unwrap();
        for entry in entries.iter_mut().filter(|entry| matches(&entry.msg)) {
            let user = self.seventv_users.value(&entry.msg.user_id);
            let room = (entry.msg.source_room.as_deref()).and_then(|room| self.rooms.value(room));
            if same(&user, &entry.user) && same(&room, &entry.room) {
                continue;
            }
            let old = self.render(entry, &providers).0;
            entry.user = user;
            entry.room = room;
            let new = self.render(entry, &providers).0;
            if new != old {
                _ = self.updates.send(Update::Replace(new.into()));
            }
        }
    }

    fn render(&self, entry: &Entry, providers: &Providers) -> Markup {
        let mention_color = |login: &str| self.colors.get(login).map(|color| *color);
        let ctx = Context {
            providers,
            user: entry.user.as_deref(),
            room: entry.room.as_deref(),
            mention_color: &mention_color,
        };
        render::render(&entry.msg, &ctx, entry.deleted)
    }

    fn render_all(&self) -> String {
        let providers = self.providers();
        let entries = self.entries.lock().unwrap();
        entries
            .iter()
            .map(|entry| self.render(entry, &providers).0)
            .collect()
    }

    /// Adds a message. It is shown at once, with the cached data of the
    /// chatter; see [`seventv_user`](Self::seventv_user).
    pub fn push(self: &Arc<Self>, msg: ChatMessage) {
        if let Some(color) = msg.color {
            self.colors.insert(msg.login.to_lowercase(), color);
        }
        let html = {
            // the lookups are under the lock, so that a fetch that ends
            // after them sees this entry in update_entries
            let mut entries = self.entries.lock().unwrap();
            if entries.iter().any(|e| e.msg.id == msg.id) {
                return;
            }
            let entry = Entry {
                user: self.seventv_user(&msg.user_id),
                room: (msg.source_room.as_deref()).and_then(|room| self.room(room)),
                msg,
                deleted: false,
            };
            let html = self.render(&entry, &self.providers()).0;
            entries.push_back(entry);
            while entries.len() > KEPT {
                entries.pop_front();
            }
            html
        };
        _ = self.updates.send(Update::Message(html.into()));
    }

    fn delete(&self, id: &str) {
        let mut entries = self.entries.lock().unwrap();
        for entry in entries.iter_mut().filter(|e| e.msg.id == id) {
            entry.deleted = true;
        }
        _ = self.updates.send(Update::Delete(id.to_owned()));
    }

    fn delete_user(&self, user_id: &str) {
        let mut entries = self.entries.lock().unwrap();
        for entry in entries.iter_mut().filter(|e| e.msg.user_id == user_id) {
            entry.deleted = true;
        }
        _ = self.updates.send(Update::DeleteUser(user_id.to_owned()));
    }

    /// Removes all the messages from the pages, without the time in the
    /// storage (see [`ChatBoxService::clear`]).
    pub fn clear_shown(&self) {
        self.entries.lock().unwrap().clear();
        _ = self.updates.send(Update::Clear);
    }

    /// Handles a `USERNOTICE`.
    fn handle_notice(self: &Arc<Self>, irc: &IRCMessage) {
        let channel = irc.params.first().and_then(|c| c.strip_prefix('#'));
        if channel == Some(&*self.channel_login)
            && let Some(msg) = ChatMessage::from_notice(irc)
        {
            self.push(msg);
        }
    }

    /// Handles a message from the chat log.
    fn handle_raw(self: &Arc<Self>, irc: IRCMessage) {
        if irc.command == "USERNOTICE" {
            return self.handle_notice(&irc);
        }
        match ServerMessage::try_from(irc) {
            Ok(msg) => self.handle_irc(msg),
            Err(error) => tracing::warn!(?error, "chat box: bad archived message"),
        }
    }

    /// Handles a message from IRC.
    pub fn handle_irc(self: &Arc<Self>, msg: ServerMessage) {
        // also the ones that twitch-irc failed to parse
        if msg.source().command == "USERNOTICE" {
            return self.handle_notice(msg.source());
        }
        match msg {
            ServerMessage::Privmsg(msg) if msg.channel_login == self.channel_login => {
                self.push(msg.into())
            }
            ServerMessage::ClearMsg(msg) if msg.channel_login == self.channel_login => {
                self.delete(&msg.message_id)
            }
            ServerMessage::ClearChat(msg) if msg.channel_login == self.channel_login => {
                match msg.action {
                    ClearChatAction::ChatCleared => self.clear_shown(),
                    ClearChatAction::UserBanned { user_id, .. }
                    | ClearChatAction::UserTimedOut { user_id, .. } => self.delete_user(&user_id),
                }
            }
            _ => {}
        }
    }

    /// Loads the messages after the last clear from the chat log.
    async fn load_backlog(self: &Arc<Self>) -> Result<()> {
        let Some(chat_log) = &self.chat_log else {
            return Ok(());
        };
        let after = self
            .storage
            .get(CLEARED_AT_KEY)
            .await?
            .and_then(|at| at.parse().ok());
        let messages = chat_log
            .recent_irc(&self.channel_login, after, BACKLOG_LOG_MESSAGES)
            .await?;

        self.prefetch(&messages).await;
        let count = messages.len();
        for msg in messages {
            self.handle_raw(msg);
        }
        tracing::info!(count, "chat box: loaded the backlog");
        Ok(())
    }

    /// Gets the data of all the chatters and shared chat channels of the
    /// backlog at once, so that the backlog is shown with it.
    async fn prefetch(self: &Arc<Self>, messages: &[IRCMessage]) {
        if self.offline {
            return;
        }
        let tag = |msg: &IRCMessage, name: &str| {
            msg.tags
                .0
                .get(name)
                .filter(|value| !value.is_empty())
                .cloned()
        };
        let mut user_ids = Vec::new();
        let mut room_ids = Vec::new();
        for msg in messages {
            if !matches!(&*msg.command, "PRIVMSG" | "USERNOTICE") {
                continue;
            }
            user_ids.extend(tag(msg, "user-id"));
            let source_room = tag(msg, "source-room-id");
            if source_room != tag(msg, "room-id") {
                room_ids.extend(source_room);
            }
        }
        for ids in [&mut user_ids, &mut room_ids] {
            ids.sort_unstable();
            ids.dedup();
        }
        let mut loads: Vec<BoxFuture<'static, ()>> = Vec::new();
        for id in user_ids {
            if self.seventv_users.lookup(&id).fetch {
                loads.push(self.clone().load_seventv_user(id).boxed());
            }
        }
        if self.twitch.is_some() {
            for id in room_ids {
                if self.rooms.lookup(&id).fetch {
                    loads.push(self.clone().load_room(id).boxed());
                }
            }
        }
        futures::stream::iter(loads)
            .buffer_unordered(8)
            .for_each(async |()| {})
            .await;
    }

    /// Keeps the emote data up to date: periodically, on request, and on
    /// the changes of the 7TV emote set.
    async fn keep_up_to_date(self: Arc<Self>) {
        let periodic = {
            let this = self.clone();
            async move {
                let mut interval = tokio::time::interval(REFRESH_PERIOD);
                interval.tick().await; // the first tick is now, and run() refreshed
                loop {
                    tokio::select! {
                        _ = interval.tick() => {}
                        _ = this.refresh_now.notified() => {}
                    }
                    this.refresh().await;
                }
            }
        };
        let seventv_events = async move {
            loop {
                let channel = self.providers().seventv_channel.clone();
                let (Some(set_id), Some(user_id)) = (channel.set_id, channel.user_id) else {
                    tokio::time::sleep(REFRESH_PERIOD).await;
                    continue;
                };
                let (changed_tx, mut changed_rx) = tokio::sync::mpsc::unbounded_channel();
                let watch = seventv::watch(&set_id, &user_id, &self.channel_id, |change| {
                    match change {
                        seventv::Change::Channel => _ = changed_tx.send(()),
                        // the next message of the user gets them again
                        seventv::Change::Cosmetics(users) => {
                            for user in users {
                                self.seventv_users.invalidate(&user);
                            }
                        }
                    }
                });
                let refresh_on_change = async {
                    while changed_rx.recv().await.is_some() {
                        // a change often comes with more changes
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        while changed_rx.try_recv().is_ok() {}
                        self.refresh_seventv().await;
                        if self.providers().seventv_channel.set_id.as_deref() != Some(&set_id) {
                            tracing::info!("chat box: the 7TV emote set changed");
                            return;
                        }
                    }
                };
                tokio::select! {
                    result = watch => {
                        if let Err(error) = result {
                            tracing::warn!(?error, "7TV EventAPI failed");
                        }
                        tokio::time::sleep(Duration::from_secs(10)).await;
                    }
                    _ = refresh_on_change => {}
                }
                // the changes while there was no connection
                self.refresh_seventv().await;
            }
        };
        tokio::join!(periodic, seventv_events);
    }

    /// Gets the emote data and the backlog, then keeps the data up to date.
    /// With `irc`, it also shows the chat of the channel; without it, only
    /// [`push`](Self::push) adds messages.
    pub async fn run(self: Arc<Self>, irc: bool) {
        // the chat is joined first, and the client keeps its messages until
        // the backlog is loaded: no message is missed between the two, and
        // the messages that are in both are shown once
        let chat = if irc {
            let (incoming, client) =
                TwitchIRCClient::<SecureTCPTransport, StaticLoginCredentials>::new(irc_config());
            match client.join(self.channel_login.clone()) {
                Ok(()) => Some((incoming, client)),
                Err(error) => {
                    tracing::error!(?error, "chat box: bad channel name");
                    None
                }
            }
        } else {
            None
        };
        self.refresh().await;
        if let Err(error) = self.load_backlog().await {
            tracing::error!(?error, "chat box: failed to load the backlog");
        }
        let updates = self.clone().keep_up_to_date();
        // the client must live as long as the chat is read
        let Some((mut incoming, _client)) = chat else {
            return updates.await;
        };
        let chat = async {
            while let Some(msg) = incoming.recv().await {
                self.handle_irc(msg);
            }
            tracing::error!("chat box: the IRC connection ended");
        };
        tokio::join!(updates, chat);
    }

    /// The routes of the pages, their SSE stream `/chat/events`, and the
    /// images at `/chat/img/...`.
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route(
                "/chat",
                get(async |State(s): State<Arc<Self>>| page(&s.render_all(), false)),
            )
            .route(
                "/chat/last",
                get(async |State(s): State<Arc<Self>>| page(&s.render_all(), true)),
            )
            .route(
                "/chat/events",
                get(async |State(s): State<Arc<Self>>| {
                    let rx = s.updates.subscribe();
                    // the first event is the full list, so that a
                    // reconnected page (e.g. after a restart) is correct
                    let stream =
                        futures::stream::unfold((s, rx, true), async |(s, mut rx, first)| {
                            let event = if first {
                                sse("reset", &s.render_all())
                            } else {
                                match rx.recv().await {
                                    Ok(update) => update.event(),
                                    Err(RecvError::Lagged(_)) => sse("reset", &s.render_all()),
                                    Err(RecvError::Closed) => return None,
                                }
                            };
                            Some((Ok::<_, Infallible>(event), (s, rx, false)))
                        });
                    Sse::new(stream).keep_alive(Default::default())
                }),
            )
            .with_state(self.clone())
            .merge(
                Router::new()
                    .route(
                        &format!("{}/{{*path}}", images::ROUTE),
                        get(ImageCache::serve),
                    )
                    .with_state(self.images.clone()),
            )
    }
}

/// Whether two cached values are the same one.
fn same<T>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (a, b) => a.is_none() && b.is_none(),
    }
}

/// The config of the anonymous IRC client.
fn irc_config() -> ClientConfig<StaticLoginCredentials> {
    ClientConfig {
        // the metrics of the bot client have the same names, and a second
        // registration panics
        metrics_config: MetricsConfig::Disabled,
        tracing_identifier: Some("chat-box".into()),
        ..Default::default()
    }
}

fn page(entries: &str, last_only: bool) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                title { "Chat" }
                style { (PreEscaped(include_str!("chat-box.css"))) }
            }
            body class=[last_only.then_some("last-only")] {
                div #log .sl__chat__layout data-kept=(KEPT) { (PreEscaped(entries)) }
                script { (PreEscaped(include_str!("chat-box.js"))) }
            }
        }
    }
}

#[async_trait]
impl ChatBoxService for ChatBox {
    async fn clear(&self) -> Result<()> {
        self.clear_shown();
        self.storage
            .set(
                CLEARED_AT_KEY,
                &Timestamp::now().as_millisecond().to_string(),
            )
            .await?;
        // e.g. new emotes for the stream
        self.refresh_now.notify_one();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use reqwest_sse::EventSource;
    use twitch_irc::message::IRCMessage;

    use super::*;
    use crate::services::storage::InMemoryStorageService;

    fn chat_box() -> Arc<ChatBox> {
        let mut chat_box = ChatBox::create(
            "160410682",
            "necauqua",
            None,
            Arc::new(InMemoryStorageService::default()),
            None,
        );
        // no 7TV requests
        chat_box.offline = true;
        Arc::new(chat_box)
    }

    /// Waits until the background fetches end, and writes a preview with
    /// the CDN image URLs. The background of the preview is dark, OBS has
    /// none.
    async fn write_preview(chat_box: &ChatBox, file: &str, html: impl Fn(String) -> String) {
        while chat_box.seventv_users.is_fetching() || chat_box.rooms.is_fetching() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        chat_box.update_entries(|_| true);
        let page = page(&chat_box.render_all(), false)
            .0
            .replace("/chat/img/", "https://")
            .replace("background: transparent", "background: #18181b");
        std::fs::write(file, html(page)).unwrap();
    }

    fn irc(raw: &str) -> ServerMessage {
        ServerMessage::try_from(IRCMessage::parse(raw).unwrap()).unwrap()
    }

    fn privmsg(id: &str, user_id: &str, text: &str) -> ServerMessage {
        irc(&format!(
            "@badge-info=;badges=;color=#0000FF;display-name=User{user_id};emotes=;id={id};\
             room-id=160410682;tmi-sent-ts=1594545155039;user-id={user_id} \
             :user{user_id}!user{user_id}@user{user_id}.tmi.twitch.tv PRIVMSG #necauqua :{text}"
        ))
    }

    fn ids(chat_box: &ChatBox) -> Vec<(String, bool)> {
        let entries = chat_box.entries.lock().unwrap();
        entries
            .iter()
            .map(|e| (e.msg.id.clone(), e.deleted))
            .collect()
    }

    #[tokio::test]
    async fn messages_and_deletions() {
        let chat_box = chat_box();
        chat_box.handle_irc(privmsg("a", "1", "hello"));
        chat_box.handle_irc(privmsg("b", "2", "hi"));
        chat_box.handle_irc(privmsg("c", "1", "bye"));
        // the same message from the backlog and IRC
        chat_box.handle_irc(privmsg("c", "1", "bye"));
        // other channels are not shown
        chat_box.handle_irc(irc(
            "@badge-info=;badges=;color=;display-name=X;emotes=;id=d;room-id=1;\
                 tmi-sent-ts=1;user-id=1 :x!x@x.tmi.twitch.tv PRIVMSG #limatoukka :x",
        ));
        assert_eq!(
            ids(&chat_box),
            [
                ("a".into(), false),
                ("b".into(), false),
                ("c".into(), false)
            ]
        );

        chat_box
            .handle_irc(irc(
                "@login=user2;room-id=;target-msg-id=b;tmi-sent-ts=1 :tmi.twitch.tv CLEARMSG #necauqua :hi",
            ));
        chat_box
            .handle_irc(irc(
                "@ban-duration=1;room-id=160410682;target-user-id=1;tmi-sent-ts=1 :tmi.twitch.tv CLEARCHAT #necauqua :user1",
            ));
        assert_eq!(
            ids(&chat_box),
            [("a".into(), true), ("b".into(), true), ("c".into(), true)]
        );
        assert!(chat_box.render_all().contains(r#"class="deleted""#));

        chat_box.clear().await.unwrap();
        assert!(ids(&chat_box).is_empty());
        assert!(
            chat_box
                .storage
                .get(CLEARED_AT_KEY)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn shown_again_with_fetched_data() {
        use self::providers::EmoteDef;

        let chat_box = chat_box();
        let mut updates = chat_box.updates.subscribe();
        chat_box.handle_irc(privmsg("a", "1", "hi mine"));
        let Ok(Update::Message(html)) = updates.try_recv() else {
            panic!("no message");
        };
        assert!(!html.contains("cdn.7tv.app"), "{html}");

        // the fetch of the personal emotes ends
        let emote = EmoteDef::new("mine", "https://cdn.7tv.app/emote/x/4x.webp");
        let user = seventv::User {
            emotes: [("mine".to_owned(), emote)].into(),
            ..Default::default()
        };
        chat_box.seventv_users.insert("1", Some(user));
        chat_box.update_entries(|msg| msg.user_id == "1");
        let Ok(Update::Replace(html)) = updates.try_recv() else {
            panic!("no replace");
        };
        assert!(html.contains(r#"data-id="a""#), "{html}");
        assert!(html.contains("cdn.7tv.app/emote/x"), "{html}");

        // the same data again: nothing to replace
        chat_box.update_entries(|msg| msg.user_id == "1");
        assert!(updates.try_recv().is_err());
    }

    #[tokio::test]
    async fn irc_client_beside_the_bot_client() {
        // the bot client has the default config, with the metrics
        let _bot = TwitchIRCClient::<SecureTCPTransport, StaticLoginCredentials>::new(
            ClientConfig::default(),
        );
        let _chat_box =
            TwitchIRCClient::<SecureTCPTransport, StaticLoginCredentials>::new(irc_config());
    }

    #[tokio::test]
    async fn keeps_the_last_ones() {
        let chat_box = chat_box();
        for i in 0..KEPT + 5 {
            chat_box.handle_irc(privmsg(&i.to_string(), "1", "spam"));
        }
        let ids = ids(&chat_box);
        assert_eq!(ids.len(), KEPT);
        assert_eq!(ids[0].0, "5");
    }

    /// Writes `target/chat-box-preview.html` (with the CDN image URLs) from
    /// the real emote data and backlog.
    #[tokio::test]
    #[ignore = "manual test"]
    async fn real_preview() -> Result<()> {
        use crate::{config::Config, services::chat_log::ChatLogServiceElastic};

        let config = Config::load()?;
        let twitch_api = TwitchApi::new(&config).await?;
        let chat_log = Arc::new(ChatLogServiceElastic::new(
            &config.elastic.url,
            &config.elastic.api_key,
            &config.elastic.index,
        )?);
        let chat_box = Arc::new(ChatBox::new(
            &twitch_api,
            Arc::new(InMemoryStorageService::default()),
            chat_log,
        ));
        chat_box.refresh().await;
        chat_box.load_backlog().await?;
        // zero-width, personal emotes, a paint, /me and a cheer
        let id = twitch_api.caster_id();
        let mut msg = ChatMessage::new(id, "necauqua", "necauqua", "ICANT RATS LoadingZW ratJAM");
        msg.badges = vec![twitch_irc::message::Badge {
            name: "broadcaster".into(),
            version: "1".into(),
        }];
        chat_box.push(msg.clone());
        chat_box.push(ChatMessage {
            is_action: true,
            ..ChatMessage::new(id, "necauqua", "necauqua", "does a /me POGGIES")
        });
        chat_box.push(ChatMessage {
            bits: Some(1100),
            ..ChatMessage::new("1", "cheerer", "Cheerer", "Cheer100 Cheer1000 ok")
        });
        write_preview(&chat_box, "target/chat-box-preview.html", |html| html).await;
        Ok(())
    }

    /// Writes `target/chat-box-edge-cases.html` (with the CDN image URLs)
    /// from the real emote data and fake messages with edge cases.
    #[tokio::test]
    #[ignore = "manual test"]
    async fn edge_cases_preview() -> Result<()> {
        use twitch_irc::message::{Badge, Emote, RGBColor};

        use crate::config::Config;

        let twitch_api = TwitchApi::new(&Config::load()?).await?;
        let chat_box = Arc::new(ChatBox::create(
            twitch_api.caster_id(),
            twitch_api.caster_login(),
            Some(twitch_api.clone()),
            Arc::new(InMemoryStorageService::default()),
            None,
        ));
        chat_box.refresh().await;

        let badges = |badges: &[&str]| {
            badges
                .iter()
                .map(|badge| {
                    let (name, version) = badge.split_once('/').unwrap();
                    Badge {
                        name: name.into(),
                        version: version.into(),
                    }
                })
                .collect::<Vec<_>>()
        };
        let color = |r, g, b| Some(RGBColor { r, g, b });
        let user = |text: &str| ChatMessage::new("100001", "chatter", "Chatter", text);
        let caster = |text: &str| ChatMessage {
            badges: badges(&["broadcaster/1", "subscriber/3024"]),
            ..ChatMessage::new(twitch_api.caster_id(), "necauqua", "necauqua", text)
        };
        let bot = |text: &str| ChatMessage {
            badges: badges(&["lead_moderator/1"]),
            color: color(0xFF, 0x69, 0xB4),
            ..ChatMessage::new(
                twitch_api.bot_id(),
                twitch_api.bot_login(),
                "Limatoukka",
                text,
            )
        };

        let messages = vec![
            user("a plain message, no badges, a default color"),
            ChatMessage {
                first: true,
                ..user("my first message here, hi!")
            },
            ChatMessage {
                is_action: true,
                ..user("does a /me with an emote ICANT")
            },
            ChatMessage {
                is_action: true,
                first: true,
                ..user("a first /me")
            },
            caster("the caster, with a 7TV paint and badge"),
            caster("personal emotes: ratJAM AINTNOWAY"),
            bot("the bot: a lead moderator with the FFZ bot badge"),
            bot("@necauqua: your balance = 704.098\u{26a1}\u{fe0e} (+1\u{26a1}\u{fe0e})"),
            ChatMessage {
                badges: badges(&[
                    "moderator/1",
                    "vip/1",
                    "subscriber/12",
                    "premium/1",
                    "glhf-pledge/1",
                ]),
                color: color(0x1E, 0x90, 0xFF),
                ..ChatMessage::new(
                    "100002",
                    "averylongname_ofachatter",
                    "AVeryLongName_OfAChatter",
                    "a long name with many badges",
                )
            },
            ChatMessage {
                color: color(0x0A, 0x00, 0x00),
                ..ChatMessage::new("100003", "darkred", "DarkRed", "a #0A0000 name")
            },
            ChatMessage {
                color: color(0x00, 0x00, 0xFF),
                ..ChatMessage::new("100004", "blue", "Blue", "a #0000FF name")
            },
            ChatMessage {
                color: color(0xFF, 0xFF, 0xFF),
                ..ChatMessage::new("100005", "white", "White", "a #FFFFFF name")
            },
            user("zero-width: ICANT RATS LoadingZW and alone RATS and at the start"),
            user("RATS hi"),
            user("BTTV :tf: FeelsGoodMan cvMask and FFZ CatBag LilZ ffzHyper"),
            ChatMessage {
                emotes: vec![
                    Emote {
                        id: "25".into(),
                        char_range: 7..12,
                        code: "Kappa".into(),
                    },
                    Emote {
                        id: "1902".into(),
                        char_range: 13..18,
                        code: "Keepo".into(),
                    },
                    // the Twitch bug: out of the message
                    Emote {
                        id: "25".into(),
                        char_range: 40..45,
                        code: "".into(),
                    },
                ],
                ..user("Twitch Kappa Keepo, and a bad range")
            },
            ChatMessage {
                bits: Some(116_151),
                ..user(
                    "Cheer1 Cheer100 Cheer1000 Cheer5000 Cheer10000 Cheer100000 cheer50 and text",
                )
            },
            user("Cheer100 without bits is text"),
            ChatMessage::new(
                "100006",
                "neca_jp",
                "\u{30cd}\u{30ab}",
                "\u{043f}\u{0440}\u{0438}\u{0432}\u{0456}\u{0442} \u{1f44b}\u{1f3fb} \u{65e5}\u{672c}\u{8a9e} \u{1f1fa}\u{1f1e6} emoji and a localized name",
            ),
            ChatMessage::new(
                "100007",
                "html",
                "<i>Html</i>",
                "<script>alert(1)</script> & <b>not bold</b> \"quotes\"",
            ),
            user(&format!("a long word: {}", "a".repeat(120))),
            user(&"ICANT POGGIES ".repeat(15)),
            user("a link https://example.com/a?b=c and a mention @necauqua"),
            user("   "),
            user("this message is deleted"),
        ];
        let deleted = messages.last().unwrap().id.clone();
        for msg in messages {
            chat_box.push(msg);
        }
        chat_box.delete(&deleted);

        // modifiers, wide zero-width emotes, emoji, mentions
        let more = vec![
            user("zero-width wider than the emote: ICANT MODDED and ICANT Aldent"),
            user("BTTV: w! ICANT h! ICANT v! ICANT l! ICANT r! ICANT"),
            user("BTTV: c! ICANT p! ICANT s! ICANT z! ICANT w! h! ICANT and w! alone"),
            user("FFZ: ICANT ffzW ICANT ffzX ICANT ffzY ICANT ffzCursed and a lone ffzX"),
            ChatMessage {
                // in the FFZ set of the animated modifiers
                login: "wid3__".into(),
                ..user(
                    "FFZ sub: ICANT ffzHyper ICANT ffzRainbow ICANT ffzJam ICANT ffzBounce ICANT ffzSpin ICANT ffzSlide",
                )
            },
            user(
                "emoji: \u{1f44b}\u{1f3fb} \u{2764}\u{fe0f} \u{1f1fa}\u{1f1e6} \u{1f3f3}\u{fe0f}\u{200d}\u{1f308} 1\u{fe0f}\u{20e3} and text 704\u{26a1}\u{fe0e}",
            ),
            user("mentions: @necauqua @Limatoukka, @unknown_user!"),
            ChatMessage::new("11148817", "pajlada", "pajlada", "a Chatterino badge"),
        ];
        for msg in more {
            chat_box.push(msg);
        }

        // from IRC: replies, highlights, Power-ups, shared chat, notices
        let tags = "badge-info=;badges=subscriber/0;color=#1E90FF;display-name=Rosa;emotes=;\
                    room-id=160410682;tmi-sent-ts=1;user-id=100010";
        let privmsg = |id: &str, extra: &str, text: &str| {
            IRCMessage::parse(&format!(
                "@{tags};id={id};{extra} :rosa!rosa@rosa.tmi.twitch.tv PRIVMSG #necauqua :{text}"
            ))
            .unwrap()
        };
        let notice = |id: &str, extra: &str, text: &str| {
            IRCMessage::parse(&format!(
                "@{tags};id={id};login=rosa;{extra} :tmi.twitch.tv USERNOTICE #necauqua{text}"
            ))
            .unwrap()
        };
        let sub = "msg-param-cumulative-months=14;msg-param-months=0;\
                   msg-param-should-share-streak=0;msg-param-sub-plan=1000;msg-param-sub-plan-name=Sub";
        let gift = "msg-param-months=1;msg-param-recipient-display-name=A;msg-param-recipient-id=5;\
                    msg-param-recipient-user-name=a;msg-param-sub-plan=1000;msg-param-sub-plan-name=Sub;\
                    msg-param-gift-months=1";
        let irc_messages = [
            privmsg(
                "r1",
                "reply-parent-display-name=necauqua;reply-parent-msg-body=the\\sparent\\smessage,\\swhich\\sis\\squite\\slong\\sand\\sgoes\\son\\sand\\son\\sand\\son;\
                 reply-parent-msg-id=p;reply-parent-user-id=160410682;reply-parent-user-login=necauqua",
                "@necauqua a reply, without the mention ICANT",
            ),
            privmsg("h1", "msg-id=highlighted-message", "a highlighted message"),
            privmsg(
                "g1",
                "msg-id=gigantified-emote-message",
                "a gigantified ICANT",
            ),
            privmsg(
                "e1",
                "msg-id=animated-message;animation-id=simmer",
                "a simmer effect",
            ),
            privmsg(
                "e2",
                "msg-id=animated-message;animation-id=rainbow-eclipse",
                "a rainbow eclipse effect",
            ),
            privmsg(
                "e3",
                "msg-id=animated-message;animation-id=cosmic-abyss",
                "a cosmic abyss effect",
            ),
            privmsg(
                "s1",
                "source-room-id=39063397;source-badges=founder/0;source-id=x",
                "from another channel in a shared chat",
            ),
            notice(
                "n1",
                &format!(
                    "msg-id=resub;{sub};system-msg=Rosa\\ssubscribed\\sat\\sTier\\s1.\\sThey've\\ssubscribed\\sfor\\s14\\smonths!"
                ),
                " :a resub message ICANT",
            ),
            notice(
                "n2",
                &format!("msg-id=sub;{sub};system-msg=Rosa\\ssubscribed\\sat\\sTier\\s1."),
                "",
            ),
            notice(
                "n3",
                "msg-id=announcement;msg-param-color=GREEN;system-msg=",
                " :a green announcement",
            ),
            notice(
                "n4",
                "msg-id=announcement;msg-param-color=PRIMARY;system-msg=",
                " :a primary announcement",
            ),
            notice(
                "n5",
                "msg-id=raid;msg-param-viewerCount=5;msg-param-profileImageURL=x;\
                 system-msg=5\\sraiders\\sfrom\\sRosa\\shave\\sjoined!",
                "",
            ),
            notice(
                "n6",
                "msg-id=viewermilestone;msg-param-category=watch-streak;msg-param-value=15;\
                 system-msg=Rosa\\swatched\\s15\\sconsecutive\\sstreams\\sand\\ssparked\\sa\\swatch\\sstreak!",
                " :good evening",
            ),
            notice(
                "n7",
                "msg-id=submysterygift;msg-param-mass-gift-count=5;msg-param-sub-plan=1000;\
                 msg-param-community-gift-id=7;system-msg=Rosa\\sis\\sgifting\\s5\\sTier\\s1\\sSubs!",
                "",
            ),
            // a single gift of the gift above: not shown
            notice(
                "n8",
                &format!("msg-id=subgift;{gift};msg-param-community-gift-id=7;system-msg=hidden"),
                "",
            ),
            notice(
                "n9",
                &format!(
                    "msg-id=subgift;{gift};system-msg=Rosa\\sgifted\\sa\\sTier\\s1\\ssub\\sto\\sA!"
                ),
                "",
            ),
        ];
        for msg in irc_messages {
            chat_box.handle_raw(msg);
        }

        write_preview(&chat_box, "target/chat-box-edge-cases.html", |html| {
            // show the deleted message too
            html.replace(
                "#log>div.deleted {\n    display: none;",
                "#log>div.deleted {\n    opacity: 0.3;",
            )
        })
        .await;
        Ok(())
    }

    /// Serves the chat box of any channel: `CHAT_BOX_CHANNEL` (default
    /// `dunkorslam`) on `CHAT_BOX_BIND` (default `localhost:7458`), until it
    /// is stopped. The backlog is from the chat log index
    /// `twitch-chat-<channel>`, when the archiver archives the channel.
    /// There is no clear, so the backlog is the last messages.
    #[tokio::test]
    #[ignore = "manual test"]
    async fn live_channel() -> Result<()> {
        use crate::{
            config::Config,
            services::{
                chat_log::ChatLogServiceElastic,
                display::DisplayServer,
                twitch::{TwitchService, TwitchServiceImpl, UserIdResponse},
            },
        };

        _ = tracing_subscriber::fmt()
            .with_env_filter("limatoukka=info")
            .try_init();
        let env = |name: &str, default: &str| std::env::var(name).unwrap_or(default.into());
        let login = env("CHAT_BOX_CHANNEL", "dunkorslam");
        let bind = env("CHAT_BOX_BIND", "localhost:7458");

        let config = Config::load()?;
        let twitch_api = TwitchApi::new(&config).await?;
        let chat_log = Arc::new(ChatLogServiceElastic::new(
            &config.elastic.url,
            &config.elastic.api_key,
            &format!("twitch-chat-{login}"),
        )?);
        let id = match TwitchServiceImpl::new(twitch_api.clone())
            .get_user_id(&login)
            .await?
        {
            UserIdResponse::Found(id) => id.take(),
            _ => anyhow::bail!("no channel {login}"),
        };
        let chat_box = Arc::new(ChatBox::create(
            &id,
            &login,
            Some(twitch_api),
            Arc::new(InMemoryStorageService::default()),
            Some(chat_log),
        ));
        let server = Arc::new(DisplayServer::default());
        tokio::spawn(server.start(&bind, chat_box.router()));
        println!("http://{bind}/chat and http://{bind}/chat/last");
        chat_box.run(true).await;
        Ok(())
    }

    #[tokio::test]
    async fn served() -> Result<()> {
        use crate::services::display::{DisplayServer, wait_until_listening};

        let chat_box = chat_box();
        chat_box.handle_irc(privmsg("a", "1", "first"));

        let bind_addr = "localhost:12314";
        let server = Arc::new(DisplayServer::default());
        tokio::spawn(server.start(bind_addr, chat_box.router()));
        wait_until_listening(bind_addr).await;

        let client = reqwest::Client::new();
        let url = format!("http://{bind_addr}/chat");
        let body = client.get(&url).send().await?.text().await?;
        assert!(body.contains("first"));
        assert!(!body.contains(r#"class="last-only""#));
        let body = client
            .get(format!("{url}/last"))
            .send()
            .await?
            .text()
            .await?;
        assert!(body.contains(r#"class="last-only""#));

        let mut events = client
            .get(format!("{url}/events"))
            .send()
            .await?
            .events()
            .await
            .map_err(|e| anyhow!(e))?;
        let event = events.next().await.unwrap().map_err(|e| anyhow!(e))?;
        assert_eq!(event.event_type, "reset");
        assert!(event.data.contains("first"));

        chat_box.handle_irc(privmsg("b", "2", "second"));
        let event = events.next().await.unwrap().map_err(|e| anyhow!(e))?;
        assert_eq!(event.event_type, "message");
        assert!(event.data.contains("second"));
        assert!(!event.data.contains("first"));

        chat_box.clear_shown();
        let event = events.next().await.unwrap().map_err(|e| anyhow!(e))?;
        assert_eq!(event.event_type, "clear");

        // the image route exists; this host is not cached, so it is a 404
        let status = client
            .get(format!("http://{bind_addr}/chat/img/example.com/a.png"))
            .send()
            .await?
            .status();
        assert_eq!(status, reqwest::StatusCode::NOT_FOUND);

        Ok(())
    }
}
