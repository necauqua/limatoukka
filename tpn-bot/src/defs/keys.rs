use anyhow::Result;

use crate::commands::{
    args::{AtMost, InRange},
    command,
    context::CommandContext,
};

async fn mv(ctx: CommandContext, millis: Option<AtMost<5000>>, key: &'static str) -> Result<()> {
    super::hold(
        ctx,
        millis,
        key,
        |ctx| ctx.xdo.keydown(key),
        |ctx| ctx.xdo.keyup(key),
    )
    .await
}

/// Hold <kbd>W</kbd> for a duration of 1-5000ms, defaulting to 500.
#[command(long)]
async fn up(ctx: CommandContext, millis: Option<AtMost<5000>>) -> Result<()> {
    mv(ctx, millis, "w").await
}

/// Hold <kbd>A</kbd> for a duration of 1-5000ms, defaulting to 500.
#[command(long)]
async fn left(ctx: CommandContext, millis: Option<AtMost<5000>>) -> Result<()> {
    mv(ctx, millis, "a").await
}

/// Hold <kbd>S</kbd> for a duration of 1-5000ms, defaulting to 500.
#[command(long)]
async fn down(ctx: CommandContext, millis: Option<AtMost<5000>>) -> Result<()> {
    mv(ctx, millis, "s").await
}

/// Hold <kbd>D</kbd> for a duration of 1-5000ms, defaulting to 500.
#[command(long)]
async fn right(ctx: CommandContext, millis: Option<AtMost<5000>>) -> Result<()> {
    mv(ctx, millis, "d").await
}

/// Press <kbd>E</kbd>.
#[command]
async fn interact(ctx: CommandContext) -> Result<()> {
    ctx.xdo.key("e").await
}

/// Press <kbd>F</kbd>.
#[command]
async fn kick(ctx: CommandContext) -> Result<()> {
    ctx.xdo.key("f").await
}

/// Press <kbd>1</kbd> through <kbd>8</kbd>.
#[command]
async fn slot(ctx: CommandContext, slot: InRange<1, 8>) -> Result<()> {
    ctx.xdo.key(&slot.get().to_string()).await
}

/// Press <kbd>Tab</kbd> _only if_ the inventory is closed.
///
/// This command also waits for the game inventory state to actually change.
/// Also this command moves the mouse to the top left corner of the screen to
/// force mouse movement after opening the inventory, since if you don't move
/// it the game does not register hovering over slots lol.
#[command]
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
#[command]
async fn close_inventory(ctx: CommandContext) -> Result<()> {
    if ctx.noita.is_inventory_open() {
        let f = ctx.noita.wait_for_inventory_close();
        ctx.xdo.key("Tab").await?;
        f.await;
    }
    Ok(())
}
