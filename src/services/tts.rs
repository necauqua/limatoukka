use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::{Mutex, oneshot::Receiver};

use crate::integration::justfile::just;

#[async_trait]
pub trait TtsService: Send + Sync {
    async fn tts(&self, text: &str, stop: Option<Receiver<()>>) -> Result<()>;
}

#[derive(Default)]
pub struct TtsServiceImpl {
    serialize: Mutex<()>,
}

#[async_trait]
impl TtsService for TtsServiceImpl {
    async fn tts(&self, text: &str, stop: Option<Receiver<()>>) -> Result<()> {
        let _guard = self.serialize.lock().await;

        tracing::debug!(text, "sending TTS");

        let mut process = just("aws-tts", &[text])?;
        if process.wait(stop).await {
            tracing::debug!(text, "finished TTS")
        } else {
            tracing::debug!(text, "interrupted TTS");
        }
        Ok(())
    }
}
