use std::time::Duration;

use anyhow::{Context, Result};
use rustis::commands::StringCommands;
use tokio::time::sleep;

use crate::{
    commands::{args::InRange, command, context::CommandContext},
    fail,
};

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

/// A shortcut for `look:500:0~`.
#[command(shortcode=lr)]
async fn look_right(ctx: CommandContext) -> Result<()> {
    look(ctx, 500, 0).await
}

/// A shortcut for `look:-500:0~`.
#[command(shortcode=ll)]
async fn look_left(ctx: CommandContext) -> Result<()> {
    look(ctx, -500, 0).await
}

/// A shortcut for `look:0:500~`.
#[command(shortcode=ld)]
async fn look_down(ctx: CommandContext) -> Result<()> {
    look(ctx, 0, 500).await
}

/// A shortcut for `look:0:-500~`.
#[command(shortcode=lu)]
async fn look_up(ctx: CommandContext) -> Result<()> {
    look(ctx, 0, -500).await
}

/// A shortcut for `look:500:-500~`.
#[command(shortcode=lur)]
async fn look_up_right(ctx: CommandContext) -> Result<()> {
    look(ctx, 500, -500).await
}

/// A shortcut for `look:-500:-500~`.
#[command(shortcode=lul)]
async fn look_up_left(ctx: CommandContext) -> Result<()> {
    look(ctx, -500, -500).await
}

/// A shortcut for `look:500:500~`.
#[command(shortcode=ldr)]
async fn look_down_right(ctx: CommandContext) -> Result<()> {
    look(ctx, 500, 500).await
}

/// A shortcut for `look:-500:500~`.
#[command(shortcode=ldl)]
async fn look_down_left(ctx: CommandContext) -> Result<()> {
    look(ctx, -500, 500).await
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
#[command(shortcode=mm)]
async fn mouse_move(ctx: CommandContext, dx: i32, dy: i32) -> Result<()> {
    ctx.xdo.mousemove_relative(dx, dy).await
}

/// Click the left mouse button.
#[command(shortcode=c)]
async fn click(ctx: CommandContext) -> Result<()> {
    if ctx
        .storage
        .get::<_, Option<i64>>("holds:lmb")
        .await?
        .unwrap_or_default()
        != 0
    {
        fail!("click during hold")
    }
    ctx.xdo.click(1).await
}

/// Click the right mouse button.
#[command]
async fn throw(ctx: CommandContext) -> Result<()> {
    ctx.xdo.click(3).await
}

/// Hold left mouse button down for the specified duration of milliseconds (defaulting to 500).
///
/// An example of consistently dragging an item in your inventory:
/// ```tpn
/// open-inventory~ hotbar:5~ hold~ close-inventory~ | wait:250~ hotbar:8~
/// ```
/// You can replace `hotbar:8~` with something like `mouse:0:0~` if you want a
/// little tomfoolery, but that will get you voted into the shadow realm.
#[command(shortcode=h)]
async fn hold(ctx: CommandContext, millis: super::HoldTime) -> Result<()> {
    super::hold(
        ctx,
        millis,
        "lmb",
        |ctx| ctx.xdo.mousedown(1),
        |ctx| ctx.xdo.mouseup(1),
    )
    .await
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

/// A shortcut for `mouse:-100:85~`.
///
/// Moves the mouse to the position of the 'Restore' button on the autosave
/// screen. You can type `restore-autosave~ click~` to click it real fast.
#[command]
async fn restore_autosave(ctx: CommandContext) -> Result<()> {
    ctx.xdo.mousemove(860, 625).await
}
