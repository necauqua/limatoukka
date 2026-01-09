use std::sync::Arc;

use anyhow::{Ok, Result};
use async_trait::async_trait;
use axum::{
    Router,
    extract::Path,
    response::{Sse, sse::Event},
    routing::get,
};
use dashmap::{DashMap, mapref::entry::Entry};
use maud::{Markup, PreEscaped, html};
use tokio::sync::broadcast::Sender;

use crate::{injector_getter, services::Service};

pub trait DisplayService: Service {
    fn set(&self, key: &str, html: Markup);
}

injector_getter!(DisplayService::display { DisplayServiceNoop });

#[derive(Default)]
pub struct DisplayServer {
    sources: DashMap<String, (Markup, Sender<Event>)>,
}

impl DisplayServer {
    fn html(&self, key: &str) -> Markup {
        self.sources
            .get(key)
            .map(|entry| entry.0.clone())
            .unwrap_or_default()
    }

    fn sender(&self, key: &str) -> Sender<Event> {
        self.sources
            .entry(key.to_owned())
            .or_insert_with(|| (html! {}, Sender::new(16)))
            .1
            .clone()
    }

    pub fn start(self: &Arc<Self>, bind_addr: &str) -> impl Future<Output = Result<()>> + use<> {
        let app = Router::new()
            .route(
                "/{key}",
                get({
                    let handle = self.clone();
                    async move |Path(key): Path<String>| {
                        html! {
                            script {
                                (PreEscaped(format!(r#"new EventSource("/{key}/events").onmessage = (e) => text.innerHTML = e.data"#)))
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
                    async move |Path(key): Path<String>| {
                    let rx = handle.sender(&key).subscribe();
                    let s = futures::stream::try_unfold(rx, |mut rx| async {
                        Ok(Some((rx.recv().await?, rx)))
                    });
                    Sse::new(s).keep_alive(Default::default())
                }}),
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
    pub fn set(&self, html: Markup) {
        self.service.set(&self.key, html)
    }
}

pub trait DisplayServiceWrap {
    fn wrap(&self, key: impl Into<String>) -> DisplayHandle;
}

impl<T: DisplayService> DisplayServiceWrap for Arc<T> {
    fn wrap(&self, key: impl Into<String>) -> DisplayHandle {
        DisplayHandle {
            service: self.clone(),
            key: key.into(),
        }
    }
}

impl DisplayService for DisplayServer {
    fn set(&self, key: &str, html: Markup) {
        let event = Event::default().data(match &*html.0 {
            "" => " ".to_owned(),
            x => x.into(),
        });
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

#[async_trait]
impl DisplayService for DisplayServiceNoop {
    fn set(&self, _key: &str, _html: Markup) {}
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use futures::StreamExt;
    use reqwest_sse::EventSource;

    use super::*;

    #[tokio::test]
    async fn display_server() -> Result<()> {
        let server = Arc::new(DisplayServer::default());
        let bind_addr = "localhost:12312";

        tokio::spawn(server.start(bind_addr));

        let client = reqwest::Client::new();
        let key = "my-service";

        let url = format!("http://{bind_addr}/{key}");
        let mut events = client
            .get(format!("{url}/events"))
            .send()
            .await?
            .events()
            .await
            .map_err(|e| anyhow!(e))?;

        server.set(key, html! { "Hello, world!" });
        let event = events.next().await.unwrap().map_err(|e| anyhow!(e))?;
        assert!(event.data.contains("Hello, world!"));
        let body = client.get(&url).send().await?.text().await?;
        assert!(body.contains("Hello, world!"));

        server.set(key, html! { "second" });
        let event = events.next().await.unwrap().map_err(|e| anyhow!(e))?;
        assert!(event.data.contains("second"));
        let body = client.get(&url).send().await?.text().await?;
        assert!(body.contains("second"));

        Ok(())
    }
}
