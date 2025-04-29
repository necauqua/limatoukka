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

#[derive(Default)]
struct Inner {
    entries: IndexMap<EntryKey, String>,
    counter: usize,
    senders: Vec<UnboundedSender<Event>>,
}

impl Inner {
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

#[derive(Default, Clone)]
pub struct StatusWall {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct EntryKey(pub usize);

pub struct EntryGuard(EntryKey, StatusWall);

impl EntryGuard {
    pub async fn set(&self, new_entry: impl Into<String>) -> Option<String> {
        self.1
            .update(|inner| inner.entries.insert(self.0, new_entry.into()))
            .await
    }

    pub async fn set_top(&self, new_entry: impl Into<String>) -> Option<String> {
        self.1.set_top(self.0, new_entry).await
    }

    pub fn key(&self) -> EntryKey {
        self.0
    }
}

impl Drop for EntryGuard {
    fn drop(&mut self) {
        let entry = self.0;
        let wall = self.1.clone();
        tokio::spawn(async move {
            wall.update(|inner| inner.entries.shift_remove(&entry))
                .await
        });
    }
}

impl StatusWall {
    pub async fn allocate(&self) -> EntryGuard {
        let key = self.inner.lock().await.new_key();
        EntryGuard(key, self.clone())
    }

    async fn update<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        let mut inner = self.inner.lock().await;
        let r = f(&mut inner);
        inner.sync().await;
        r
    }

    pub async fn push(&self, entry: impl Into<String>) -> EntryGuard {
        self.update(|inner| {
            let id = inner.new_key();
            inner.entries.insert(id, entry.into());
            EntryGuard(id, self.clone())
        })
        .await
    }

    pub async fn push_top(&self, entry: impl Into<String>) -> EntryGuard {
        self.update(|inner| {
            let id = inner.new_key();
            inner.entries.shift_insert(0, id, entry.into());
            EntryGuard(id, self.clone())
        })
        .await
    }

    pub async fn set_top(&self, id: EntryKey, new_entry: impl Into<String>) -> Option<String> {
        self.update(|inner| inner.entries.shift_insert(0, id, new_entry.into()))
            .await
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
