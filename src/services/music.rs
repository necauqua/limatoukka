use std::sync::Arc;

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
use rustis::{
    client::Client as ValkeyClient,
    commands::{
        GenericCommands, LMoveWhere, ListCommands, SetCondition, SetExpiration, StringCommands,
    },
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::broadcast::Sender;

use crate::{
    injector_getter,
    integration::yt_music_api::{Restricted, Song, YouTubeMusic},
    services::Service,
};

#[derive(Debug, Error)]
pub enum MusicError {
    #[error("Not found")]
    SongNotFound,
    #[error("Already in queue")]
    SongAlreadyInQueue,
    #[error("The video is age-restricted")]
    AgeRestricted,
    #[error("The video is region-restricted")]
    RegionRestricted,
    #[error("The video is both age- and region-restricted")]
    AgeAndRegionRestricted,
    #[error("internal")]
    Internal(#[from] anyhow::Error),
}

impl From<rustis::Error> for MusicError {
    fn from(e: rustis::Error) -> Self {
        MusicError::Internal(anyhow!(e))
    }
}

pub enum SongQuery<'s> {
    Any(&'s str),
    IdOnly(&'s str),
}

#[async_trait]
pub trait MusicService: Service {
    async fn get_volume(&self) -> Result<u32>;

    async fn set_volume(&self, volume: u32) -> Result<()>;

    async fn is_paused(&self) -> Result<bool>;

    async fn set_pause(&self, paused: bool) -> Result<()>;

    async fn current(&self) -> Result<Option<(Song, SongSource)>>;

    async fn last(&self) -> Result<Option<(Song, SongSource)>>;

    async fn request(&self, query: SongQuery<'_>, requester: &str) -> Result<Song, MusicError>;

    async fn cancel_last(&self, requester: &str) -> Result<Option<Song>>;

    async fn skip(&self) -> Result<bool>;

    async fn unskip(&self) -> Result<bool>;

    async fn queue(&self) -> Result<Vec<(Song, SongSource)>>;

    async fn clear(&self) -> Result<()>;
}

injector_getter!(MusicService::music);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SongSource {
    Playlist,
    Request { requester: String },
}

pub struct YouTubeMusicPlayer {
    ytm: YouTubeMusic,
    playlist_id: String,
    valkey: ValkeyClient,
    broadcast: Sender<Event>,
}

const PLAYLIST: &str = "music:playlist";
const REQUESTS: &str = "music:requests";
const HISTORY: &str = "music:history";
const VOLUME: &str = "music:volume";
const PAUSED: &str = "music:paused";

impl YouTubeMusicPlayer {
    pub async fn new(ytm: YouTubeMusic, playlist_id: &str, valkey: ValkeyClient) -> Result<Self> {
        valkey
            .set_with_options(VOLUME, "15", SetCondition::NX, SetExpiration::None, false)
            .await?;

        Ok(Self {
            ytm,
            playlist_id: playlist_id.to_string(),
            valkey,
            broadcast: tokio::sync::broadcast::channel(16).0,
        })
    }

    async fn next(&self) -> Result<(Song, SongSource)> {
        let request = self
            .valkey
            .lmove::<_, _, Option<String>>(REQUESTS, HISTORY, LMoveWhere::Left, LMoveWhere::Left)
            .await?;

        if let Some(request) = request {
            return Ok(serde_json::from_str(&request)?);
        }

        let next = self
            .valkey
            .lmove::<_, _, String>(PLAYLIST, PLAYLIST, LMoveWhere::Left, LMoveWhere::Right)
            .await?;
        self.valkey.lpush(HISTORY, &next).await?;
        Ok(serde_json::from_str(&next)?)
    }

    async fn prev(&self) -> Result<Option<(Song, SongSource)>> {
        match self
            .valkey
            .lmove::<_, _, Option<String>>(HISTORY, REQUESTS, LMoveWhere::Left, LMoveWhere::Left)
            .await?
        {
            Some(_) => Ok(self.current().await?),
            _ => Ok(None),
        }
    }

    async fn do_request(&self, song: Song, source: SongSource) -> Result<()> {
        self.valkey
            .rpush(REQUESTS, serde_json::to_string(&(song, source))?)
            .await?;
        Ok(())
    }

    async fn reset(&self) -> Result<()> {
        self.valkey.del([HISTORY, REQUESTS, PLAYLIST]).await?;

        let mut playlist = self.ytm.load_playlist(&self.playlist_id).await?;
        let intro = playlist.swap_remove(0);
        if playlist.is_empty() {
            return Err(anyhow!("playlist had a single song (intro)"));
        }
        playlist.shuffle(&mut rand::rng());

        self.valkey
            .rpush(
                PLAYLIST,
                playlist.into_iter().try_fold(Vec::new(), |mut acc, s| {
                    acc.push(serde_json::to_string(&(s, SongSource::Playlist))?);
                    anyhow::Ok(acc)
                })?,
            )
            .await?;

        self.do_request(intro.clone(), SongSource::Playlist).await?;

        Ok(())
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
                           -> Result<StatusCode, MusicError> {
                        s.request(SongQuery::Any(&q), requester.as_deref().unwrap_or("anon"))
                            .await?;
                        Ok(StatusCode::ACCEPTED)
                    }
                }),
            )
            .route(
                "/next",
                post(async |State(s): State<Arc<Self>>| Ok::<_, MusicError>(Json(s.next().await?))),
            )
            .route(
                "/prev",
                post(async |State(s): State<Arc<Self>>| Ok::<_, MusicError>(Json(s.prev().await?))),
            )
            .route(
                "/reset",
                post(async |State(s): State<Arc<Self>>| {
                    Ok::<_, MusicError>(Json(s.reset().await?))
                }),
            )
            .route(
                "/state/volume/{volume}",
                post(
                    async |State(s): State<Arc<Self>>, Path(volume): Path<u32>| {
                        s.valkey.set(VOLUME, volume.min(100).to_string()).await?;
                        Ok::<_, MusicError>(())
                    },
                ),
            )
            .route(
                "/state/paused/{state}",
                post(
                    async |State(s): State<Arc<Self>>, Path(state): Path<bool>| {
                        if state {
                            s.valkey.set(PAUSED, "1").await?;
                        } else {
                            s.valkey.del(PAUSED).await?;
                        }
                        Ok::<_, MusicError>(())
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

impl IntoResponse for MusicError {
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
        Ok(self
            .valkey
            .get::<_, Option<String>>(VOLUME)
            .await?
            .and_then(|v| v.parse().ok())
            .unwrap_or_default())
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
        Ok(self
            .valkey
            .get::<_, Option<String>>(PAUSED)
            .await?
            .is_some())
    }

    async fn set_pause(&self, paused: bool) -> Result<()> {
        _ = self.broadcast.send(
            Event::default()
                .event(if paused { "pause" } else { "play" })
                .data(""),
        );
        Ok(())
    }

    async fn current(&self) -> Result<Option<(Song, SongSource)>> {
        let current = self.valkey.lindex::<_, Option<String>>(HISTORY, 0).await?;
        if let Some(current) = current {
            return Ok(Some(serde_json::from_str::<(Song, SongSource)>(&current)?));
        }
        Ok(None)
    }

    async fn last(&self) -> Result<Option<(Song, SongSource)>> {
        let current = self.valkey.lindex::<_, Option<String>>(HISTORY, 1).await?;
        if let Some(current) = current {
            return Ok(Some(serde_json::from_str::<(Song, SongSource)>(&current)?));
        }
        Ok(None)
    }

    async fn request(&self, query: SongQuery<'_>, requester: &str) -> Result<Song, MusicError> {
        let song = match query {
            SongQuery::Any(query) => match self.ytm.get_song(query).await? {
                Some(song) => Some(song),
                None => self.ytm.search(query).await?,
            },
            SongQuery::IdOnly(id) => self.ytm.get_song(id).await?,
        };

        if let Some(mut song) = song {
            match song.restricted.take() {
                Some(Restricted::Age) => return Err(MusicError::AgeRestricted),
                Some(Restricted::Region) => return Err(MusicError::RegionRestricted),
                Some(Restricted::AgeAndRegion) => return Err(MusicError::AgeAndRegionRestricted),
                None => {}
            }

            // todo ehh technically this is not atomic
            let queue = self
                .valkey
                .lrange::<_, _, Vec<String>>(REQUESTS, 0, -1)
                .await?;

            if queue.iter().any(|s| {
                serde_json::from_str::<(Song, SongSource)>(s)
                    .is_ok_and(|(s, _)| s.video_id == song.video_id)
            }) {
                return Err(MusicError::SongAlreadyInQueue);
            }

            self.do_request(
                song.clone(),
                SongSource::Request {
                    requester: requester.into(),
                },
            )
            .await?;

            return Ok(song);
        }

        Err(MusicError::SongNotFound)
    }

    async fn cancel_last(&self, requester: &str) -> Result<Option<Song>> {
        let queue = self
            .valkey
            .lrange::<_, _, Vec<String>>(REQUESTS, 0, -1)
            .await?;

        let song = queue.iter().rfind(|s| {
            serde_json::from_str::<(Song, SongSource)>(s).is_ok_and(|(_, source)| match source {
                SongSource::Request { requester: r } => r == requester,
                _ => false,
            })
        });

        if let Some(song) = song
            && self.valkey.lrem(REQUESTS, 1, song).await? > 0
        {
            let (song, _) = serde_json::from_str::<(Song, SongSource)>(song)?;
            return Ok(Some(song));
        }

        Ok(None)
    }

    async fn skip(&self) -> Result<bool> {
        if self.is_paused().await? {
            return Ok(false);
        }
        _ = self
            .broadcast
            .send(Event::default().event("next").data("1"));
        Ok(true)
    }

    async fn unskip(&self) -> Result<bool> {
        if self.is_paused().await? {
            return Ok(false);
        }
        _ = self
            .broadcast
            .send(Event::default().event("prev").data("1"));
        Ok(true)
    }

    async fn queue(&self) -> Result<Vec<(Song, SongSource)>> {
        self.valkey
            .lrange::<_, _, Vec<String>>(REQUESTS, 0, -1)
            .await?
            .into_iter()
            .map(|s| serde_json::from_str::<(Song, SongSource)>(&s))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow!(e))
    }

    async fn clear(&self) -> Result<()> {
        self.valkey.del(REQUESTS).await?;
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
                ValkeyClient::connect(config.valkey).await?,
            )
            .await?,
        );

        server.start("localhost:35354").await?;

        Ok(())
    }
}
