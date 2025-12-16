use std::{pin::Pin, time::Duration};

use futures::StreamExt;
use tokio::{net::TcpStream, time::Sleep};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{
        self, client::IntoClientRequest, handshake::client::Request, protocol::CloseFrame,
    },
};

type WebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub(crate) struct WebSocketConnection<P> {
    pub connect_request: Request,
    pub canary_timeout: Option<Duration>,
    pub processor: P,
    canary: Option<Pin<Box<Sleep>>>,
}

pub enum MessageResult {
    Ok,
    Reconnect,
}

pub(crate) trait MessageProcessor: Sized {
    async fn process_text(
        _conn: &mut WebSocketConnection<Self>,
        _message: &str,
    ) -> anyhow::Result<MessageResult> {
        Ok(MessageResult::Ok)
    }
    async fn process_binary(
        _conn: &mut WebSocketConnection<Self>,
        _message: &[u8],
    ) -> anyhow::Result<MessageResult> {
        Ok(MessageResult::Ok)
    }
}

impl<P: MessageProcessor> WebSocketConnection<P> {
    pub fn new(connect_request: impl IntoClientRequest, processor: P) -> Self {
        Self {
            connect_request: connect_request.into_client_request().unwrap(),
            canary_timeout: None,
            processor,
            canary: None,
        }
    }

    pub fn keep_alive(&mut self) {
        self.canary = self
            .canary_timeout
            .map(|timeout| Box::pin(tokio::time::sleep(timeout)))
    }

    async fn connect(&self) -> Result<WebSocket, tungstenite::Error> {
        tracing::info!("websocket connect");
        let (stream, _) = tokio_tungstenite::connect_async(self.connect_request.clone()).await?;
        Ok(stream)
    }

    async fn process_message(
        &mut self,
        msg: tungstenite::Message,
    ) -> anyhow::Result<Result<MessageResult, Option<CloseFrame>>> {
        match msg {
            tungstenite::Message::Text(text) => P::process_text(self, &text).await.map(Ok),
            tungstenite::Message::Binary(bin) => P::process_binary(self, &bin).await.map(Ok),
            tungstenite::Message::Close(frame) => Ok(Err(frame)),
            _ => Ok(Ok(MessageResult::Ok)),
        }
    }

    async fn run_iteration(&mut self) -> anyhow::Result<()> {
        let mut prev: Option<WebSocket> = None;
        let mut stream = self.connect().await?;

        loop {
            tokio::select! {
                Some(msg) = async {
                    match prev {
                        Some(ref mut prev) => prev.next().await,
                        None => None,
                    }
                } => match msg {
                    Ok(msg) => {
                        _ = self.process_message(msg).await?;
                    },
                    Err(e) => {
                        tracing::warn!(error=?e, "prev websocket error");
                        prev = None;
                    },
                },
                Some(msg) = stream.next() => {
                    self.keep_alive();

                    let prev_keepalive = self.canary_timeout;

                    match self.process_message(msg?).await? {
                        Ok(MessageResult::Ok) => {},
                        Ok(MessageResult::Reconnect) => {
                            tracing::info!("reconnect request requested");
                            prev = Some(std::mem::replace(&mut stream, self.connect().await?));
                        },
                        Err(frame) => {
                            tracing::warn!(?frame, "closed");
                            return Ok(())
                        },
                    }

                    // reset it in case it was changed
                    if self.canary_timeout != prev_keepalive {
                        self.keep_alive();
                    }
                }
                true = async {
                    match self.canary {
                        Some(ref mut canary) => {
                            canary.await;
                            true
                        },
                        None => false,
                    }
                } => {
                    tracing::warn!("websocket timeout, reconnecting");
                    prev = Some(std::mem::replace(&mut stream, self.connect().await?));
                }
                // disconnected
                else => return Ok(()),
            }
        }
    }

    pub async fn run(mut self) -> ! {
        self.keep_alive();
        loop {
            if let Err(e) = self.run_iteration().await {
                tracing::warn!(error=?e, "websocket fail");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}
