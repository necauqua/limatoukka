use std::sync::Arc;

use anyhow::{Ok, Result};
use axum::{
    Router,
    extract::{Path, ws::WebSocketUpgrade},
    routing::get,
};
use dashmap::{DashMap, mapref::entry::Entry};
use maud::{Markup, PreEscaped, Render, html};
use tokio::sync::broadcast::Sender;

use crate::{
    injector_getter,
    services::{
        Service,
        live::{self, LiveEvent},
    },
};

pub trait DisplayService: Service {
    fn set(&self, key: &str, html: Markup);
}

injector_getter!(DisplayService::display { DisplayServiceNoop });

#[derive(Default)]
pub struct DisplayServer {
    sources: DashMap<String, (Markup, Sender<LiveEvent>)>,
}

impl DisplayServer {
    fn html(&self, key: &str) -> Markup {
        self.sources
            .get(key)
            .map(|entry| entry.0.clone())
            .unwrap_or_default()
    }

    fn sender(&self, key: &str) -> Sender<LiveEvent> {
        self.sources
            .entry(key.to_owned())
            .or_insert_with(|| (html! {}, Sender::new(16)))
            .1
            .clone()
    }

    /// Serves the display pages `/{key}` (each with its live updates at
    /// `/{key}/events`) and the other `routes`.
    pub fn start(
        self: &Arc<Self>,
        bind_addr: &str,
        routes: Router,
    ) -> impl Future<Output = Result<()>> + use<> {
        let app = routes
            .route(
                "/{key}",
                get({
                    let handle = self.clone();
                    async move |Path(key): Path<String>| {
                        html! {
                            (live::script())
                            script {
                                (PreEscaped("live(location.pathname + '/events', { html: data => text.innerHTML = data });"))
                            }
                            div #text style="
                                position: absolute;
                                inset: 0;
                                white-space: pre;
                                color: white;
                                font-family: NoitaPixel;
                                font-smooth: never;
                            " {
                                (handle.html(&key))
                            };
                        }
                    }
                }),
            )
            .route(
                "/{key}/events",
                get({
                    let handle = self.clone();
                    async move |Path(key): Path<String>, ws: WebSocketUpgrade| {
                        let rx = handle.sender(&key).subscribe();
                        let handle = handle.clone();
                        let snapshot = move || {
                            let html = handle.html(&key);
                            async move { Some(LiveEvent::new("html", &html.0)) }
                        };
                        live::serve(ws, live::with_snapshot(rx, snapshot, Some))
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

pub struct DisplayHandle {
    service: Arc<dyn DisplayService>,
    key: String,
}

impl DisplayHandle {
    pub fn render(&self, html: impl Render) {
        self.service.set(&self.key, html.render())
    }
}

pub trait DisplayServiceAux {
    fn wrap(&self, key: impl Into<String>) -> DisplayHandle;

    fn render(&self, key: impl Into<String>, html: impl Render) {
        self.wrap(key).render(html)
    }
}

impl<T: DisplayService> DisplayServiceAux for Arc<T> {
    fn wrap(&self, key: impl Into<String>) -> DisplayHandle {
        DisplayHandle {
            service: self.clone(),
            key: key.into(),
        }
    }
}

impl DisplayServiceAux for Arc<dyn DisplayService> {
    fn wrap(&self, key: impl Into<String>) -> DisplayHandle {
        DisplayHandle {
            service: self.clone(),
            key: key.into(),
        }
    }
}

impl DisplayService for DisplayServer {
    fn set(&self, key: &str, html: Markup) {
        let event = LiveEvent::new("html", &html.0);
        match self.sources.entry(key.to_owned()) {
            Entry::Occupied(mut o) => {
                o.get_mut().0 = html.clone();
                let _ = o.get().1.send(event);
            }
            Entry::Vacant(v) => {
                let _ = v.insert((html.clone(), Sender::new(16))).1.send(event);
            }
        };
    }
}

pub struct DisplayServiceNoop;

impl DisplayService for DisplayServiceNoop {
    fn set(&self, _key: &str, _html: Markup) {}
}

/// Waits until a server that was spawned on `addr` accepts connections.
#[cfg(test)]
pub(crate) async fn wait_until_listening(addr: &str) {
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("nothing listens on {addr}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::live::TestClient;

    #[tokio::test]
    async fn display_server() -> Result<()> {
        let server = Arc::new(DisplayServer::default());
        let bind_addr = "localhost:12312";

        tokio::spawn(server.start(bind_addr, Router::new()));
        wait_until_listening(bind_addr).await;

        let client = reqwest::Client::new();
        let key = "my-service";

        let url = format!("http://{bind_addr}/{key}");
        server.set(key, html! { "before" });
        let mut events = TestClient::connect(bind_addr, &format!("/{key}/events")).await?;
        // the current HTML first
        let (event, data) = events.next().await?;
        assert_eq!(event, "html");
        assert!(data.contains("before"));

        server.set(key, html! { "Hello, world!" });
        let (event, data) = events.next().await?;
        assert_eq!(event, "html");
        assert!(data.contains("Hello, world!"));
        let body = client.get(&url).send().await?.text().await?;
        assert!(body.contains("Hello, world!"));

        server.set(key, html! { "second" });
        let (_, data) = events.next().await?;
        assert!(data.contains("second"));
        let body = client.get(&url).send().await?.text().await?;
        assert!(body.contains("second"));

        Ok(())
    }
}
