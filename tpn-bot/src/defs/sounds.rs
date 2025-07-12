use anyhow::Result;

use crate::{
    commands::command,
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

    tracing::debug!(msg, "sending TTS");

    let mut process = AppContext::aws_tts(&msg)?;
    if process.wait(ctx.wait_for_interrupt()).await {
        tracing::debug!(msg, "finished TTS")
    } else {
        tracing::debug!(msg, "interrupted TTS");
    }

    Ok(())
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

    if let Err(e @ (SoundError::NotFound | SoundError::DidntChoose)) =
        ctx.sounds().play_sound(&sound_id, int).await
    {
        fail!("{e}")
    };

    Ok(())
}
