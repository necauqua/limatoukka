use std::time::Duration;

use anyhow::{Context, Result};
use futures::FutureExt;
use rustis::commands::StringCommands;
use tokio::time::sleep;

use crate::{
    commands::{
        args::{HoldTime, InRange},
        command,
        context::CommandContext,
    },
    fail,
    services::noita::NoitaHandle,
};

async fn player_screen_pos(noita: &NoitaHandle) -> Result<(f32, f32)> {
    let (player_pos, [x, y, w, h]) = noita
        .with(|noita| {
            let (player, _) = noita.get_player()?.context("no player entity")?;
            let bounds = noita.get_camera_bounds()?;
            Ok((player.transform.pos, bounds))
        })
        .await?;

    Ok((
        (player_pos.x - x as f32) * 1920.0 / w as f32,
        (player_pos.y - y as f32) * 1080.0 / h as f32,
    ))
}

/// Position the mouse relative to the player on the screen.
///
/// For example, `look:0:0~` will move the mouse to the player's position - but
/// note that camera will move, quickly making the mouse position no longer be
/// on the player.
///
/// This is extremely useful, for example `slot:5~ look:0:-70~ hold~` to douse
/// Minä.
#[command]
async fn look(ctx: CommandContext, dx: i32, dy: i32, duration: HoldTime<0>) -> Result<()> {
    if duration.get() == Duration::ZERO {
        let (px, py) = player_screen_pos(ctx.noita()).await?;
        let mx = (px + dx as f32) as i32;
        let my = (py + dy as f32) as i32;
        ctx.xdo().mousemove(mx, my).await?;
        return Ok(());
    }

    let (sx, sy) = ctx.xdo().getmouselocation().await?;

    let millis = 5_u32;
    let mut interval = tokio::time::interval(Duration::from_millis(millis as _));
    let ticks = ((duration.get().as_millis() as u32) / millis).max(1);

    for i in 0..ticks {
        ctx.holds()
            .interruptible(interval.tick().map(|_| ()))
            .await?;

        let t = (i + 1) as f32 / ticks as f32;

        let (px, py) = player_screen_pos(ctx.noita()).await?;
        let ex = px + dx as f32;
        let ey = py + dy as f32;

        let ix = sx as f32 + (ex - sx as f32) * t;
        let iy = sy as f32 + (ey - sy as f32) * t;

        ctx.xdo().mousemove(ix as _, iy as _).await?;
    }
    Ok(())
}

/// Move the mouse to the absolute position on the screen.
///
/// The screen is 1920x1080, and the origin is in the center of it,
/// so `mouse:0:0~` will move the mouse to the center of the screen.
#[command(shortcode=m)]
async fn mouse(ctx: CommandContext, x: i32, y: i32, duration: HoldTime<0>) -> Result<()> {
    let duration = duration.get();
    if duration == Duration::ZERO {
        ctx.xdo().mousemove(960 + x, 540 + y).await?;
        return Ok(());
    }

    let (sx, sy) = ctx.xdo().getmouselocation().await?;
    let dx = (960 + x - sx as i32) as f32;
    let dy = (540 + y - sy as i32) as f32;

    let millis = 5_u32;
    let mut interval = tokio::time::interval(Duration::from_millis(millis as _));
    let ticks = ((duration.as_millis() as u32) / millis).max(1);

    for i in 0..ticks {
        ctx.holds()
            .interruptible(interval.tick().map(|_| ()))
            .await?;

        let t = (i + 1) as f32 / ticks as f32;

        let ix = sx as f32 + dx * t;
        let iy = sy as f32 + dy * t;

        ctx.xdo().mousemove(ix as _, iy as _).await?;
    }
    Ok(())
}

/// Move the mouse relative to its current position.
#[command(shortcode=mm)]
async fn mouse_move(ctx: CommandContext, dx: i32, dy: i32) -> Result<()> {
    ctx.xdo().mousemove_relative(dx, dy).await
}

/// Click the left mouse button.
#[command(shortcode=c)]
async fn click(ctx: CommandContext) -> Result<()> {
    if ctx
        .storage()
        .get::<_, Option<i64>>("holds:lmb")
        .await?
        .unwrap_or_default()
        != 0
    {
        fail!("click during hold")
    }
    ctx.xdo().click(1).await
}

/// Click the right mouse button.
#[command]
async fn throw(ctx: CommandContext) -> Result<()> {
    ctx.xdo().click(3).await
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
        |ctx| ctx.xdo().mousedown(1),
        |ctx| ctx.xdo().mouseup(1),
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
    ctx.xdo()
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
    ctx.xdo()
        .mousemove(
            WAND_START_X + 60 * (slot.get() as i32 - 1),
            WAND_START_Y + 190 * (wand.get() as i32 - 1),
        )
        .await?;

    // wait for a bit to allow the game to register the movement
    sleep(Duration::from_millis(50)).await;

    Ok(())
}
