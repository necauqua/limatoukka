use std::time::Duration;

use anyhow::Result;
use reqwest::Url;
use serde::Deserialize;

use crate::{
    commands::{CommandResult, args::InRange, command, runner::CommandError},
    context::{app::AppContext, cmd::CommandContext},
    fail,
    services::{
        sounds::{SoundError, SoundService},
        tts::TtsService,
    },
};

/// Say something on stream through the TTS.
#[command(sender_gate = 1m, cost=0.5, free_for = Subscriber, GlobalMacroExempt)]
async fn tts(ctx: CommandContext, msg: String) -> CommandResult {
    if msg.is_empty() {
        fail!("message cannot be empty");
    }

    ctx.service::<dyn TtsService>()
        .tts(&msg, Some(ctx.interrupt_signal()))
        .await?;

    Ok(())
}

/// Play a sound on stream.
/// Sounds ids are secret, but look through `global-macro-list~` for macros that
/// use this command to get an idea of what sounds are available.
///
/// Sender gate is at least 10 seconds for everything, but individual sounds
/// can have their own dynamic cooldowns (currently all sounds at 1 minute).
#[command(sender_gate=10s, permission = Caster, GlobalMacroExempt)]
async fn play_sound(ctx: CommandContext, sound_id: String) -> CommandResult {
    if !ctx
        .sender_gate(&format!("play-sound:{sound_id}"), Duration::from_secs(60))
        .await?
    {
        fail!("sender gate 1m for {sound_id}");
    }

    match ctx
        .service::<dyn SoundService>()
        .play(&sound_id, Some(ctx.interrupt_signal()))
        .await
    {
        Ok(_) => Ok(()),
        Err(e @ (SoundError::NotFound | SoundError::DidntChoose)) => fail!("{e}"),
        Err(SoundError::InternalError(e)) => Err(e.into()),
    }
}

/// Get the title of the song that's currently playing on stream, if any.
#[command(global_gate = 5s, shortcode = np)]
async fn now_playing(ctx: CommandContext) -> CommandResult {
    match AppContext::just("music-np", &[])?.get().await? {
        Ok(title) => ctx.send(format!("Now playing: {title}")).await?,
        Err(_) => fail!("Nothing is playing right now"),
    }
    Ok(())
}

/// Skips the song that's currently playing on stream, if any.
#[command(global_gate = 15s, cost = 1, free_for = Vip)]
async fn skip(ctx: CommandContext) -> CommandResult {
    match AppContext::just("music-skip", &[])?.get().await? {
        Ok(_) => ctx.reply("song skipped Madge".into()).await?,
        Err(_) => fail!("Nothing is playing right now"),
    }
    Ok(())
}

fn get_youtube_id(raw: &str) -> Result<String, CommandError> {
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
        fail!("Not a YouTube URL");
    }

    url.query_pairs()
        .find(|(k, _)| k == "v")
        .map(|(_, v)| v.into())
        .or_else(|| {
            url.path_segments()
                .and_then(|mut s| s.rfind(|s| !s.is_empty()))
                .map(|s| s.into())
        })
        .ok_or_else(|| CommandError::PreconditionFail("Malformed YouTube URL".into()))
}

/// Adds the given song to the YouTube Music queue.
///
/// This only accepts YouTube links or IDs.
///
/// Note that the song will be added to the front of the queue - this is
/// because usually the queue is full of songs from my stream playlist and the
/// point of the command is to show me a song you think I wont insta-skip :)
#[command(sender_gate = 1m, shortcode=sr)]
async fn song_request(ctx: CommandContext, url_or_id: String) -> CommandResult {
    let id = get_youtube_id(&url_or_id)?;
    if id == "dQw4w9WgXcQ" {
        ctx.reply("At least don't use a dQw link ICANT".into())
            .await?;
        return Ok(());
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
            .await?;
        }
        AddResponse::Borked { reason } => {
            ctx.reply(format!("Failed to add song, stated reason: {reason}"))
                .await?;
        }
        AddResponse::AlreadyInQueue { .. } => ctx.reply("already in queue 🤦".into()).await?,
    }
    Ok(())
}

/// Set YouTube Music volume.
#[command(sender_gate = 3s, permission = Vip)]
async fn volume(_ctx: CommandContext, volume: InRange<0, 100>) -> CommandResult {
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
