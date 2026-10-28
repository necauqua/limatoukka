//! A folder of audio files that the music player plays instead of the
//! YouTube playlist or together with it.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Result, bail};
use lofty::{file::TaggedFileExt, prelude::*};
use reqwest::Url;
use walkdir::WalkDir;

use crate::{
    config::FolderMode,
    integration::yt_music_api::{Media, Song},
};

/// The extensions of the audio files that Chromium (and thus OBS) can play.
const EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "wav", "m4a", "aac", "webm", "weba",
];

/// The route of the music player server that serves the files.
pub const ROUTE: &str = "/media";

pub struct Library {
    pub root: PathBuf,
    /// What `now-playing` says about a song from the folder
    pub label: String,
    /// Whether the folder replaces the YouTube playlist or plays together
    /// with it
    pub mode: FolderMode,
}

impl Library {
    /// All the audio files in the folder and its subfolders.
    pub async fn scan(&self) -> Result<Vec<Song>> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || scan(&root)).await?
    }
}

/// The URL of the file on the music player server.
pub fn url(file: &Path) -> String {
    let mut url = Url::parse("http://localhost").expect("valid URL");
    url.path_segments_mut()
        .expect("URL with a path")
        .push(&ROUTE[1..])
        .extend(file.iter().map(|part| part.to_string_lossy()));
    url.path().to_owned()
}

fn scan(root: &Path) -> Result<Vec<Song>> {
    if !root.is_dir() {
        bail!("the music folder {} is not a folder", root.display());
    }
    let mut songs = Vec::new();
    for entry in WalkDir::new(root).follow_links(true) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                tracing::warn!(%e, "skipped a part of the music folder");
                continue;
            }
        };
        let path = entry.path();
        let is_audio = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)));
        if !entry.file_type().is_file() || !is_audio {
            continue;
        }
        let file = path.strip_prefix(root)?;
        if file.to_str().is_none() {
            // the song is stored as JSON, which needs UTF-8
            tracing::warn!(path = %path.display(), "skipped a file with a non-UTF-8 name");
            continue;
        }
        songs.push(read_song(path, file.to_owned()));
    }
    tracing::info!("found {} songs in {}", songs.len(), root.display());
    Ok(songs)
}

/// The title and artist come from the tags. Without tags, they come from a
/// `Artist - Title.ext` (or `Artist – Title.ext`) file name.
fn read_song(path: &Path, file: PathBuf) -> Song {
    let (tag_title, tag_artist, length) = match lofty::read_from_path(path) {
        Ok(tagged) => {
            let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
            let get = |value: Option<std::borrow::Cow<'_, str>>| {
                value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
            };
            (
                tag.and_then(|t| get(t.title())),
                tag.and_then(|t| get(t.artist())),
                tagged.properties().duration(),
            )
        }
        Err(e) => {
            tracing::debug!(%e, path = %path.display(), "could not read the tags");
            (None, None, Duration::ZERO)
        }
    };

    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let (name_artist, name_title) = match stem.split_once(" - ").or_else(|| stem.split_once(" – "))
    {
        Some((artist, title)) => (Some(artist.trim()), title.trim()),
        None => (None, stem.trim()),
    };

    Song {
        title: tag_title.unwrap_or_else(|| name_title.to_owned()),
        author: tag_artist
            .or_else(|| name_artist.map(str::to_owned))
            .unwrap_or_else(|| "Unknown artist".to_owned()),
        media: Media::File { file },
        length,
        restricted: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "manual test"]
    async fn scan_config_folder() -> Result<()> {
        let music = crate::config::Config::load()?
            .music
            .ok_or_else(|| anyhow::anyhow!("no [music] config"))?;
        let library = Library {
            root: music.folder,
            label: music.folder_label,
            mode: music.mode,
        };
        let songs = library.scan().await?;
        for song in &songs {
            println!(
                "{} - {} ({:?}) {:?}",
                song.author, song.title, song.length, song.media
            );
        }
        println!("{} songs", songs.len());
        Ok(())
    }

    #[test]
    fn url_encodes_each_part() {
        assert_eq!(
            url(Path::new("a b/c?#%d/Artist - Title.mp3")),
            "/media/a%20b/c%3F%23%25d/Artist%20-%20Title.mp3"
        );
    }

    #[test]
    fn name_without_tags() {
        let song = read_song(
            Path::new("/nonexistent/x/Artist - Title.mp3"),
            "x/Artist - Title.mp3".into(),
        );
        assert_eq!(song.author, "Artist");
        assert_eq!(song.title, "Title");

        let song = read_song(Path::new("/nonexistent/Title.mp3"), "Title.mp3".into());
        assert_eq!(song.author, "Unknown artist");
        assert_eq!(song.title, "Title");
    }
}
