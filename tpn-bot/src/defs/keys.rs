use std::time::Duration;

use tokio::time::timeout;

use super::HoldTime;
use crate::{
    commands::{CommandResult, args::InRange, command},
    context::cmd::CommandContext,
    services::noita::NoitaEvent,
};

async fn mv(ctx: CommandContext, duration: HoldTime, key: &'static str) -> CommandResult {
    super::hold(
        ctx,
        duration,
        key,
        async |ctx| Ok(ctx.xdo().keydown(key).await?),
        async |ctx| Ok(ctx.xdo().keyup(key).await?),
    )
    .await
}

/// Hold <kbd>W</kbd> for the specified duration.
#[command(shortcode=u, NoitaControl)]
async fn up(ctx: CommandContext, duration: HoldTime) -> CommandResult {
    mv(ctx, duration, "w").await
}

/// Hold <kbd>A</kbd> for the specified duration.
#[command(shortcode=l, NoitaControl)]
async fn left(ctx: CommandContext, duration: HoldTime) -> CommandResult {
    mv(ctx, duration, "a").await
}

/// Hold <kbd>S</kbd> for the specified duration.
#[command(shortcode=d, NoitaControl)]
async fn down(ctx: CommandContext, duration: HoldTime) -> CommandResult {
    mv(ctx, duration, "s").await
}

/// Hold <kbd>D</kbd> for the specified duration.
#[command(shortcode=r, NoitaControl)]
async fn right(ctx: CommandContext, duration: HoldTime) -> CommandResult {
    mv(ctx, duration, "d").await
}

/// Press <kbd>E</kbd>.
#[command(shortcode=i, NoitaControl)]
async fn interact(ctx: CommandContext) -> CommandResult {
    ctx.xdo().key("e").await?;
    Ok(())
}

/// Press <kbd>F</kbd>.
#[command(shortcode=k, NoitaControl)]
async fn kick(ctx: CommandContext) -> CommandResult {
    ctx.xdo().key("f").await?;
    Ok(())
}

/// Press <kbd>1</kbd> through <kbd>8</kbd>.
#[command(shortcode=s, NoitaControl)]
async fn slot(ctx: CommandContext, slot: InRange<1, 8>) -> CommandResult {
    ctx.xdo().key(&slot.get().to_string()).await?;
    Ok(())
}

/// Press <kbd>Tab</kbd> _only if_ the inventory is closed.
///
/// This command also waits for the game inventory state to actually change.
/// Also this command moves the mouse to the top left corner of the screen to
/// force mouse movement after opening the inventory, since if you don't move
/// it the game does not register hovering over slots lol.
#[command(shortcode=o, NoitaControl)]
async fn open_inventory(ctx: CommandContext) -> CommandResult {
    if ctx.noita().is_inventory_open() {
        return Ok(());
    }

    let f = timeout(Duration::from_millis(100), async {
        let mut events = ctx.noita().subscribe();
        while !matches!(
            events.recv().await,
            Ok(NoitaEvent::InventoryOpened) | Err(_)
        ) {}
    });

    ctx.xdo().key("Tab").await?;
    _ = f.await;

    Ok(())
}

/// Press <kbd>Tab</kbd> _only if_ the inventory is opened.
///
/// This command also waits for the game inventory state to actually change.
#[command(shortcode=x, NoitaControl)]
async fn close_inventory(ctx: CommandContext) -> CommandResult {
    if !ctx.noita().is_inventory_open() {
        return Ok(());
    }

    let f = timeout(Duration::from_millis(100), async {
        let mut events = ctx.noita().subscribe();
        while !matches!(
            events.recv().await,
            Ok(NoitaEvent::InventoryClosed) | Err(_)
        ) {}
    });

    ctx.xdo().key("Tab").await?;
    _ = f.await;

    Ok(())
}

/// Pause the game.
///
/// This actually just presses the <kbd>Esc</kbd> key.
///
/// And yes, chatters will be able to move the mouse around and click stuff, so
/// this command is kinda annoying without `full-stop~` as they could mess up
/// the settings or start a different gamemode.
#[command(permission = Moderator, NoitaControl)]
async fn pause(ctx: CommandContext) -> CommandResult {
    ctx.xdo().key("Escape").await?;
    Ok(())
}
