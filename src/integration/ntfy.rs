use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::broadcast::{Receiver, Sender};

use crate::{
    config::Ntfy,
    integration::websocket::{MessageProcessor, MessageResult, WebSocketConnection},
};

pub struct NtfyTopic {
    url: String,
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
    pub fn new(config: &Ntfy, topic: &str) -> Self {
        let host = &config.host;
        let config = config.topic.get(topic).cloned().unwrap_or_default();
        let mut url = format!("wss://{host}/{topic}/ws");
        if let Some(auth) = config.auth {
            url.push_str("?auth=");
            url.push_str(&auth);
        }
        Self {
            url,
            events: Sender::new(16),
        }
    }

    pub fn subscribe(&self) -> Receiver<Arc<str>> {
        self.events.subscribe()
    }

    pub async fn run(self) -> ! {
        WebSocketConnection::new(self.url.clone(), self).run().await
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
        let kofi = NtfyTopic::new(&config.ntfy, "kofi");
        let mut kofi_events = kofi.subscribe();
        tokio::spawn(kofi.run());

        loop {
            let event = KofiPayload::parse_payload(&kofi_events.recv().await?)?;
            println!("kofi event: {event:#?}");
        }
    }
}
