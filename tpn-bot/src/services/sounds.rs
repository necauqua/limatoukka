use std::collections::HashMap;

use anyhow::{Result, anyhow};
use rand::seq::IndexedRandom;
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::Mutex;

use crate::context::app::AppContext;

#[derive(Default)]
pub struct Sounds {
    exclusive_sound: Mutex<()>,
}

#[derive(Debug, Deserialize)]
pub struct SoundMeta(pub HashMap<String, SoundEntry>);

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SoundEntry {
    Single(SoundFile),
    Multiple(Vec<SoundFile>),
}

impl SoundEntry {
    pub fn choose(&self) -> Option<&SoundFile> {
        match self {
            SoundEntry::Single(file) => Some(file),
            SoundEntry::Multiple(files) => {
                let known_rarity_sum: f32 = files.iter().filter_map(|f| f.rarity).sum();
                let default_rarity_count = files.iter().filter(|f| f.rarity.is_none()).count();
                let default_rarity = if default_rarity_count > 0 {
                    (1.0 - known_rarity_sum) / default_rarity_count as f32
                } else {
                    0.0
                };
                files
                    .choose_weighted(&mut rand::rng(), |f| f.rarity.unwrap_or(default_rarity))
                    .ok()
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SoundFile {
    pub file: String,
    #[serde(default)]
    pub volume: Option<f32>,
    #[serde(default)]
    pub rarity: Option<f32>,
    #[serde(default)]
    pub exclusive: bool,
}

#[derive(Debug, Error)]
pub enum SoundError {
    #[error("Sound not found")]
    NotFound,
    #[error(
        "This sound allows randomly choosing to not play anything, and you failed the dice roll ¯\\_(ツ)_/¯"
    )]
    DidntChoose,
    #[error("Internal error: {0}")]
    InternalError(#[from] anyhow::Error),
}

impl Sounds {
    pub async fn tts(&self, text: &str, interrupt_signal: impl Future) -> Result<()> {
        tracing::debug!(text, "sending TTS");

        let mut process = AppContext::just("aws-tts", &[text])?;
        if process.wait(interrupt_signal).await {
            tracing::debug!(text, "finished TTS")
        } else {
            tracing::debug!(text, "interrupted TTS");
        }
        Ok(())
    }

    pub async fn play_sound(
        &self,
        sound_id: &str,
        interrupt_signal: impl Future,
    ) -> Result<(), SoundError> {
        // just read it every time for runtime editing (like with justfile)
        let data: SoundMeta = serde_yml::from_str(
            &std::fs::read_to_string("./sounds/_meta.yml").map_err(|e| anyhow!(e))?,
        )
        .map_err(|e| anyhow!(e))?;

        let sound = data.0.get(sound_id).ok_or(SoundError::NotFound)?;
        let sound_file = sound.choose().ok_or(SoundError::DidntChoose)?;

        let _guard = if sound_file.exclusive {
            Some(self.exclusive_sound.lock().await)
        } else {
            None
        };

        let volume = sound_file.volume.unwrap_or(1.0);

        tracing::debug!(sound_id, "playing a sound");
        let mut process = AppContext::just(
            "play-sound",
            &[&sound_file.file, volume.to_string().as_str()],
        )?;

        if process.wait(interrupt_signal).await {
            tracing::debug!(sound_id, "finished playing sound");
        } else {
            tracing::debug!(sound_id, "interrupted sound playback");
        }

        Ok(())
    }
}
