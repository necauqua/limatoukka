use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use thiserror::Error;

use crate::integration::justfile::just;

#[derive(Debug, Error)]
pub enum AddSongError {
    #[error("Failed to add song")]
    Failed,
    #[error("Song is already in queue")]
    AlreadyInQueue,
    #[error("This YouTube video ID was not accepted by YouTube Music")]
    NotYTMusic,
    #[error("internal")]
    Internal(#[from] anyhow::Error),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Song {
    pub title: String,
    pub author: String,
    pub video_id: String,
}

#[async_trait]
pub trait MusicService: Send + Sync {
    async fn get_volume(&self) -> Result<u32>;
    async fn set_volume(&self, volume: u32) -> Result<()>;
    async fn current(&self) -> Result<Option<String>>;
    async fn add(&self, id: &str) -> Result<Song, AddSongError>;
    async fn skip(&self) -> Result<bool>;
    async fn queue(&self) -> Result<Vec<Song>>;
    async fn queue_reset(&self) -> Result<()>;
}

pub struct MusicServiceImpl {
    request_cursor: AtomicU32,
    volume_endpoint: String,
}

impl MusicServiceImpl {
    pub fn new(url: String) -> Self {
        Self {
            request_cursor: AtomicU32::new(0),
            volume_endpoint: format!("{url}/api/v1/volume"),
        }
    }
}

// ughghghghfghghgh
fn surf_to_anyhow(e: surf::Error) -> anyhow::Error {
    let ctx = format!("Status code {}", e.status());
    e.into_inner().context(ctx)
}

#[async_trait]
impl MusicService for MusicServiceImpl {
    async fn get_volume(&self) -> Result<u32> {
        #[derive(Deserialize)]
        struct VolumeResponse {
            state: u32,
        }

        Ok(surf::get(&self.volume_endpoint)
            .recv_json::<VolumeResponse>()
            .await
            .map_err(surf_to_anyhow)?
            .state)
    }

    async fn set_volume(&self, volume: u32) -> Result<()> {
        #[derive(serde::Serialize)]
        struct SetVolumeRequest {
            volume: u32,
        }

        surf::post(&self.volume_endpoint)
            .body_json(&SetVolumeRequest { volume })
            .map_err(surf_to_anyhow)?
            .send()
            .await
            .map_err(surf_to_anyhow)?;

        Ok(())
    }

    async fn current(&self) -> Result<Option<String>> {
        Ok(just("music-np", &[])?.get().await?.ok())
    }

    async fn add(&self, id: &str) -> Result<Song, AddSongError> {
        let cursor = self.request_cursor.load(Ordering::Relaxed);

        let res = just("music-queue-add", &[id, &(cursor + 1).to_string()])?
            .check()
            .await?;

        if res.trim().is_empty() {
            return Err(AddSongError::Failed);
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum AddResponse {
            Success {
                #[serde(flatten)]
                song: Song,
                idx: u32,
            },
            Broken {
                _broken: bool,
            },
            AlreadyInQueue {
                _already_in_queue: bool,
            },
        }

        match serde_json::from_str::<AddResponse>(&res).map_err(|e| anyhow!(e))? {
            AddResponse::Success { song, idx } => {
                self.request_cursor.store(idx, Ordering::Relaxed);
                Ok(song)
            }
            AddResponse::Broken { .. } => Err(AddSongError::NotYTMusic),
            AddResponse::AlreadyInQueue { .. } => Err(AddSongError::AlreadyInQueue),
        }
    }

    async fn skip(&self) -> Result<bool> {
        Ok(just("music-skip", &[])?.get().await?.is_ok())
    }

    async fn queue(&self) -> Result<Vec<Song>> {
        let cursor = self.request_cursor.load(Ordering::Relaxed);
        let res = just("music-queue", &[&cursor.to_string()])?.check().await?;
        res.lines().try_fold(Vec::new(), |mut acc, line| {
            acc.push(serde_json::from_str(line).map_err(|e| anyhow!(e))?);
            Ok(acc)
        })
    }

    async fn queue_reset(&self) -> Result<()> {
        self.request_cursor.store(0, Ordering::Relaxed);
        Ok(())
    }
}
