use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use anyhow::{Ok, Result};
use async_trait::async_trait;
use axum::{
    Router,
    response::{Sse, sse::Event},
    routing::get,
};
use indexmap::IndexMap;
use maud::{PreEscaped, html};
use serde::{Deserialize, Serialize};
use tokio::sync::{
    Mutex,
    broadcast::{self, Sender},
};

use crate::{injector_getter, services::Service};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct EntryKey(usize);

#[async_trait]
pub trait StatusService: Service {
    fn new_key(&self) -> EntryKey;

    async fn set(&self, key: EntryKey, text: String) -> Option<String>;

    async fn set_and_bump(&self, key: EntryKey, text: String) -> Option<String>;

    async fn remove(&self, key: EntryKey) -> Option<String>;
}

injector_getter!(StatusService::status);

pub struct EntryGuard {
    key: EntryKey,
    wall: Arc<dyn StatusService>,
}

impl EntryGuard {
    pub fn key(&self) -> EntryKey {
        self.key
    }

    pub async fn set(&self, text: String) -> Option<String> {
        self.wall.set(self.key, text).await
    }

    pub async fn set_and_bump(&self, text: String) -> Option<String> {
        self.wall.set_and_bump(self.key, text).await
    }
}

impl Drop for EntryGuard {
    fn drop(&mut self) {
        let key = self.key;
        let wall = self.wall.clone();
        tokio::spawn(async move { wall.remove(key).await });
    }
}

impl dyn StatusService {
    pub async fn allocate(self: &Arc<Self>) -> EntryGuard {
        EntryGuard {
            key: self.new_key(),
            wall: self.clone(),
        }
    }

    pub async fn push(self: &Arc<Self>, text: impl Into<String>) -> EntryGuard {
        let entry = self.allocate().await;
        entry.set(text.into()).await;
        entry
    }

    pub async fn push_top(self: &Arc<Self>, text: impl Into<String>) -> EntryGuard {
        let entry = self.allocate().await;
        entry.set_and_bump(text.into()).await;
        entry
    }
}

pub struct StatusWall {
    entries: Mutex<IndexMap<EntryKey, String>>,
    broadcast: Sender<Event>,
    counter: AtomicUsize,
}

impl Default for StatusWall {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(16);
        Self {
            entries: Default::default(),
            broadcast: tx,
            counter: AtomicUsize::new(0),
        }
    }
}

impl StatusWall {
    async fn get_text(&self) -> String {
        self.entries
            .lock()
            .await
            .iter()
            .fold(String::new(), |acc, (_, entry)| acc + entry + "\n")
    }

    async fn update<R>(&self, f: impl FnOnce(&mut IndexMap<EntryKey, String>) -> R) -> R {
        let (r, text) = {
            let mut entries = self.entries.lock().await;
            let r = f(&mut entries);
            let text = entries
                .iter()
                .fold(String::new(), |acc, (_, entry)| acc + entry + "\n");
            (r, text)
        };
        _ = self.broadcast.send(Event::default().data(text));
        r
    }

    pub fn start(self: Arc<Self>, bind_addr: &str) -> impl Future<Output = Result<()>> + use<> {
        let app = Router::new()
            .route(
                "/",
                get({
                    let handle = self.clone();
                    async move || {
                        html! {
                            script {
                                (PreEscaped(r#"new EventSource("/events").onmessage = (e) => text.innerHTML = e.data"#))
                            }
                            div #text style="
                                position: absolute;
                                inset: 0;
                                white-space: pre;
                                text-align: right;
                                color: white;
                                font-family: NoitaPixel;
                                font-smooth: never;
                            " {
                                (PreEscaped(handle.get_text().await))
                            };
                        }
                    }
                }),
            )
            .route(
                "/events",
                get(async move || {
                    let rx = self.broadcast.subscribe();
                    let s = futures::stream::try_unfold(rx, |mut rx| async {
                        Ok(Some((rx.recv().await?, rx)))
                    });
                    Sse::new(s).keep_alive(Default::default())
                }),
            );

        let bind_addr = bind_addr.to_owned(); // meh
        async move {
            let listener = tokio::net::TcpListener::bind(bind_addr).await?;
            tracing::info!("listening on {}", listener.local_addr()?);
            axum::serve(listener, app).await?;

            Ok(())
        }
    }
}

#[async_trait]
impl StatusService for StatusWall {
    fn new_key(&self) -> EntryKey {
        EntryKey(self.counter.fetch_add(1, Ordering::Relaxed))
    }

    async fn set(&self, key: EntryKey, text: String) -> Option<String> {
        self.update(|entries| entries.insert(key, text)).await
    }

    async fn set_and_bump(&self, key: EntryKey, text: String) -> Option<String> {
        self.update(|entries| entries.shift_insert(0, key, text))
            .await
    }

    async fn remove(&self, key: EntryKey) -> Option<String> {
        self.update(|entries| entries.shift_remove(&key)).await
    }
}

#[derive(Default)]
pub struct TestStatusWall {
    counter: AtomicUsize,
}

#[async_trait]
impl StatusService for TestStatusWall {
    fn new_key(&self) -> EntryKey {
        EntryKey(self.counter.fetch_add(1, Ordering::Relaxed))
    }

    async fn set(&self, key: EntryKey, text: String) -> Option<String> {
        tracing::info!(key = key.0, %text, "set status");
        None
    }

    async fn set_and_bump(&self, key: EntryKey, text: String) -> Option<String> {
        tracing::info!(key = key.0, %text, "bump status");
        None
    }

    async fn remove(&self, key: EntryKey) -> Option<String> {
        tracing::info!(key = key.0, "remove status");
        None
    }
}
