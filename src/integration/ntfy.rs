use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;
use tokio::sync::broadcast::{Receiver, Sender};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    handshake::client::Request,
    http::{HeaderValue, header::AUTHORIZATION},
};

use crate::{
    config::Ntfy,
    integration::websocket::{MessageProcessor, MessageResult, WebSocketConnection},
};

pub struct NtfyTopic {
    connect_request: Request,
    events: Sender<Arc<str>>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum NtfyEvent {
    Open,
    Keepalive,
    Message { message: String },
    PollRequest,
}

// a subset of ntfy event fields
#[derive(Debug, Deserialize)]
pub struct NtfyPayload {
    pub id: String,
    pub topic: String,
    #[serde(flatten)]
    pub event: NtfyEvent,
}

impl MessageProcessor for NtfyTopic {
    async fn process_text(
        conn: &mut WebSocketConnection<Self>,
        message: &str,
    ) -> anyhow::Result<MessageResult> {
        let payload = serde_json::from_str::<NtfyPayload>(message)?;
        tracing::debug!(?payload, "ntfy message");
        if let NtfyEvent::Message { message } = payload.event {
            conn.processor.events.send(message.into())?;
        }
        Ok(MessageResult::Ok)
    }
}

impl NtfyTopic {
    pub fn new(config: &Ntfy, topic: &str) -> Result<Self> {
        let host = &config.host;
        let topic_config = config.topic.get(topic).cloned().unwrap_or_default();

        let mut connect_request = format!("wss://{host}/{topic}/ws").into_client_request()?;

        if let Some(token) = topic_config.token {
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))?;
            value.set_sensitive(true);
            connect_request.headers_mut().insert(AUTHORIZATION, value);
        }

        Ok(Self {
            connect_request,
            events: Sender::new(16),
        })
    }

    pub fn subscribe(&self) -> Receiver<Arc<str>> {
        self.events.subscribe()
    }

    pub async fn run(self) -> ! {
        let connect_request = self.connect_request.clone();
        WebSocketConnection::new(connect_request, self).run().await
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use crate::{config::Config, integration::kofi::KofiPayload};

    use super::*;

    #[tokio::test]
    #[ignore = "manual test"]
    async fn kofi() -> Result<()> {
        let config = Config::load()?;
        let kofi = NtfyTopic::new(&config.ntfy, "kofi")?;
        let mut kofi_events = kofi.subscribe();
        tokio::spawn(kofi.run());

        loop {
            let event = KofiPayload::parse_payload(&kofi_events.recv().await?)?;
            println!("kofi event: {event:#?}");
        }
    }
}
