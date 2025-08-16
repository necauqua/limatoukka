use anyhow::{Result, bail};
use reqwest::Url;
use serde::Deserialize;

use crate::{
    commands::{args::InRange, command, runner::CommandFailure},
    context::{app::AppContext, cmd::CommandContext},
    fail,
    services::{messaging::PermissionLevel, sounds::SoundError},
};

/// Say something on stream through the TTS.
///
/// Only works for >= subscriber level, or from global macros.
#[command(sender_gate = 1m)]
async fn tts(ctx: CommandContext, msg: String) -> Result<()> {
    if !ctx.in_global_macro && ctx.message().sender.level < PermissionLevel::Subscriber {
        fail!("TTS is pay to win, or from global macros");
    }
    if msg.is_empty() {
        fail!("message cannot be empty");
    }

    let int = ctx.wait_for_interrupt();

    ctx.sounds().tts(&msg, int).await
}

/// Play a sound on stream.
/// Sounds ids are secret.
/// And also the command can only be run from global macros anyway ¯\_(ツ)_/¯.
#[command(sender_gate = 1m)]
async fn play_sound(ctx: CommandContext, sound_id: String) -> Result<()> {
    if !ctx.in_global_macro && ctx.message().sender.level < PermissionLevel::Caster {
        fail!("Sounds can only be played through global macros");
    }

    let int = ctx.wait_for_interrupt();

    match ctx.sounds().play_sound(&sound_id, int).await {
        Ok(_) => Ok(()),
        Err(e @ (SoundError::NotFound | SoundError::DidntChoose)) => fail!("{e}"),
        Err(e) => bail!(e),
    }
}

/// Get the title of the song that's currently playing on stream, if any.
#[command(global_gate = 5s, shortcode = np)]
async fn now_playing(ctx: CommandContext) -> Result<()> {
    match AppContext::just("music-np", &[])?.get().await? {
        Ok(title) => ctx.send(format!("Now playing: {title}")).await,
        Err(_) => fail!("Nothing is playing right now"),
    }
}

/// Skips the song that's currently playing on stream, if any.
#[command(global_gate = 15s, permission = Vip)]
async fn skip(ctx: CommandContext) -> Result<()> {
    match AppContext::just("music-skip", &[])?.get().await? {
        Ok(_) => ctx.reply("song skipped Madge".into()).await,
        Err(_) => fail!("Nothing is playing right now"),
    }
}

fn get_youtube_id(raw: &str) -> Result<String, CommandFailure> {
    let no_proto = !raw.starts_with("http://") && !raw.starts_with("https://");
    let url = if no_proto {
        format!("https://{raw}")
    } else {
        raw.into()
    };

    let Ok(url) = Url::parse(&url) else {
        return Ok(url);
    };

    const YOUTUBE: &[&str] = &[
        "youtube.com",
        "www.youtube.com",
        "music.youtube.com",
        "youtu.be",
    ];
    if !YOUTUBE.contains(&url.host_str().unwrap_or_default()) {
        // cringe way to support raw ids or smth
        if no_proto {
            return Ok(raw.into());
        }
        return Err(CommandFailure::new("Not a YouTube URL".into()));
    }

    url.query_pairs()
        .find(|(k, _)| k == "v")
        .map(|(_, v)| v.into())
        .or_else(|| {
            url.path_segments()
                .and_then(|mut s| s.rfind(|s| !s.is_empty()))
                .map(|s| s.into())
        })
        .ok_or_else(|| CommandFailure::new("Malformed YouTube URL".into()))
}

/// Adds the given song to the YouTube Music queue.
///
/// This only accepts YouTube links or IDs.
///
/// Note that the song will be added to the front of the queue - this is
/// because usually the queue is full of songs from my stream playlist and the
/// point of the command is to show me a song you think I wont insta-skip :)
#[command(sender_gate = 1m, shortcode=sr)]
async fn song_request(ctx: CommandContext, url_or_id: String) -> Result<()> {
    let id = get_youtube_id(&url_or_id)?;
    if id == "dQw4w9WgXcQ" {
        fail!("At least don't use a dQw link ICANT");
    }

    let res = AppContext::just("music-queue-add", &[&id])?.check().await?;
    if res.trim().is_empty() {
        fail!("Failed to add the song, likely it wasn't found");
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum AddResponse {
        Success { title: String, author: String },
        Borked { reason: String },
        AlreadyInQueue { _already_in_queue: bool },
    }

    match serde_json::from_str::<AddResponse>(&res)? {
        AddResponse::Success { title, author } => {
            ctx.send(format!(
                "Added a song to be played next: {author} - {title}"
            ))
            .await
        }
        AddResponse::Borked { reason } => {
            ctx.reply(format!("Failed to add song, stated reason: {reason}"))
                .await
        }
        AddResponse::AlreadyInQueue { .. } => ctx.reply("already in queue 🤦".into()).await,
    }
}

/// Set YouTube Music volume.
#[command(sender_gate = 3s, permission = Vip)]
async fn volume(_ctx: CommandContext, volume: InRange<0, 100>) -> Result<()> {
    AppContext::just("music-volume", &[&volume.get().to_string()])?
        .check()
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_youtube_id() {
        assert_eq!(
            get_youtube_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap(),
            "dQw4w9WgXcQ"
        );
        assert_eq!(
            get_youtube_id("https://youtube.com/watch?v=dQw4w9WgXcQ").unwrap(),
            "dQw4w9WgXcQ"
        );
        assert_eq!(
            get_youtube_id("https://youtu.be/dQw4w9WgXcQ").unwrap(),
            "dQw4w9WgXcQ"
        );
        assert_eq!(
            get_youtube_id("https://music.youtube.com/watch?v=dQw4w9WgXcQ").unwrap(),
            "dQw4w9WgXcQ"
        );
        assert_eq!(
            get_youtube_id("youtu.be/dQw4w9WgXcQ").unwrap(),
            "dQw4w9WgXcQ"
        );
        assert_eq!(get_youtube_id("not-a-url").unwrap(), "not-a-url");
        assert_eq!(
            get_youtube_id("https://example.com/watch?v=dQw4w9WgXcQ")
                .unwrap_err()
                .to_string(),
            "Not a YouTube URL"
        );
        assert_eq!(
            get_youtube_id("https://www.youtube.com/") // no v query, no last path segment
                .unwrap_err()
                .to_string(),
            "Malformed YouTube URL"
        );
    }
}
