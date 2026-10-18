//! The live updates of the browser source pages, over a WebSocket.
//!
//! Each WebSocket message is a JSON object `{"event": ..., "data": ...}`.
//! The pages connect with `live(path, handlers)` from `live.js` (see
//! [`script`]); it connects again when the connection ends.
//!
//! A WebSocket, not SSE: Chromium (also the CEF of OBS) has a maximum of 6
//! HTTP/1.1 connections to one host, for all the browser sources together.
//! Each SSE stream holds one of them, so with 6 pages the other requests
//! (e.g. the images of the chat box) wait forever. WebSockets are not in
//! this limit.

use std::pin::pin;

use axum::{
    extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade},
    response::Response,
};
use futures::{SinkExt as _, Stream, StreamExt as _};
use maud::{Markup, PreEscaped, html};
use serde_json::json;
use tokio::sync::broadcast::{self, error::RecvError};

/// One message to a page; the JSON is made once for all the pages.
#[derive(Debug, Clone)]
pub struct LiveEvent(Utf8Bytes);

impl LiveEvent {
    pub fn new(event: &str, data: &str) -> Self {
        Self(json!({ "event": event, "data": data }).to_string().into())
    }
}

/// The script that defines `live(path, handlers)`.
pub fn script() -> Markup {
    html! { script { (PreEscaped(include_str!("live.js"))) } }
}

/// The events of a page: first `snapshot()`, the full state, so that a
/// (re)connected page is correct; then `event(update)` for each update.
/// When `event` gives `None`, or when the page lags behind the updates, the
/// page gets `snapshot()` again. The events end when `snapshot` gives
/// `None` (then the page connects again), or when the updates end.
pub fn with_snapshot<T, S, F, E>(
    updates: broadcast::Receiver<T>,
    snapshot: S,
    event: E,
) -> impl Stream<Item = LiveEvent> + Send + 'static
where
    T: Clone + Send + 'static,
    S: Fn() -> F + Send + 'static,
    F: Future<Output = Option<LiveEvent>> + Send,
    E: Fn(T) -> Option<LiveEvent> + Send + 'static,
{
    struct State<T, S, E> {
        updates: broadcast::Receiver<T>,
        snapshot: S,
        event: E,
        first: bool,
    }
    let state = State {
        updates,
        snapshot,
        event,
        first: true,
    };
    futures::stream::unfold(state, async |mut state| {
        let event = if std::mem::take(&mut state.first) {
            None
        } else {
            match state.updates.recv().await {
                Ok(update) => (state.event)(update),
                Err(RecvError::Lagged(_)) => None,
                Err(RecvError::Closed) => return None,
            }
        };
        let event = match event {
            Some(event) => event,
            None => (state.snapshot)().await?,
        };
        Some((event, state))
    })
}

/// Upgrades the request to a WebSocket and sends the `events` to it. When
/// the stream ends, the WebSocket closes and the page connects again.
pub fn serve(
    ws: WebSocketUpgrade,
    events: impl Stream<Item = LiveEvent> + Send + 'static,
) -> Response {
    ws.on_upgrade(|socket| send(socket, events))
}

async fn send(socket: WebSocket, events: impl Stream<Item = LiveEvent>) {
    let (mut tx, mut rx) = socket.split();
    let mut events = pin!(events);
    loop {
        tokio::select! {
            event = events.next() => {
                let Some(LiveEvent(text)) = event else {
                    _ = tx.send(Message::Close(None)).await;
                    return;
                };
                if tx.send(Message::Text(text)).await.is_err() {
                    return;
                }
            }
            // the pages send nothing; this only sees the end of the
            // connection
            message = rx.next() => match message {
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

/// A WebSocket client of [`serve`] for the tests.
#[cfg(test)]
pub(crate) struct TestClient(
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
);

#[cfg(test)]
impl TestClient {
    /// Connects to `ws://{addr}{path}`.
    pub async fn connect(addr: &str, path: &str) -> anyhow::Result<Self> {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}{path}")).await?;
        Ok(Self(ws))
    }

    /// The next (event, data).
    pub async fn next(&mut self) -> anyhow::Result<(String, String)> {
        use anyhow::Context as _;
        use tokio_tungstenite::tungstenite::Message;

        loop {
            match self.0.next().await.context("the WebSocket closed")?? {
                Message::Text(text) => {
                    let value: serde_json::Value = serde_json::from_str(&text)?;
                    let field = |name: &str| value[name].as_str().map(str::to_owned);
                    return Ok((
                        field("event").context("no event")?,
                        field("data").context("no data")?,
                    ));
                }
                Message::Close(_) => anyhow::bail!("the WebSocket closed"),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::{Router, routing::get};

    use super::*;
    use crate::services::display::{DisplayServer, wait_until_listening};

    #[tokio::test]
    async fn snapshot_then_updates() {
        let (tx, rx) = broadcast::channel(2);
        let events = with_snapshot(
            rx,
            || async { Some(LiveEvent::new("snapshot", "")) },
            |update: &'static str| (!update.is_empty()).then(|| LiveEvent::new(update, "")),
        );
        let mut events = Box::pin(events);
        let mut next = async || events.next().await.map(|LiveEvent(text)| text.to_string());
        let json = |event: &str| LiveEvent::new(event, "").0.to_string();
        assert_eq!(next().await, Some(json("snapshot")));
        tx.send("a").unwrap();
        assert_eq!(next().await, Some(json("a")));
        // no event of its own
        tx.send("").unwrap();
        assert_eq!(next().await, Some(json("snapshot")));
        // lagged behind
        for update in ["b", "c", "d"] {
            tx.send(update).unwrap();
        }
        assert_eq!(next().await, Some(json("snapshot")));
        assert_eq!(next().await, Some(json("c")));
        assert_eq!(next().await, Some(json("d")));
        drop(tx);
        assert_eq!(next().await, None);
    }

    #[tokio::test]
    async fn events_then_close() -> anyhow::Result<()> {
        let routes = Router::new().route(
            "/events",
            get(async |ws: WebSocketUpgrade| {
                let events = ["a", "b"].map(|data| LiveEvent::new("x", data));
                serve(ws, futures::stream::iter(events))
            }),
        );
        let bind_addr = "localhost:12315";
        tokio::spawn(std::sync::Arc::new(DisplayServer::default()).start(bind_addr, routes));
        wait_until_listening(bind_addr).await;

        let mut client = TestClient::connect(bind_addr, "/events").await?;
        assert_eq!(client.next().await?, ("x".into(), "a".into()));
        assert_eq!(client.next().await?, ("x".into(), "b".into()));
        assert!(client.next().await.is_err());
        Ok(())
    }
}
