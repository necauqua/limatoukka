use std::{borrow::Cow, time::Duration};

use anyhow::Result;
use opentelemetry::trace::Status;
use tpn_bot::{
    commands::runner,
    config::Config,
    context::app::AppContext,
    logging,
    services::{
        messaging,
        noita::{Inventory, ItemFound, NoitaHandle},
        status_wall::StatusWall,
        storage::Storage,
        twitch::Twitch,
        xdo::XDoClient,
    },
    storage,
};

use tracing::{Instrument, Span, instrument};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use twitch_api::{
    eventsub::{Event, Message, Payload},
    helix::chat::SendAShoutoutRequest,
    types::SubscriptionTier,
};

async fn run(config: Config) -> Result<()> {
    let storage = Storage::new(&config).await?;
    let xdo = XDoClient::new(config.display.clone());

    let (twitch, eventsub) = Twitch::new(&config.twitch).await?;
    let (mut incoming, messaging) = messaging::connect_to_twitch(twitch.clone());

    let mut eventsub_rx = eventsub.subscribe();
    tokio::spawn(eventsub.run());

    let ctx = AppContext::new(
        messaging,
        config,
        storage,
        xdo,
        NoitaHandle::default(),
        StatusWall::default(),
        twitch,
    );

    ctx.init();

    let mut found_items = ctx.noita().subscribe_to_found_items();

    loop {
        let res = tokio::select! {
            Some(msg) = incoming.recv() => {
                let span = tracing::info_span!(
                    "message",
                    msg.id,
                    msg.sender.id = msg.sender.id,
                    otel.name = format!("{}: {}", msg.sender.login, msg.text)
                );
                let ctx = ctx.clone();
                tokio::spawn(
                    async move {
                        if let Err(error) = runner::receive_message(ctx, msg).await {
                            tracing::error!(?error, "failed to handle message");
                            Span::current().set_status(Status::error("error"));
                        }
                    }
                    .instrument(span),
                );
                Ok(())
            }
            _ = ctx.noita().wait_for_player_death() => ctx.next_run().await,
            Ok((best_inv, item)) = found_items.recv() => found_item(&ctx, best_inv, item).await,
            Ok(event) = eventsub_rx.recv() => eventsub_event(&ctx, event).await,
            else => return Ok(()),
        };
        if let Err(error) = res {
            tracing::error!(?error, "main loop error");
        }
    }
}

async fn found_item(ctx: &AppContext, best_inv: Inventory, item: ItemFound) -> Result<()> {
    storage!(ctx, set, "best-inventory", { best_inv.bits() })?;

    tracing::info!("found item {item:?}");

    ctx.send(match item {
        ItemFound::TreeTablet => "The best TABLET in the game acquired!",
        ItemFound::OtherTablet => "TABLET acquired",
        ItemFound::EvilEye => "Got the EVILEYE",
        ItemFound::EarthStone => {
            "The final frontier before all the wacky shit, EARTHSTONE acquired! POGGIES"
        }
        ItemFound::TouchOfGold => {
            "TOUCHOFGOLD - Infinite money glitch? Midas at home? A boss-killer even ( Clueless )?"
        }
        ItemFound::Taikasauva => {
            "Got the SUMMONTAIKASAUVA , the whole world is in your hands now"
        }
    }.into()).await
}

#[instrument(skip_all)]
async fn eventsub_event(ctx: &AppContext, event: Event) -> Result<()> {
    match event {
        Event::ChannelPointsCustomRewardRedemptionAddV1(Payload {
            message: Message::Notification(data),
            ..
        }) => {
            tracing::info!(
                user.id = data.user_id.as_str(),
                user.login = data.user_login.as_str(),
                reward.name = data.reward.title.as_str(),
                reward.id = data.reward.id.as_str(),
                "reward redemption"
            );
            match data.reward.id.as_str() {
                // hello
                "7d046898-3594-45ec-ae58-d3dae0c68187" => {
                    ctx.send("hiii".into()).await?;
                }
                // bless
                "f2a54ce8-5c8a-4ed0-ab52-fe9fd11c41c9" => {
                    storage!(ctx, incr, "balance:blesses")?;
                }
                // curse
                "5716f47f-f8df-4fef-baf0-6b6a2b24ef76" => {
                    storage!(ctx, incr, "balance:curses")?;
                }
                _ => {}
            }
        }
        Event::ChannelAdBreakBeginV1(Payload {
            message: Message::Notification(data),
            ..
        }) => {
            tracing::info!(
                duration = data.duration_seconds,
                auto = data.is_automatic,
                "ad start"
            );
            ctx.send(
                "ADS TIME! Avoiding prerolls so people can check the stream without getting blasted. You can sub or get turbo xdd".into(),
            )
            .await?;
            ctx.schedule(
                Duration::from_secs(data.duration_seconds as _),
                |ctx| async move { ctx.send("ADS over".into()).await },
            );
        }
        Event::ChannelSubscribeV1(Payload {
            message: Message::Notification(data),
            ..
        }) => {
            tracing::info!(
                user.id = data.user_id.as_str(),
                user.login = data.user_login.as_str(),
                tier = ?data.tier,
                gifted = data.is_gift,
                "sub"
            );
            if data.is_gift {
                return Ok(());
            }
            match data.tier {
                SubscriptionTier::Tier1 => {
                    ctx.send(format!(
                        "Yooo, thanks for subscribing @{} <3",
                        data.user_name
                    ))
                    .await?
                }
                SubscriptionTier::Tier2 => {
                    ctx.send(format!(
                        "Yooo, thanks for subscribing @{} <3 <3",
                        data.user_name
                    ))
                    .await?
                }
                SubscriptionTier::Tier3 => {
                    ctx.send(format!(
                        "TIER 3 SIMP, HOOLY! Thanks for subscribing @{} <3 <3 <3",
                        data.user_name
                    ))
                    .await?
                }
                SubscriptionTier::Prime => {
                    ctx.send(format!(
                        "Yooo, free money! Thanks for the Prime @{} <3",
                        data.user_name
                    ))
                    .await?
                }
                _ => {}
            }
        }
        Event::ChannelSubscriptionGiftV1(Payload {
            message: Message::Notification(data),
            ..
        }) => {
            tracing::info!(
                user.id = data.user_id.as_deref().map(|u| u.as_str()),
                user.login = data.user_id.as_deref().map(|u| u.as_str()),
                tier = ?data.tier,
                amount = data.total,
                total = data.cumulative_total,
                "sub gift"
            );
            let name = data
                .user_name
                .map_or(Cow::Borrowed("anon"), |n| Cow::Owned(format!("@{n}")));
            ctx.send(match data.total {
                1 => format!("Thanks for the gifted sub, {name} <3"),
                _ => format!("Thanks for {} gifted subs {name} <3", data.total),
            })
            .await?;
        }
        Event::ChannelSubscriptionMessageV1(Payload {
            message: Message::Notification(data),
            ..
        }) => {
            tracing::info!(
                user.id = data.user_id.as_str(),
                user.login = data.user_login.as_str(),
                tier = ?data.tier,
                total = data.cumulative_months,
                streak = data.streak_months,
                text = data.message.text,
                "resub"
            );
        }
        Event::ChannelCheerV1(Payload {
            message: Message::Notification(data),
            ..
        }) => {
            tracing::info!(
                user.id = data.user_id.as_deref().map(|u| u.as_str()),
                user.login = data.user_id.as_deref().map(|u| u.as_str()),
                amount = data.bits,
                text = data.message,
                "cheer"
            );
        }
        Event::ChannelRaidV1(Payload {
            message: Message::Notification(data),
            ..
        }) => {
            tracing::info!(
                user.id = data.from_broadcaster_user_id.as_str(),
                user.login = data.from_broadcaster_user_login.as_str(),
                viewers = data.viewers,
                "raid"
            );
            ctx.send(format!(
                "VoHiYo Thanks for the raid @{}, and welcome raiders TwitchUnity",
                data.from_broadcaster_user_name
            ))
            .await?;
            ctx.twitch()
                .call(move |t| {
                    let user_id = data.from_broadcaster_user_id.clone();
                    async move {
                        let request = SendAShoutoutRequest::new(
                            t.target.id.clone(),
                            user_id,
                            t.token.user_id.clone(),
                        );
                        t.helix
                            .req_post(request, Default::default(), &t.token)
                            .await?;
                        Ok(())
                    }
                })
                .await?;
        }
        Event::ChannelHypeTrainBeginV1(Payload {
            message: Message::Notification(_),
            ..
        }) => {
            tracing::info!("hype train start");
            ctx.send("Scam train ICANT".into()).await?;
        }
        Event::ChannelHypeTrainEndV1(Payload {
            message: Message::Notification(data),
            ..
        }) => {
            tracing::info!("hype train end");
            let plural = match data.top_contributions.len() {
                1 => " was",
                _ => "s were",
            };
            let top = data
                .top_contributions
                .into_iter()
                .map(|c| c.user_name)
                .collect::<Vec<_>>()
                .join(", ");
            ctx.send(format!(
                "Scam train over, pfew.. Top contributor{plural} {top}"
            ))
            .await?;
        }
        event => tracing::info!(?event, "unhandled eventsub event"),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::load()?;
    logging::init(&config)?;

    tracing::info!("started");

    run(config).await
}
