use anyhow::Result;
use reqwest::Url;
use std::fmt::Write as _;

use crate::{
    commands::{CommandResult, args::InRange, command, runner::CommandError},
    context::cmd::CommandContext,
    fail,
    services::{
        charges::{Charges, ChargesServiceExt},
        gates::GateServiceExt,
        messaging::PermissionLevel,
        music::{AddSongError, MusicServiceExt, Song},
        sounds::SoundServiceExt,
        storage::StorageServiceExt,
        tts::TtsServiceExt,
    },
};

/// Say something on stream through the TTS.
#[command(sender_gate = 1m, cost=0.5, free_for = Subscriber, GlobalMacroExempt)]
async fn tts(ctx: CommandContext, msg: String) -> CommandResult {
    if msg.is_empty() {
        fail!("message cannot be empty");
    }

    ctx.tts().tts(&msg, Some(ctx.interrupt_signal())).await?;

    Ok(())
}

/// Play a sound on stream.
/// Sounds ids are secret, but look through `global-macro-list~` for macros that
/// use this command to get an idea of what sounds are available.
///
/// Sender gate is at least 10 seconds for everything, but individual sounds
/// have their own dynamic cooldowns.
#[command(sender_gate = 10s, permission = Caster, GlobalMacroExempt)]
async fn play_sound(ctx: CommandContext, sound_id: String) -> CommandResult {
    let sound_service = ctx.sounds();

    let Some(s) = sound_service.select(&sound_id).await? else {
        fail!("Sound not found");
    };

    let gate_key = format!("play-sound:{}", s.group.as_deref().unwrap_or(&*sound_id));

    if ctx.message().sender.level != PermissionLevel::Caster {
        ctx.gates()
            .command_gates(ctx.sender(), &gate_key, s.global_gate, s.sender_gate)
            .await?;
    }

    let Some(s) = s.choose() else {
        fail!("Sound had a chance of not playing, you lost to random lmao");
    };

    if s.reward != 0 {
        ctx.charges()
            .add(ctx.sender(), Charges::from(s.reward))
            .await?;
    }

    sound_service.play(s, Some(ctx.interrupt_signal())).await?;

    Ok(())
}

/// Get the title of the song that's currently playing on stream, if any.
#[command(global_gate = 5s, shortcode = np)]
async fn now_playing(ctx: CommandContext) -> CommandResult {
    match ctx.music().current().await? {
        Some(title) => ctx.send(format!("Now playing: {title}")).await?,
        None => ctx.fail("Nothing is playing right now").await?,
    }
    Ok(())
}

/// Skips the song that's currently playing on stream, if any.
#[command(global_gate = 15s, cost = 1, free_for = Vip)]
async fn skip(ctx: CommandContext) -> CommandResult {
    if ctx.music().skip().await? {
        ctx.reply("song skipped Madge".into()).await?
    } else {
        fail!("Nothing is playing right now")
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
///
/// The `extra` parameter is used to allow specifying full URLs without quotes.
/// For example, `sr:https://youtu.be/dQw4w9WgXcQ` <- here the first argument
/// is actually `"https"` and the `extra` is `"//youtu.be/dQw4w9WgXcQ"`.
#[command(sender_gate = 1m, shortcode=sr)]
async fn song_request(
    ctx: CommandContext,
    url_or_id: String,
    extra: Option<String>,
) -> CommandResult {
    if ctx.storage().has("setting:nosr").await? {
        ctx.fail("Song requests are disabled").await?;
    }

    let url_or_id = match extra {
        Some(e) => format!("{url_or_id}:{e}"),
        None => url_or_id,
    };

    let id = get_youtube_id(&url_or_id)?;
    if id == "dQw4w9WgXcQ" {
        ctx.fail("At least don't use a dQw link ICANT").await?;
    }

    match ctx.music().add(&id).await {
        Ok(Song { author, title, .. }) => {
            ctx.storage()
                .set(&format!("song-requester:{id}"), &ctx.message().sender.name)
                .await?;
            ctx.send(format!("Added a song to the queue: {author} - {title}"))
                .await?;
        }
        Err(AddSongError::Internal(e)) => return Err(CommandError::Internal(e)),
        Err(e) => ctx.fail(e.to_string()).await?,
    }
    Ok(())
}

/// List the songs that were requested through `song-request~`.
#[command(global_gate = 30s)]
async fn music_queue(ctx: CommandContext, top: Option<u32>) -> CommandResult {
    let storage = ctx.storage();
    let queue = ctx.music().queue().await?;
    if queue.is_empty() {
        ctx.send("Queue is empty".into()).await?;
        // todo could cleanup all song-requester:* keys here somehow
        return Ok(());
    }

    let mut response = String::new();

    for Song {
        author,
        title,
        video_id,
    } in queue.iter().take(top.unwrap_or(999999) as _)
    {
        if !response.is_empty() {
            response.push_str(";\n");
        }

        let requester = storage
            .get(&format!("song-requester:{video_id}"))
            .await?
            .unwrap_or_default();

        if requester.is_empty() {
            write!(&mut response, "{author} - {title}").unwrap();
        } else {
            write!(
                &mut response,
                "{author} - {title} (requested by {requester})",
            )
            .unwrap();
        }
    }

    ctx.reply(response).await?;

    Ok(())
}

/// A helper command to help fix potential music queue issues.
#[command(permission = Moderator)]
async fn music_queue_reset(ctx: CommandContext) -> CommandResult {
    ctx.music().queue_reset().await?;
    ctx.reply("queue cursor reset".into()).await?;

    Ok(())
}

/// Get or set the YouTube Music volume.
#[command(sender_gate = 3s, permission = Vip)]
async fn volume(ctx: CommandContext, volume: Option<InRange<0, 100>>) -> CommandResult {
    let music_service = ctx.music();
    match volume {
        Some(volume) => music_service.set_volume(volume.get()).await?,
        None => {
            let volume = music_service.get_volume().await?;
            ctx.send(format!("Current volume is {volume}%")).await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {

    use anyhow::Result;

    use crate::{
        commands::{discover_declared_commands, runner::Runner},
        logging, testing,
    };

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

    #[tokio::test]
    async fn song_request_extra_param() -> Result<()> {
        _ = logging::init();
        let ctx = testing::mock_context();
        let runner = Runner::new(discover_declared_commands(), &ctx);

        runner
            .process_message(
                ctx.clone(),
                testing::message(" sr:https://youtu.be/dQw4w9WgXcQ "),
            )
            .await?;

        let err = ctx.storage().get("last-error:mock-sender-id").await?;
        assert_eq!(
            err.as_deref(),
            Some("sr(0:0): At least don't use a dQw link ICANT")
        );

        Ok(())
    }
}
