//! A disk cache of the emote and badge images, served at `/chat/img/...`.
//!
//! `https://<host>/<path>` is served at `/chat/img/<host>/<path>`, for the
//! hosts in [`HOSTS`] only. The files are in the XDG cache folder; they are
//! never removed, because the CDN URLs do not change their content.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use axum::{
    extract::{Path as UrlPath, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

/// The prefix of the image routes
pub const ROUTE: &str = "/chat/img";

/// The CDNs whose images are served from the cache
const HOSTS: &[&str] = &[
    "static-cdn.jtvnw.net",
    "d3aqoihi2n8ty8.cloudfront.net", // cheermotes
    "cdn.7tv.app",
    "cdn.betterttv.net",
    "cdn.frankerfacez.com",
    "fourtf.com",       // Chatterino badges
    "cdn.jsdelivr.net", // Twemoji
];

/// The cache folder: `$XDG_CACHE_HOME/limatoukka/chat-images`, or
/// `~/.cache/limatoukka/chat-images`.
pub fn default_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("limatoukka").join("chat-images")
}

/// The local URL of an image, or the URL itself if its host is not cached.
/// Protocol-relative URLs (`//host/path`) are accepted.
pub fn local_url(url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("//"))
        .unwrap_or(url);
    match rest.split_once('/') {
        Some((host, path)) if HOSTS.contains(&host) && is_safe_path(path) => {
            format!("{ROUTE}/{host}/{path}")
        }
        _ => url.to_owned(),
    }
}

/// Only the characters that the CDN URLs use, so that the path cannot
/// have a query, `..` etc.
fn is_safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 200
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-@".contains(&b))
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

pub struct ImageCache {
    dir: PathBuf,
    http: reqwest::Client,
}

impl ImageCache {
    pub fn new(dir: PathBuf, http: reqwest::Client) -> Self {
        Self { dir, http }
    }

    /// The cache file of `host/path`. The path is one file name (`/` →
    /// `%2F`), so that `a/b` and `a/b/c` do not collide.
    fn file(&self, host: &str, path: &str) -> PathBuf {
        self.dir.join(host).join(path.replace('/', "%2F"))
    }

    async fn get(&self, host: &str, path: &str) -> Result<Vec<u8>> {
        if !HOSTS.contains(&host) || !is_safe_path(path) {
            bail!("not a cached image URL");
        }
        let file = self.file(host, path);
        if let Ok(data) = tokio::fs::read(&file).await {
            return Ok(data);
        }
        let data = self
            .http
            .get(format!("https://{host}/{path}"))
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?
            .to_vec();
        if let Err(error) = write_atomically(&file, &data).await {
            tracing::warn!(?error, ?file, "failed to cache an image");
        }
        Ok(data)
    }

    /// The handler of `/chat/img/{*path}`.
    pub async fn serve(
        State(cache): State<std::sync::Arc<Self>>,
        UrlPath(path): UrlPath<String>,
    ) -> Response {
        let Some((host, path)) = path.split_once('/') else {
            return StatusCode::NOT_FOUND.into_response();
        };
        match cache.get(host, path).await {
            Ok(data) => (
                [
                    (header::CONTENT_TYPE, content_type(&data)),
                    (header::CACHE_CONTROL, "public, max-age=604800"),
                ],
                data,
            )
                .into_response(),
            Err(error) => {
                tracing::debug!(?error, host, path, "image not available");
                StatusCode::NOT_FOUND.into_response()
            }
        }
    }
}

async fn write_atomically(file: &Path, data: &[u8]) -> Result<()> {
    let dir = file.parent().context("no parent folder")?;
    tokio::fs::create_dir_all(dir).await?;
    let tmp = dir.join(format!(".tmp-{}", uuid::Uuid::now_v7()));
    tokio::fs::write(&tmp, data).await?;
    tokio::fs::rename(&tmp, file).await?;
    Ok(())
}

/// The type from the magic bytes; some CDN URLs have no file extension.
fn content_type(data: &[u8]) -> &'static str {
    match data {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => "image/webp",
        [
            _,
            _,
            _,
            _,
            b'f',
            b't',
            b'y',
            b'p',
            b'a',
            b'v',
            b'i',
            b's' | b'f',
            ..,
        ] => "image/avif",
        [0xFF, 0xD8, ..] => "image/jpeg",
        _ if data.trim_ascii_start().starts_with(b"<") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_urls() {
        assert_eq!(
            local_url("https://cdn.7tv.app/emote/01ABC/4x.webp"),
            "/chat/img/cdn.7tv.app/emote/01ABC/4x.webp"
        );
        assert_eq!(
            local_url("//cdn.frankerfacez.com/emote/1/4"),
            "/chat/img/cdn.frankerfacez.com/emote/1/4"
        );
        // other hosts and odd paths stay as they are
        assert_eq!(
            local_url("https://example.com/a.png"),
            "https://example.com/a.png"
        );
        assert_eq!(
            local_url("https://cdn.7tv.app/a/../b"),
            "https://cdn.7tv.app/a/../b"
        );
        assert_eq!(
            local_url("https://cdn.7tv.app/a?b=c"),
            "https://cdn.7tv.app/a?b=c"
        );
    }

    #[test]
    fn files_do_not_collide() {
        let cache = ImageCache::new("/c".into(), reqwest::Client::new());
        assert_eq!(
            cache.file("cdn.7tv.app", "emote/x/4x.webp"),
            PathBuf::from("/c/cdn.7tv.app/emote%2Fx%2F4x.webp")
        );
    }

    #[tokio::test]
    #[ignore = "manual test"]
    async fn real_fetch() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("chat-images-{}", uuid::Uuid::now_v7()));
        let cache = ImageCache::new(dir.clone(), reqwest::Client::new());
        let path = "emoticons/v2/25/default/dark/3.0";
        let data = cache.get("static-cdn.jtvnw.net", path).await?;
        assert_eq!(content_type(&data), "image/png");
        let cached = tokio::fs::read(cache.file("static-cdn.jtvnw.net", path)).await?;
        assert_eq!(cached, data);
        tokio::fs::remove_dir_all(dir).await?;
        Ok(())
    }

    #[test]
    fn sniffing() {
        assert_eq!(content_type(b"GIF89a..."), "image/gif");
        assert_eq!(content_type(b"RIFF\0\0\0\0WEBPVP8 "), "image/webp");
        assert_eq!(content_type(b"  <svg xmlns=..."), "image/svg+xml");
    }
}
