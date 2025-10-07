use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::Response,
    response::{Html, IntoResponse, Sse, sse::Event},
    routing::{get, post},
};
use rand::seq::SliceRandom;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::broadcast::Sender;

use crate::{
    injector_getter,
    integration::yt_music_api::{Restricted, Song, YouTubeMusic},
    services::Service,
};

#[derive(Debug, Error)]
pub enum AddSongError {
    #[error("Not found")]
    NotFound,
    #[error("Already in queue")]
    AlreadyInQueue,
    #[error("The video is age-restricted")]
    AgeRestricted,
    #[error("The video is region-restricted")]
    RegionRestricted,
    #[error("The video is both age- and region-restricted")]
    AgeAndRegionRestricted,
    #[error("internal")]
    Internal(#[from] anyhow::Error),
}

#[async_trait]
pub trait MusicService: Service {
    async fn get_volume(&self) -> Result<u32>;

    async fn set_volume(&self, volume: u32) -> Result<()>;

    async fn is_paused(&self) -> Result<bool>;

    async fn set_pause(&self, paused: bool) -> Result<()>;

    async fn current(&self) -> Result<Option<Song>>;

    async fn request(&self, query: &str, requester: &str) -> Result<Song, AddSongError>;

    async fn cancel_last(&self, requester: &str) -> Result<Option<Song>>;

    async fn skip(&self) -> Result<bool>;

    async fn unskip(&self) -> Result<bool>;

    async fn queue(&self) -> Result<Vec<(Song, String)>>;

    async fn clear(&self) -> Result<()>;
}

injector_getter!(MusicService::music);

#[derive(Debug, Serialize, Deserialize)]
enum SongSource {
    Playlist,
    Request { requester: String },
}

#[derive(Default)]
struct ServerState {
    playlist: VecDeque<Song>,
    request_queue: VecDeque<(Song, String)>,
    history: VecDeque<(Song, SongSource)>,
    volume: u32,
    paused: bool,
}

pub struct YouTubeMusicPlayer {
    ytm: YouTubeMusic,
    state: Mutex<ServerState>,
    broadcast: Sender<Event>,
}

impl YouTubeMusicPlayer {
    pub async fn new(ytm: YouTubeMusic, playlist_id: &str) -> Result<Self> {
        let mut playlist = ytm.load_playlist(playlist_id).await?;

        // swap_remove is ok because we shuffle anyway
        let intro = playlist.swap_remove(0);
        if playlist.is_empty() {
            return Err(anyhow!("playlist had a single song (intro)"));
        }
        playlist.shuffle(&mut rand::rng());

        Ok(Self {
            ytm,
            state: Mutex::new(ServerState {
                playlist: playlist.into(),
                request_queue: [(intro, "<system>".into())].into(),
                volume: 5,
                ..Default::default()
            }),
            broadcast: tokio::sync::broadcast::channel(16).0,
        })
    }

    async fn next(&self) -> Song {
        let mut state = self.state.lock().unwrap();
        if let Some((song, requester)) = state.request_queue.pop_front() {
            state
                .history
                .push_back((song.clone(), SongSource::Request { requester }));
            return song;
        }
        let next = state.playlist.front().cloned().unwrap(); // playlist is never empty and we never pop it
        state.playlist.rotate_left(1);
        state
            .history
            .push_back((next.clone(), SongSource::Playlist));
        next
    }

    async fn prev(&self) -> Option<Song> {
        let mut state = self.state.lock().unwrap();
        if let Some((song, source)) = state.history.pop_back() {
            match source {
                SongSource::Playlist => state.playlist.rotate_right(1),
                SongSource::Request { requester } => {
                    state.request_queue.push_front((song.clone(), requester))
                }
            }
            return state.history.back().map(|(s, _)| s.clone());
        }
        None
    }

    pub fn start(self: Arc<Self>, bind_addr: &str) -> impl Future<Output = Result<()>> + use<> {
        let app = Router::new()
            .route("/", get(Html(include_str!("./music-player.html"))))
            .route(
                "/events",
                get(async |State(s): State<Arc<Self>>| {
                    let rx = s.broadcast.subscribe();
                    let stream = futures::stream::try_unfold(rx, |mut rx| async {
                        anyhow::Ok(Some((rx.recv().await?, rx)))
                    });
                    Sse::new(stream).keep_alive(Default::default())
                }),
            )
            .route(
                "/request",
                post({
                    #[derive(Deserialize)]
                    struct Request {
                        q: String,
                        requester: Option<String>,
                    }

                    async |State(s): State<Arc<Self>>,
                           Query(Request { q, requester }): Query<Request>|
                           -> Result<StatusCode, AddSongError> {
                        s.request(&q, requester.as_deref().unwrap_or("anon"))
                            .await?;
                        Ok(StatusCode::ACCEPTED)
                    }
                }),
            )
            .route(
                "/next",
                get(async |State(s): State<Arc<Self>>| Json(s.next().await)),
            )
            .route(
                "/prev",
                get(async |State(s): State<Arc<Self>>| Json(s.prev().await)),
            )
            .route(
                "/state/volume/{volume}",
                post(
                    async |State(s): State<Arc<Self>>, Path(volume): Path<u32>| {
                        s.state.lock().unwrap().volume = volume;
                    },
                ),
            )
            .route(
                "/state/paused/{state}",
                post(
                    async |State(s): State<Arc<Self>>, Path(state): Path<bool>| {
                        s.state.lock().unwrap().paused = state;
                    },
                ),
            )
            .with_state(self);

        let bind_addr = bind_addr.to_owned(); // meh
        async move {
            let listener = tokio::net::TcpListener::bind(bind_addr).await?;
            tracing::info!("listening on {}", listener.local_addr()?);
            axum::serve(listener, app).await?;
            Ok(())
        }
    }
}

impl IntoResponse for AddSongError {
    fn into_response(self) -> Response<Body> {
        tracing::error!(e = ?self, "music server error");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {self}"),
        )
            .into_response()
    }
}

#[async_trait]
impl MusicService for YouTubeMusicPlayer {
    async fn get_volume(&self) -> Result<u32> {
        Ok(self.state.lock().unwrap().volume)
    }

    async fn set_volume(&self, volume: u32) -> Result<()> {
        _ = self.broadcast.send(
            Event::default()
                .event("volume")
                .data(volume.min(100).to_string()),
        );
        Ok(())
    }

    async fn is_paused(&self) -> Result<bool> {
        Ok(self.state.lock().unwrap().paused)
    }

    async fn set_pause(&self, paused: bool) -> Result<()> {
        _ = self.broadcast.send(
            Event::default()
                .event(if paused { "pause" } else { "play" })
                .data(""),
        );
        Ok(())
    }

    async fn current(&self) -> Result<Option<Song>> {
        let state = self.state.lock().unwrap();
        if state.paused {
            return Ok(None);
        }
        Ok(state.history.back().map(|(s, _)| s.clone()))
    }

    async fn request(&self, query: &str, requester: &str) -> Result<Song, AddSongError> {
        let song = match self.ytm.get_song(query).await? {
            Some(song) => Some(song),
            None => self.ytm.search(query).await?,
        };

        if let Some(mut song) = song {
            match song.restricted.take() {
                Some(Restricted::Age) => return Err(AddSongError::AgeRestricted),
                Some(Restricted::Region) => return Err(AddSongError::RegionRestricted),
                Some(Restricted::AgeAndRegion) => return Err(AddSongError::AgeAndRegionRestricted),
                None => {}
            }

            let mut state = self.state.lock().unwrap();
            if state
                .request_queue
                .iter()
                .any(|(s, _)| s.video_id == song.video_id)
            {
                return Err(AddSongError::AlreadyInQueue);
            }
            state
                .request_queue
                .push_back((song.clone(), requester.into()));
            return Ok(song);
        }

        Err(AddSongError::NotFound)
    }

    async fn cancel_last(&self, requester: &str) -> Result<Option<Song>> {
        let mut state = self.state.lock().unwrap();
        if let Some(pos) = state
            .request_queue
            .iter()
            .rposition(|(_, r)| r == requester)
        {
            return Ok(state.request_queue.remove(pos).map(|(s, _)| s));
        }
        Ok(None)
    }

    async fn skip(&self) -> Result<bool> {
        if self.state.lock().unwrap().paused {
            return Ok(false);
        }
        _ = self.broadcast.send(Event::default().event("next").data(""));
        Ok(true)
    }

    async fn unskip(&self) -> Result<bool> {
        if self.state.lock().unwrap().paused {
            return Ok(false);
        }
        _ = self.broadcast.send(Event::default().event("prev").data(""));
        Ok(true)
    }

    async fn queue(&self) -> Result<Vec<(Song, String)>> {
        let state = self.state.lock().unwrap();
        Ok(state.request_queue.iter().cloned().collect())
    }

    async fn clear(&self) -> Result<()> {
        self.state.lock().unwrap().request_queue.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{config::Config, logging};

    use super::*;

    #[tokio::test]
    #[ignore = "manual test"]
    async fn music_player_server() -> Result<()> {
        _ = logging::init();

        let config = Config::load()?;
        let server = Arc::new(
            YouTubeMusicPlayer::new(
                YouTubeMusic::new(config.youtube.api_key, config.youtube.country_code),
                &config.youtube.playlist,
            )
            .await?,
        );

        server.start("localhost:35354").await?;

        Ok(())
    }
}
