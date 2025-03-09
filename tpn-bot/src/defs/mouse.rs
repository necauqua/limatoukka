use std::time::Duration;

use anyhow::{Context, Result};
use tokio::time::sleep;

use crate::commands::{args::InRange, command, context::CommandContext};

/// Position the mouse relative to the player on the screen.
///
/// For example, `look:0:0~` will move the mouse to the player's position - but
/// note that camera will move, quickly making the mouse position no longer be
/// on the player.
///
/// This is extremely useful, for example `slot:5~ look:0:-70~ hold~` to douse
/// Minä.
#[command]
async fn look(ctx: CommandContext, dx: i32, dy: i32) -> Result<()> {
    let (player_pos, camera_bounds) = ctx
        .noita
        .with(|noita| {
            let (player, _) = noita.get_player()?.context("no player entity")?;
            let bounds = noita.get_camera_bounds()?;
            Ok((player.transform.pos, bounds))
        })
        .await?;

    let [x, y, w, h] = camera_bounds;

    let px = (player_pos.x - x as f32) * 1920.0 / w as f32;
    let py = (player_pos.y - y as f32) * 1080.0 / h as f32;

    let mx = (px + dx as f32) as i32;
    let my = (py + dy as f32) as i32;

    ctx.xdo.mousemove(mx, my).await
}

/// A shortcut for `look:0:500~`.
#[command]
async fn look_right(ctx: CommandContext) -> Result<()> {
    look(ctx, 500, 0).await
}

/// A shortcut for `look:0:-500~`.
#[command]
async fn look_left(ctx: CommandContext) -> Result<()> {
    look(ctx, -500, 0).await
}

/// A shortcut for `look:500:0~`.
#[command]
async fn look_down(ctx: CommandContext) -> Result<()> {
    look(ctx, 0, 500).await
}

/// A shortcut for `look:-500:0~`.
#[command]
async fn look_up(ctx: CommandContext) -> Result<()> {
    look(ctx, 0, -500).await
}

/// Move the mouse to the absolute position on the screen.
///
/// The screen is 1920x1080, and the origin is in the center of it,
/// so `mouse:0:0~` will move the mouse to the center of the screen.
#[command]
async fn mouse(ctx: CommandContext, x: i32, y: i32) -> Result<()> {
    ctx.xdo.mousemove(960 + x, 540 + y).await
}

/// Move the mouse relative to its current position.
#[command]
async fn mouse_move(ctx: CommandContext, dx: i32, dy: i32) -> Result<()> {
    ctx.xdo.mousemove_relative(dx, dy).await
}

/// Click the left mouse button.
#[command]
async fn fire(ctx: CommandContext) -> Result<()> {
    ctx.xdo.click(1).await
}

/// Click the right mouse button.
#[command]
async fn throw(ctx: CommandContext) -> Result<()> {
    ctx.xdo.click(3).await
}

/// Start holding left mouse button down.
#[command]
async fn hold(ctx: CommandContext) -> Result<()> {
    ctx.xdo.mousedown(1).await?;

    // wait for a bit to allow the game to register the mouse down
    sleep(Duration::from_millis(50)).await;

    Ok(())
}

/// Stop holding left mouse button down.
#[command]
async fn drop(ctx: CommandContext) -> Result<()> {
    ctx.xdo.mouseup(1).await
}

static HOTBAR_XS: [i32; 24] = [
    88, 148, 208, 268, 331, 391, 451, 511, 601, 661, 721, 781, 841, 901, 961, 1021, 1081, 1141,
    1201, 1261, 1321, 1381, 1441, 1501,
];
static HOTBAR_Y: i32 = 91;

/// A helper to move the mouse to the hotbar slot without you having to guess slot coordinates.
#[command]
async fn hotbar(ctx: CommandContext, slot: InRange<1, 24>) -> Result<()> {
    ctx.xdo
        .mousemove(HOTBAR_XS[slot.get() as usize - 1], HOTBAR_Y)
        .await?;

    // wait for a bit to allow the game to register the movement
    sleep(Duration::from_millis(50)).await;

    Ok(())
}

const WAND_START_X: i32 = 105; // + 60 * 4
const WAND_START_Y: i32 = 273; // + 190 * 26

/// A helper to move the mouse to the wand slot without you having to guess slot coordinates.
#[command]
async fn wand(ctx: CommandContext, wand: InRange<1, 4>, slot: InRange<1, 26>) -> Result<()> {
    ctx.xdo
        .mousemove(
            WAND_START_X + 60 * (slot.get() as i32 - 1),
            WAND_START_Y + 190 * (wand.get() as i32 - 1),
        )
        .await?;

    // wait for a bit to allow the game to register the movement
    sleep(Duration::from_millis(50)).await;

    Ok(())
}
