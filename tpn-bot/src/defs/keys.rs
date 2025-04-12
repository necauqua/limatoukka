use anyhow::Result;

use super::HoldTime;
use crate::commands::{args::InRange, command, context::CommandContext};

async fn mv(ctx: CommandContext, duration: HoldTime, key: &'static str) -> Result<()> {
    super::hold(
        ctx,
        duration,
        key,
        |ctx| ctx.xdo.keydown(key),
        |ctx| ctx.xdo.keyup(key),
    )
    .await
}

/// Hold <kbd>W</kbd> for the specified duration.
#[command(shortcode=u)]
async fn up(ctx: CommandContext, duration: HoldTime) -> Result<()> {
    mv(ctx, duration, "w").await
}

/// Hold <kbd>A</kbd> for the specified duration.
#[command(shortcode=l)]
async fn left(ctx: CommandContext, duration: HoldTime) -> Result<()> {
    mv(ctx, duration, "a").await
}

/// Hold <kbd>S</kbd> for the specified duration.
#[command(shortcode=d)]
async fn down(ctx: CommandContext, duration: HoldTime) -> Result<()> {
    mv(ctx, duration, "s").await
}

/// Hold <kbd>D</kbd> for the specified duration.
#[command(shortcode=r)]
async fn right(ctx: CommandContext, duration: HoldTime) -> Result<()> {
    mv(ctx, duration, "d").await
}

/// Press <kbd>E</kbd>.
#[command(shortcode=i)]
async fn interact(ctx: CommandContext) -> Result<()> {
    ctx.xdo.key("e").await
}

/// Press <kbd>F</kbd>.
#[command(shortcode=k)]
async fn kick(ctx: CommandContext) -> Result<()> {
    ctx.xdo.key("f").await
}

/// Press <kbd>1</kbd> through <kbd>8</kbd>.
#[command(shortcode=s)]
async fn slot(ctx: CommandContext, slot: InRange<1, 8>) -> Result<()> {
    ctx.xdo.key(&slot.get().to_string()).await
}

/// Press <kbd>Tab</kbd> _only if_ the inventory is closed.
///
/// This command also waits for the game inventory state to actually change.
/// Also this command moves the mouse to the top left corner of the screen to
/// force mouse movement after opening the inventory, since if you don't move
/// it the game does not register hovering over slots lol.
#[command(shortcode=o)]
async fn open_inventory(ctx: CommandContext) -> Result<()> {
    if !ctx.noita.is_inventory_open() {
        let f = ctx.noita.wait_for_inventory_open();
        ctx.xdo.mousemove(0, 0).await?;
        ctx.xdo.key("Tab").await?;
        f.await;
    }
    Ok(())
}

/// Press <kbd>Tab</kbd> _only if_ the inventory is opened.
///
/// This command also waits for the game inventory state to actually change.
#[command(shortcode=x)]
async fn close_inventory(ctx: CommandContext) -> Result<()> {
    if ctx.noita.is_inventory_open() {
        let f = ctx.noita.wait_for_inventory_close();
        ctx.xdo.key("Tab").await?;
        f.await;
    }
    Ok(())
}

/// Pause the game.
///
/// This actually just presses the <kbd>Esc</kbd> key.
///
/// And yes, chatters will be able to move the mouse around and click stuff, so
/// this command is kinda annoying without `full-stop~` as they could mess up
/// the settings or start a different gamemode.
#[command(permission = Moderator)]
async fn pause(ctx: CommandContext) -> Result<()> {
    ctx.xdo.key("Escape").await
}
