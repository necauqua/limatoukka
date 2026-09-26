use std::{collections::HashMap, time::Duration};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub struct YouTubeMusic {
    api_key: String,
    country: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Restricted {
    Age,
    Region,
    AgeAndRegion,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Song {
    pub title: String,
    pub author: String,
    pub video_id: String,
    pub length: Duration,
    pub restricted: Option<Restricted>,
}

impl YouTubeMusic {
    pub fn new(api_key: String, country: String) -> Self {
        Self { api_key, country }
    }

    async fn query<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        mut query: HashMap<&str, &str>,
    ) -> Result<T> {
        query.insert("key", &self.api_key);

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum YoutubeResult<T> {
            Ok(T),
            Error { error: serde_json::Value },
        }

        let url = format!("https://www.googleapis.com/youtube/v3/{endpoint}");

        let res = reqwest::Client::new()
            .get(url)
            .query(&query)
            .send()
            .await?
            .error_for_status()?
            .json::<YoutubeResult<T>>()
            .await?;

        match res {
            YoutubeResult::Ok(r) => Ok(r),
            YoutubeResult::Error { error } => Err(anyhow!("YouTube API error: {error:#?}")),
        }
    }

    pub async fn get_song(&self, video_id: &str) -> Result<Option<Song>> {
        #[derive(Deserialize)]
        struct VideoResponse {
            items: Vec<VideoItem>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct VideoLocalization {
            title: String,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct VideoItem {
            snippet: VideoSnippet,
            content_details: VideoContentDetails,
            localizations: HashMap<String, VideoLocalization>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct VideoContentDetails {
            duration: String,
            region_restriction: Option<RegionRestriction>,
            content_rating: Option<ContentRating>,
        }

        #[derive(Deserialize)]
        struct RegionRestriction {
            blocked: Option<Vec<String>>,
            allowed: Option<Vec<String>>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ContentRating {
            yt_rating: Option<String>, // "ytAgeRestricted"
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct VideoSnippet {
            title: String,
            channel_title: String,
            live_broadcast_content: String, // "none", "live"
        }

        let Some(mut item) = self
            .query::<VideoResponse>(
                "videos",
                HashMap::from([
                    ("part", "snippet,contentDetails,localizations"),
                    ("id", video_id),
                ]),
            )
            .await?
            .items
            .into_iter()
            .next()
        else {
            return Ok(None);
        };

        if item.snippet.live_broadcast_content != "none" {
            return Ok(None); // skip live videos
        }

        let title = item
            .localizations
            .remove("en")
            .map_or(item.snippet.title, |l| l.title);

        let song = Song {
            title,
            author: item
                .snippet
                .channel_title
                .trim_end_matches(" - Topic")
                .into(),
            video_id: video_id.to_string(),
            length: parse_duration::parse(&item.content_details.duration)?,
            restricted: {
                let age = item
                    .content_details
                    .content_rating
                    .and_then(|rating| rating.yt_rating)
                    .is_some_and(|yt| yt == "ytAgeRestricted");
                let region = item.content_details.region_restriction.is_some_and(|r| {
                    r.blocked.is_some_and(|b| b.contains(&self.country))
                        || r.allowed.is_some_and(|a| !a.contains(&self.country))
                });
                match (age, region) {
                    (false, false) => None,
                    (true, false) => Some(Restricted::Age),
                    (false, true) => Some(Restricted::Region),
                    (true, true) => Some(Restricted::AgeAndRegion),
                }
            },
        };

        Ok(Some(song))
    }

    pub async fn search(&self, query: &str) -> Result<Option<Song>> {
        #[derive(Deserialize)]
        struct SearchResponse {
            items: Vec<SearchItem>,
        }

        #[derive(Deserialize)]
        struct SearchItem {
            id: SearchId,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct SearchId {
            video_id: String,
        }

        let res = self
            .query::<SearchResponse>(
                "search",
                HashMap::from([
                    ("part", "id"),
                    ("type", "video"),
                    ("topicId", "/m/04rlf"), // music
                    ("q", query),
                    ("maxResults", "1"),
                ]),
            )
            .await?;

        match res.items.into_iter().next() {
            Some(item) => self.get_song(&item.id.video_id).await,
            None => Ok(None),
        }
    }

    pub async fn load_playlist(&self, playlist_id: &str) -> Result<Vec<Song>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct PlaylistResponse {
            items: Vec<PlaylistItem>,
            next_page_token: Option<String>,
        }

        #[derive(Deserialize)]
        struct PlaylistItem {
            snippet: PlaylistSnippet,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct PlaylistSnippet {
            title: String,
            video_owner_channel_title: Option<String>,
            resource_id: ResourceId,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ResourceId {
            video_id: String,
        }

        let mut songs = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut query = HashMap::from([
                ("part", "snippet,contentDetails"),
                ("maxResults", "50"),
                ("playlistId", playlist_id),
            ]);
            if let Some(token) = &page_token {
                query.insert("pageToken", token);
            }

            let res = self
                .query::<PlaylistResponse>("playlistItems", query)
                .await?;

            for item in res.items {
                let Some(video_owner_channel_title) = item.snippet.video_owner_channel_title else {
                    tracing::warn!(
                        "Skipping video {} because it has no channel title (was privated or something)",
                        item.snippet.resource_id.video_id
                    );
                    continue;
                };
                songs.push(Song {
                    title: item.snippet.title,
                    author: video_owner_channel_title
                        .trim_end_matches(" - Topic")
                        .into(),
                    video_id: item.snippet.resource_id.video_id,
                    length: Duration::ZERO, // can't get it and we don't care for playlists really
                    restricted: None,       // assume the playlist is manually curated
                });
            }

            if res.next_page_token.is_none() {
                return Ok(songs);
            }

            page_token = res.next_page_token;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{config::Config, logging};

    use super::*;

    #[tokio::test]
    #[ignore = "manual test"]
    async fn youtube_music() -> Result<()> {
        _ = logging::init();

        let config = Config::load()?;

        let ytm = YouTubeMusic::new(config.youtube.api_key, config.youtube.country_code);

        let playlist = ytm.load_playlist(&config.youtube.playlist).await?;

        println!("Playlist has {} songs", playlist.len());

        Ok(())
    }
}
