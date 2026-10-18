use std::{sync::Arc, time::Duration};

use anyhow::Result;
use futures::FutureExt;
use limatoukka::{
    bot::{self, BotServices, StateStore},
    config::Config,
    context::app::{AppContext, InterruptKind},
    events::{ChannelEvent, handle_channel_event, handle_kofi_event, handle_noita_event},
    integration::{eventsub::EventSub, kofi::KofiPayload, ntfy::NtfyTopic, twitch_api::TwitchApi},
    logging,
    services::{
        caches::CacheServiceExt,
        ipc::IpcServiceExt,
        messaging::{self, MessagingService},
        twitch::{TwitchService, TwitchServiceImpl},
    },
};
use tokio::task::JoinSet;

use tracing::Instrument;
use twitch_api::eventsub::{
    channel::*,
    stream::{StreamOfflineV1, StreamOnlineV1},
};

async fn run() -> Result<()> {
    let config = Config::load()?;

    let state = StateStore::connect(&config.valkey).await?;

    let twitch_api = TwitchApi::new(&config).await?;

    let mut eventsub = EventSub::new(twitch_api.clone());

    let id = twitch_api.caster_id();
    eventsub.listen_to(ChannelAdBreakBeginV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelFollowV2::new(id, id));
    eventsub.listen_to(ChannelBitsUseV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelCharityCampaignDonateV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelPointsCustomRewardRedemptionAddV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelSubscribeV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelSubscriptionGiftV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelSubscriptionMessageV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelCheerV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelRaidV1::to_broadcaster_user_id(id));
    eventsub.listen_to(ChannelHypeTrainBeginV2::broadcaster_user_id(id));
    eventsub.listen_to(ChannelHypeTrainEndV2::broadcaster_user_id(id));
    eventsub.listen_to(StreamOnlineV1::broadcaster_user_id(id));
    eventsub.listen_to(StreamOfflineV1::broadcaster_user_id(id));

    let (mut incoming, messaging) = messaging::connect_to_twitch(twitch_api.clone());

    let mut eventsub_rx = eventsub.subscribe();
    let eventsub_init = eventsub.wait_for_full_init();

    let BotServices {
        injector,
        display_server,
        event_list,
        noita: noita_handle,
        music_player,
    } = bot::services(&config, &state, &twitch_api).await?;

    let services = injector
        .with::<dyn MessagingService>(messaging.into())
        .with::<dyn TwitchService>(Arc::new(TwitchServiceImpl::new(twitch_api.clone())));

    let ctx = AppContext::new(services)
        .with_caster_id(twitch_api.caster_id().to_owned())
        .with_bot_id(twitch_api.bot_id().to_owned());

    let runner = bot::runner(&ctx);

    tokio::spawn(eventsub.run(ctx.clone()));
    tokio::spawn(display_server.start(&config.browser_source_bind, event_list.router()));
    if let Some(music_player) = &music_player {
        tokio::spawn(music_player.start(&config.music_player_bind));
    }

    let mut noita_events = noita_handle.subscribe();
    tokio::spawn(noita_handle.poll_state_updates(ctx.clone()));

    let kofi = NtfyTopic::new(&config.ntfy, "kofi")?;
    let mut kofi_events = kofi.subscribe();
    tokio::spawn(kofi.run());

    eventsub_init.await;
    // after eventsub init so we can receive redemptions
    twitch_api.unpause_rewards().await?;

    let ipc = ctx.ipc();
    ipc.publish("bot-restart", b"1").await?;

    let mut restart_signal = ipc.listen("bot-restart");
    let mut restart_received = false;
    let mut tasks = JoinSet::new();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = ctx.wait_for_quit() => break,
            _ = &mut restart_signal => {
                tracing::info!("received a restart signal from new instance");

                // I think this is technicaly racey?
                // but the chance is so slim we dont care ig
                let ipc = ctx.ipc();
                let interrupt_signal = async move { ipc.listen("interrupt").await };
                let handle = ctx.clone();
                tokio::spawn(async move {
                    if let Ok(Some(chatter_id)) = interrupt_signal.await {
                        // eh just panic the task on errors, we're shutting down soon anyway
                        let chatter_id = String::from_utf8(chatter_id).unwrap();
                        tracing::info!("received an interrupt from new instance");
                        handle.interrupt(
                            Some(&*chatter_id).filter(|id| *id != "<all>"),
                            InterruptKind::Interrupt, // ehh guess break never worked across bot restarts
                        );
                    }
                });
                restart_received = true;
                break;
            },
            Some(msg) = incoming.recv() => {
                let new = ctx
                    .caches()
                    .set(
                        "irc-seen",
                        Duration::from_secs(600),
                        &msg.id,
                        "1",
                    )
                    .await?;
                if !new {
                    tracing::info!("received duplicate irc message: {msg:?}");
                    continue;
                }

                let ctx = ctx.clone();
                let runner = runner.clone();
                let span = tracing::info_span!(
                    "message",
                    msg.id,
                    msg.sender.id = msg.sender.id,
                    otel.name = format!("{}: {}", msg.sender.login, msg.text)
                );
                cleanup(&mut tasks).spawn(
                    async move {
                        if let Err(error) = runner.process_message(ctx, msg).await {
                            tracing::error!(?error, "failed to handle message");
                        }
                    }
                    .instrument(span),
                );
            }
            Ok(event) = noita_events.recv() => {
                let ctx = ctx.clone();
                mainloop_task(&mut tasks, handle_noita_event(ctx, event))
            },
            Ok(event) = eventsub_rx.recv() => {
                if let Some(event) = ChannelEvent::from_eventsub(event) {
                    let ctx = ctx.clone();
                    mainloop_task(&mut tasks, handle_channel_event(ctx, event))
                }
            },
            Ok(event) = kofi_events.recv() => {
                let ctx = ctx.clone();
                mainloop_task(&mut tasks, async move {
                    handle_kofi_event(ctx, KofiPayload::parse_payload(&event)?).await
                })
            },
            else => break,
        };
    }

    // just pray nothing errored beforehand
    // where's my errdefer :(
    if !restart_received {
        twitch_api.pause_rewards().await?;
    }

    tasks.join_all().await;

    Ok(())
}

/// Cleanup completed tasks from given JoinSet.
///
/// See https://github.com/tokio-rs/tokio/discussions/5910
fn cleanup<T: 'static>(tasks: &mut JoinSet<T>) -> &mut JoinSet<T> {
    while let Some(Some(_)) = tokio::task::unconstrained(tasks.join_next()).now_or_never() {}
    tasks
}

fn mainloop_task(tasks: &mut JoinSet<()>, task: impl Future<Output = Result<()>> + Send + 'static) {
    cleanup(tasks).spawn(async move {
        if let Err(error) = task.await {
            tracing::error!(?error, "main loop error: {error:?}");
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    let _guard = logging::init()?;
    tracing::info!("starting up");
    run().await
}
