use anyhow::Result;
use reqwest::Url;
use std::fmt::Write as _;

use crate::{
    commands::{
        CommandResult,
        args::{InRange, RestOfArgs},
        command,
        runner::CommandError,
    },
    context::cmd::CommandContext,
    fail,
    integration::yt_music_api::Song,
    services::{
        charges::{Charges, ChargesServiceExt},
        gates::GateServiceExt,
        messaging::PermissionLevel,
        music::{MusicError, MusicServiceExt, SongQuery, SongSource},
        sounds::SoundServiceExt,
        stats::StatsServiceExt,
        storage::StorageServiceExt,
        tts::TtsServiceExt,
    },
};

/// Say something on stream through the TTS.
#[command(sender_gate = 1m, cost=0.5, free_for = Subscriber, GlobalMacroExempt)]
async fn tts(ctx: CommandContext, msg: String, extra: RestOfArgs) -> CommandResult {
    if msg.is_empty() {
        fail!("message cannot be empty");
    }

    let msg = if extra.is_empty() {
        msg
    } else {
        std::iter::once(msg)
            .chain(extra.get(&ctx).await?.into_iter().flatten())
            .collect::<Vec<_>>()
            .join(" ")
    };

    ctx.tts().tts(&msg, Some(ctx.interrupt_signal())).await?;

    Ok(())
}

/// Play a sound on stream.
/// Sounds ids are secret, but look through `global-macro-list~` for macros that
/// use this command to get an idea of what sounds are available.
///
/// If given, the `start` parameter specifies the number of milliseconds to
/// skip from the start of the sound.
/// The `time` parameter (if given) specifies the number of milliseconds of the
/// song to actually play.
///
/// Sender gate is at least 10 seconds for everything, but individual sounds
/// have their own dynamic cooldowns.
#[command(sender_gate = 10s, permission = Caster, GlobalMacroExempt)]
async fn play_sound(
    ctx: CommandContext,
    sound_id: String,
    start: Option<u32>,
    time: Option<u32>,
) -> CommandResult {
    let sound_service = ctx.sounds();

    let Some(s) = sound_service.get_sound_entry(&sound_id).await? else {
        fail!("Sound not found");
    };

    let gate_key = format!("play-sound:{}", s.group.as_deref().unwrap_or(&*sound_id));

    if ctx.message().sender.level != PermissionLevel::Caster {
        if s.internal {
            fail!("Internal sounds not allowed in global macros");
        }

        ctx.gates()
            .command_gates(ctx.sender(), &gate_key, s.global_gate, s.sender_gate)
            .await?;

        if let Some(cost) = s.cost {
            let cost = cost.into();
            let paid = ctx.charges().consume(ctx.sender(), cost).await?;
            if !paid {
                fail!("poor (sound costs {cost})");
            }
        }
    }

    let Some(v) = s.choose() else {
        fail!("Sound had a chance of not playing, you lost to random lmao");
    };

    if v.reward != 0 {
        ctx.charges()
            .add(ctx.sender(), Charges::from(v.reward))
            .await?;
    }
    if let Some(message) = &v.message {
        ctx.reply(message.clone()).await?;
    }

    let int = s.cost.is_none().then(|| ctx.interrupt_signal());
    sound_service.play(v, start, time, int).await?;

    Ok(())
}

/// Get the title of the song that's currently playing on stream, if any.
#[command(global_gate = 5s, shortcode = np)]
async fn now_playing(ctx: CommandContext) -> CommandResult {
    match ctx.music().current().await? {
        Some((Song { author, title, .. }, SongSource::Playlist)) => {
            ctx.reply(format!("Now playing: {author} - {title} (from !playlist)",))
                .await?
        }
        Some((Song { author, title, .. }, SongSource::Request { requester })) => {
            ctx.reply(format!(
                "Now playing: {author} - {title} (requested by {requester})",
            ))
            .await?
        }
        None => ctx.fail("Nothing is playing right now").await?,
    }
    Ok(())
}

/// Get the title of the song was playing before the current one, if any.
#[command(global_gate = 5s, shortcode = lp)]
async fn last_playing(ctx: CommandContext) -> CommandResult {
    match ctx.music().last().await? {
        Some((Song { author, title, .. }, SongSource::Playlist)) => {
            ctx.reply(format!(
                "Previous song was: {author} - {title} (from !playlist)",
            ))
            .await?
        }
        Some((Song { author, title, .. }, SongSource::Request { requester })) => {
            ctx.reply(format!(
                "Previous song was: {author} - {title} (requested by {requester})",
            ))
            .await?
        }
        None => ctx.fail("No previous song?.").await?,
    }
    Ok(())
}

/// Get or set the YouTube Music volume.
#[command(sender_gate = 3s, permission = Vip, shortcode = v)]
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

/// A "back" button, undoes skips or otherwise goes back to the previous song.
#[command(global_gate = 15s, cost = 2, free_for = Vip)]
async fn unskip(ctx: CommandContext) -> CommandResult {
    if ctx.music().unskip().await? {
        ctx.reply("unskipped 😌".into()).await?
    } else {
        fail!("Nothing is playing right now")
    }
    Ok(())
}

/// Pause/unpause the music.
#[command(permission = Moderator)]
async fn pause(ctx: CommandContext, state: Option<bool>) -> CommandResult {
    let pause = match state {
        Some(s) => s,
        None => !ctx.music().is_paused().await?,
    };
    ctx.music().set_pause(pause).await?;
    if pause {
        ctx.reply("paused".into()).await?;
    } else {
        ctx.reply("unpaused".into()).await?;
    }
    Ok(())
}

fn unwrap_youtube_id(raw: &str) -> Result<String, CommandError> {
    let no_proto = !raw.starts_with("http://") && !raw.starts_with("https://");
    let url = if no_proto {
        format!("https://{raw}")
    } else {
        raw.into()
    };

    let Ok(url) = Url::parse(&url) else {
        return Ok(raw.into());
    };

    const YOUTUBE: &[&str] = &[
        "youtube.com",
        "m.youtube.com",
        "www.youtube.com",
        "music.youtube.com",
        "youtu.be",
    ];
    if !YOUTUBE.contains(&url.host_str().unwrap_or_default()) {
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
        .ok_or_else(|| CommandError::PreconditionFail("YouTube URL had no video ID".into()))
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
#[command(shortcode=sr)]
async fn song_request(
    ctx: CommandContext,
    url_or_id: String,
    extra: Option<String>,
    extra_extra: RestOfArgs,
) -> CommandResult {
    if ctx.storage().has("setting:nosr").await? {
        ctx.fail("Song requests are disabled").await?;
    }

    let url_or_id = match (&*url_or_id, extra) {
        ("https", Some(e)) if extra_extra.is_empty() => format!("https:{e}"),
        (_, extra) => std::iter::once(url_or_id)
            .chain(extra)
            .chain(extra_extra.get(&ctx).await?.into_iter().flatten())
            .collect::<Vec<_>>()
            .join(" "),
    };

    let query = unwrap_youtube_id(&url_or_id)?;
    if query == "dQw4w9WgXcQ" {
        ctx.fail("At least don't use a dQw link ICANT").await?;
    }

    let name = &ctx.message().sender.name;

    let query = if ctx.macro_depth != 0 {
        SongQuery::IdOnly(&query)
    } else {
        SongQuery::Any(&query)
    };

    match ctx.music().request(query, name).await {
        Ok(Song { author, title, video_id, length, .. }) => {
            ctx.reply(format!("Added a song to the queue: {author} - {title}"))
                .await?;
            let length = length.as_millis().to_string();
            ctx.stats().record(ctx.sender(), Some(name), "song-request", &[
                ("title", &title),
                ("author", &author),
                ("video_id", &video_id),
                ("length", length.as_str()),
            ])?;
        }
        Err(MusicError::SongNotFound) => {
            if ctx.macro_depth != 0 {
                ctx.fail("Did not find anything with that ID (song-request only accepts IDs when called from macros)").await?
            } else {
                ctx.fail("Actually did not find anything (search only searches in the music category)").await?
            }
        },
        Err(MusicError::SongAlreadyInQueue) => {
            ctx.fail("Already in the queue ICANT").await?
        },
        Err(MusicError::AgeRestricted) => {
            ctx.fail("A few select videos are so turbo-age-restricted YouTube disallows embedding them Sadge").await?
        },
        Err(MusicError::RegionRestricted) => {
            ctx.fail("Oh wow you found a video that's *actually* region-locked").await?
        },
        Err(MusicError::AgeAndRegionRestricted) => {
            ctx.fail("How tf did you find a video thats *BOTH* age- and region-locked lmao").await?
        },
        Err(MusicError::RequestLimitReached) => {
            ctx.fail("Too many of your songs in queue already").await?
        },
        Err(MusicError::Internal(e)) => return Err(CommandError::Internal(e)),
    }
    Ok(())
}

/// Removes the last song you requested from the queue.
#[command(sender_gate = 5s, shortcode = cr)]
async fn cancel_request(ctx: CommandContext) -> CommandResult {
    match ctx.music().cancel_last(&ctx.message().sender.name).await? {
        Some(Song { title, author, .. }) => {
            ctx.gates().ungate(ctx.sender(), "song-request").await?;
            ctx.reply(format!("Removed from queue: {author} - {title}",))
                .await?
        }
        None => ctx.fail("Queue had no songs requested by you").await?,
    }

    Ok(())
}

/// List the songs that were requested through `song-request~`.
#[command(global_gate = 30s, shortcode = mq)]
async fn music_queue(ctx: CommandContext, top: Option<u32>) -> CommandResult {
    let queue = ctx.music().queue().await?;
    if queue.is_empty() {
        ctx.reply("Queue is empty".into()).await?;
        return Ok(());
    }

    let mut response = String::new();

    for (Song { author, title, .. }, requester) in queue.iter().take(top.unwrap_or(999999) as _) {
        if !response.is_empty() {
            response.push_str(";\n");
        }

        match requester {
            SongSource::Request { requester } => {
                write!(
                    &mut response,
                    "{author} - {title} (requested by {requester})",
                )
                .unwrap();
            }
            SongSource::Playlist => {
                write!(&mut response, "{author} - {title}").unwrap();
            }
        }
    }

    ctx.reply(response).await?;

    Ok(())
}

/// A helper command to help fix potential music queue issues.
#[command(permission = Moderator, shortcode = cmq)]
async fn clear_music_queue(ctx: CommandContext) -> CommandResult {
    ctx.music().clear().await?;
    ctx.reply("queue cursor reset".into()).await?;

    Ok(())
}

#[cfg(test)]
mod tests {

    use anyhow::Result;

    use crate::{
        commands::{discover_declared_commands, runner::Runner},
        context::app::AppContext,
        logging,
        services::Injector,
        testing::{self, MessageExt},
    };

    use super::*;

    #[test]
    fn test_get_youtube_id() {
        assert_eq!(
            unwrap_youtube_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap(),
            "dQw4w9WgXcQ"
        );
        assert_eq!(
            unwrap_youtube_id("https://youtube.com/watch?v=dQw4w9WgXcQ").unwrap(),
            "dQw4w9WgXcQ"
        );
        assert_eq!(
            unwrap_youtube_id("https://youtu.be/dQw4w9WgXcQ").unwrap(),
            "dQw4w9WgXcQ"
        );
        assert_eq!(
            unwrap_youtube_id("https://m.youtube.com/watch?v=dQw4w9WgXcQ").unwrap(),
            "dQw4w9WgXcQ"
        );
        assert_eq!(
            unwrap_youtube_id("https://music.youtube.com/watch?v=dQw4w9WgXcQ").unwrap(),
            "dQw4w9WgXcQ"
        );
        assert_eq!(
            unwrap_youtube_id("youtu.be/dQw4w9WgXcQ").unwrap(),
            "dQw4w9WgXcQ"
        );
        assert_eq!(unwrap_youtube_id("not-a-url").unwrap(), "not-a-url");
        assert_eq!(
            unwrap_youtube_id("https://example.com/watch?v=dQw4w9WgXcQ")
                .unwrap_err()
                .to_string(),
            "Not a YouTube URL"
        );
        assert_eq!(
            unwrap_youtube_id("https://www.youtube.com/") // no v query, no last path segment
                .unwrap_err()
                .to_string(),
            "Malformed YouTube URL"
        );
    }

    #[tokio::test]
    async fn song_request_extra_param() -> Result<()> {
        let _guard = logging::init();
        let ctx = AppContext::new(Injector::new());
        let runner = Runner::new(discover_declared_commands(), &ctx);

        runner
            .process_message(
                ctx.clone(),
                testing::message(" sr:https://youtu.be/dQw4w9WgXcQ ")
                    .permission(PermissionLevel::Caster),
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
