use std::{collections::HashMap, sync::LazyLock, time::Duration};

use anyhow::{Context, Result};
use futures::FutureExt;
use noita_engine_reader::{
    Noita, PlayerState,
    memory::MemoryStorage,
    types::components::{AbilityComponent, ItemComponent},
};
use rustis::commands::StringCommands;
use tokio::time::sleep;

use crate::{
    commands::{
        args::{HoldTime, InRange},
        command,
    },
    context::cmd::CommandContext,
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
        ctx.interruptible(interval.tick().map(|_| ())).await?;

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
        ctx.interruptible(interval.tick().map(|_| ())).await?;

        let t = (i + 1) as f32 / ticks as f32;

        let ix = sx as f32 + dx * t;
        let iy = sy as f32 + dy * t;

        ctx.xdo().mousemove(ix as _, iy as _).await?;
    }
    Ok(())
}

/// Move the mouse relative to its current position.
#[command(shortcode=mm)]
async fn mouse_move(ctx: CommandContext, dx: i32, dy: i32, duration: HoldTime<0>) -> Result<()> {
    if duration.get() == Duration::ZERO {
        return ctx.xdo().mousemove_relative(dx, dy).await;
    }

    let (sx, sy) = ctx.xdo().getmouselocation().await?;
    mouse(ctx, sx as i32 - 960 + dx, sy as i32 - 540 + dy, duration).await
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

/// A helper to move the mouse to the wand slot without you having to guess slot coordinates.
///
/// Especially useful as it takes into account different heights of wand sprites.
#[command]
async fn wand(ctx: CommandContext, wand: InRange<1, 4>, slot: InRange<1, 26>) -> Result<()> {
    if !ctx.noita().is_inventory_open() {
        fail!("inventory not open")
    }

    let wand_heights = ctx.noita().with(read_wands_sizes).await?;

    let wand = wand.get() as usize;
    let slot = slot.get() as i32;

    if wand > wand_heights.len() {
        fail!("no wand {wand}");
    }
    let (_, capacity) = wand_heights[wand - 1];
    if slot > capacity {
        fail!("wand {wand} has only {capacity} slots, not {slot}",);
    }

    let offset_y: i32 = wand_heights
        .into_iter()
        .take(wand)
        .map(|(h, _)| 142 + h * 6)
        .sum();

    ctx.xdo()
        .mousemove(105 + 60 * (slot - 1), 87 + offset_y)
        .await?;

    // wait for a bit to allow the game to register the movement
    sleep(Duration::from_millis(50)).await;

    Ok(())
}

fn read_wands_sizes(noita: &mut Noita) -> Result<Vec<(i32, i32)>> {
    let Some((entity, PlayerState::Normal)) = noita.get_player()? else {
        return Ok(vec![]);
    };

    let p = noita.proc().clone();

    let Some(wand_tag) = noita.get_entity_tag_index("wand")? else {
        return Ok(vec![]);
    };

    let mut inv_quick = None;
    for child in entity.children.read(&p)?.read(&p)? {
        let child = child.read(&p)?;
        if &*child.name.read(&p)? == "inventory_quick" {
            inv_quick = Some(child);
            break;
        }
    }

    let inv_quick = inv_quick.context("no inventory")?;
    let item_store = noita.component_store::<ItemComponent>()?;
    let ability_store = noita.component_store::<AbilityComponent>()?;

    let mut wands = Vec::with_capacity(4);

    for child in inv_quick.children.read(&p)?.read(&p)? {
        let child = child.read(&p)?;
        if !child.tags[wand_tag] {
            continue;
        }
        let Some(ability) = ability_store.get(&child)? else {
            continue;
        };
        let Some(item) = item_store.get(&child)? else {
            continue;
        };
        let sprite = ability.sprite_file.read(&p)?;
        let height = SPRITE_HEIGHTS.get(&sprite).cloned().unwrap_or(8); // 8 is the height of the starter idk
        wands.push((
            item.inventory_slot.x,
            height,
            ability.gun_config.deck_capacity,
        ));
    }
    wands.sort_by_key(|(x, _, _)| *x);

    Ok(wands.into_iter().map(|(_, h, c)| (h, c)).collect())
}

static SPRITE_HEIGHTS: LazyLock<HashMap<String, i32>> =
    LazyLock::new(|| serde_yml::from_str(include_str!("../../data/sprite-heights.yml")).unwrap());
