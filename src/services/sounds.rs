use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use rand::seq::IndexedRandom;
use serde::Deserialize;
use tokio::sync::{Mutex, oneshot::Receiver};
use tracing::{Span, field::Empty};

use crate::{injector_getter, integration::justfile::just, services::Service};

#[async_trait]
pub trait SoundService: Service {
    async fn select(&self, sound_id: &str) -> Result<Option<SoundEntry>>;

    async fn play(&self, sound: &SoundVariant, stop: Option<Receiver<()>>) -> Result<()>;
}

injector_getter!(SoundService::sounds);

impl dyn SoundService {
    pub async fn play_builtin(&self, id: &str) -> Result<()> {
        let sound = self
            .select(id)
            .await?
            .with_context(|| format!("'{id}' sound not found"))?;
        let sound_variant = sound
            .choose()
            .with_context(|| format!("'{id}' sound did not choose a variant"))?;
        self.play(sound_variant, None).await
    }
}

#[derive(Default)]
pub struct SoundServiceImpl {
    exclusive_sound: Mutex<()>,
}

#[derive(Debug, Deserialize)]
pub struct SoundMeta(pub HashMap<String, SoundEntry>);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SoundEntry {
    #[serde(default, with = "humantime_serde")]
    pub global_gate: Option<Duration>,
    #[serde(default, with = "humantime_serde")]
    pub sender_gate: Option<Duration>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub cost: Option<u64>,
    #[serde(flatten)]
    pub variants: SoundVariants,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SoundVariants {
    Single(SoundVariant),
    Multiple { variants: Vec<SoundVariant> },
}

#[derive(Debug, Clone, Deserialize)]
pub struct SoundVariant {
    pub file: String,
    #[serde(default)]
    pub volume: Option<f32>,
    #[serde(default)]
    pub reward: i64,
    #[serde(default)]
    pub rarity: Option<f32>,
    #[serde(default)]
    pub exclusive: bool,
}

impl SoundEntry {
    pub fn choose(&self) -> Option<&SoundVariant> {
        match &self.variants {
            SoundVariants::Single(variant) => Some(variant),
            SoundVariants::Multiple { variants } => {
                let known_rarity_sum: f32 = variants.iter().filter_map(|f| f.rarity).sum();
                let default_rarity_count = variants.iter().filter(|f| f.rarity.is_none()).count();
                let default_rarity = if default_rarity_count > 0 {
                    (1.0 - known_rarity_sum) / default_rarity_count as f32
                } else {
                    0.0
                };
                variants
                    .choose_weighted(&mut rand::rng(), |f| f.rarity.unwrap_or(default_rarity))
                    .ok()
            }
        }
    }
}

#[async_trait]
impl SoundService for SoundServiceImpl {
    async fn select(&self, sound_id: &str) -> Result<Option<SoundEntry>> {
        // just read it every time for runtime editing (like with justfile)
        let mut data: SoundMeta = serde_yml::from_str(
            &std::fs::read_to_string("./sounds/_meta.yml").map_err(|e| anyhow!(e))?,
        )
        .map_err(|e| anyhow!(e))?;

        Ok(data.0.remove(sound_id))
    }

    #[tracing::instrument(skip(self, stop), fields(file = Empty))]
    async fn play(&self, sound: &SoundVariant, stop: Option<Receiver<()>>) -> Result<()> {
        let _guard = if sound.exclusive {
            Some(self.exclusive_sound.lock().await)
        } else {
            None
        };

        Span::current().record("file", sound.file.as_str());

        let volume = sound.volume.unwrap_or(1.0);

        tracing::debug!("playing a sound");
        let mut process = just("play-sound", &[&sound.file, volume.to_string().as_str()])?;

        if process.wait(stop).await {
            tracing::debug!("finished playing sound");
        } else {
            tracing::debug!("interrupted sound playback");
        }

        Ok(())
    }
}
