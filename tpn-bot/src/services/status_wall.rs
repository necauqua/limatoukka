use std::sync::Arc;

use anyhow::{Ok, Result};
use axum::{
    Router,
    response::{Sse, sse::Event},
    routing::get,
};
use futures::{
    SinkExt,
    channel::mpsc::{self, UnboundedSender},
};
use futures_util::StreamExt;
use indexmap::IndexMap;
use maud::{PreEscaped, html};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

#[derive(Default, Clone)]
pub struct StatusWall {
    inner: Arc<Mutex<StatusWallInner>>,
}

#[derive(Default)]
struct StatusWallInner {
    entries: IndexMap<EntryKey, String>,
    counter: usize,
    senders: Vec<UnboundedSender<Event>>,
}

impl StatusWallInner {
    fn new_key(&mut self) -> EntryKey {
        self.counter += 1;
        EntryKey(self.counter)
    }

    fn get_text(&self) -> String {
        self.entries
            .iter()
            .fold(String::new(), |acc, (_, entry)| acc + entry + "\n")
    }

    async fn sync(&mut self) {
        let text = self.get_text();
        let mut has_dead = false;
        for sender in &mut self.senders {
            if sender.send(Event::default().data(&text)).await.is_err() {
                has_dead = true;
            }
        }
        if has_dead {
            self.senders.retain(|sender| !sender.is_closed());
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct EntryKey(pub usize);

pub struct EntryGuard(EntryKey, StatusWall);

impl Drop for EntryGuard {
    fn drop(&mut self) {
        let entry = self.0;
        let wall = self.1.clone();
        tokio::spawn(async move { wall.pop(entry).await });
    }
}

impl StatusWall {
    pub async fn allocate(&self) -> EntryKey {
        self.inner.lock().await.new_key()
    }

    pub fn guard(&self, key: EntryKey) -> EntryGuard {
        EntryGuard(key, self.clone())
    }

    pub async fn push(&self, entry: impl Into<String>) -> EntryGuard {
        let mut inner = self.inner.lock().await;
        let id = inner.new_key();
        inner.entries.insert(id, entry.into());
        inner.sync().await;
        EntryGuard(id, self.clone())
    }

    pub async fn push_top(&self, entry: impl Into<String>) -> EntryGuard {
        let mut inner = self.inner.lock().await;
        let id = inner.new_key();
        inner.entries.shift_insert(0, id, entry.into());
        inner.sync().await;
        EntryGuard(id, self.clone())
    }

    pub async fn set(&self, id: EntryKey, new_entry: impl Into<String>) -> Option<String> {
        let mut inner = self.inner.lock().await;
        let old_entry = inner.entries.shift_insert(0, id, new_entry.into());
        inner.sync().await;
        old_entry
    }

    pub async fn pop(&self, id: EntryKey) {
        let mut inner = self.inner.lock().await;
        inner.entries.shift_remove(&id);
        inner.sync().await;
    }

    pub fn start(&self, bind_addr: &str) -> impl Future<Output = Result<()>> + use<> {
        let app = Router::new()
            .route(
                "/",
                get({
                    let inner = self.inner.clone();
                    || async move {
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
                                (PreEscaped(inner.lock().await.get_text()))
                            };
                        }
                    }
                }),
            )
            .route(
                "/events",
                get({
                    let inner = self.inner.clone();
                    || async move {
                        let (tx, rx) = mpsc::unbounded();
                        inner.lock().await.senders.push(tx);
                        Sse::new(rx.map(Ok)).keep_alive(Default::default())
                    }
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
