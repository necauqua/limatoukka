use std::{borrow::Cow, time::Duration};

use anyhow::Result;
use futures::{FutureExt, StreamExt};
use opentelemetry::trace::Status;
use rustis::commands::{
    GenericCommands as _, PubSubCommands, SetCondition, SetExpiration, StringCommands,
};
use tokio::{task::JoinSet, time::sleep};
use tpn_bot::{
    commands::runner,
    config::Config,
    context::app::{AppContext, InterruptKind},
    logging,
    services::{
        Services,
        chat_log::ChatLog,
        messaging,
        noita::{ItemFound, NoitaEvent, NoitaHandle},
        status_wall::StatusWall,
        storage::{BalanceMessage, Storage},
        twitch::{EventSub, Twitch},
        xdo::XDoClient,
    },
};

use tracing::{Instrument, Span, instrument};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use twitch_api::{
    eventsub::{Event, Message, Payload},
    helix::{
        chat::SendAShoutoutRequest,
        points::{
            CustomRewardRedemptionStatus, UpdateRedemptionStatusBody, UpdateRedemptionStatusRequest,
        },
    },
    types::SubscriptionTier,
};

async fn run(config: Config) -> Result<()> {
    let chat_log = ChatLog::new(&config).await?;
    let storage = Storage::new(&config).await?;
    let xdo = XDoClient::new(config.display.clone());

    let twitch = Twitch::new(&config).await?;

    let eventsub = EventSub::new(twitch.clone());
    let (mut incoming, messaging) = messaging::connect_to_twitch(twitch.clone());

    let mut eventsub_rx = eventsub.subscribe();
    let eventsub_init = eventsub.wait_for_full_init();

    let ctx = AppContext::new(
        config,
        Services::new(
            messaging,
            storage,
            chat_log,
            xdo,
            NoitaHandle::default(),
            StatusWall::default(),
            twitch,
        ),
    );

    tokio::spawn(eventsub.run(ctx.clone()));
    tokio::spawn(ctx.status_wall().start(&ctx.config().browser_source_bind));
    tokio::spawn(NoitaHandle::poll_state_updates(ctx.clone()));

    let mut noita_events = ctx.noita().subscribe();

    eventsub_init.await;
    // after eventsub init so we can receive redemptions
    ctx.twitch().unpause_rewards().await?;

    ctx.storage().publish("bot-restart", "1").await?;

    let mut restart_signal = ctx.storage().subscribe("bot-restart").await?;
    let mut tasks = JoinSet::new();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = restart_signal.next() => {
                tracing::info!("received a restart signal from new instance");

                // I think this is technicaly racey?
                // but the chance is so slim we dont care ig
                let mut interrupt_signal = ctx.storage().subscribe("interrupt").await?;
                let handle = ctx.clone();
                tokio::spawn(async move {
                    if let Some(chatter_id) = interrupt_signal.next().await {
                        // eh just panic the task on errors, we're shutting down soon anyway
                        let chatter_id = String::from_utf8(chatter_id.unwrap().payload).unwrap();
                        tracing::info!("received an interrupt from new instance");
                        handle.interrupt(
                            Some(&*chatter_id).filter(|id| *id != "<all>"),
                            InterruptKind::Interrupt, // ehh guess break never worked across bot restarts
                        );
                    }
                });
                break;
            },
            Some(msg) = incoming.recv() => {
                let new = ctx
                    .storage()
                    .set_with_options(
                        format!("seen:irc:{}", msg.id),
                        "1",
                        SetCondition::NX,
                        SetExpiration::Ex(600),
                        false,
                    )
                    .await?;
                if !new {
                    tracing::info!("received duplicate irc message: {msg:?}");
                    continue;
                }

                let ctx = ctx.clone();
                let span = tracing::info_span!(
                    "message",
                    msg.id,
                    msg.sender.id = msg.sender.id,
                    otel.name = format!("{}: {}", msg.sender.login, msg.text)
                );
                cleanup(&mut tasks).spawn(
                    async move {
                        if let Err(error) = runner::receive_message(ctx, msg).await {
                            tracing::error!(?error, "failed to handle message");
                            Span::current().set_status(Status::error("error"));
                        }
                    }
                    .instrument(span),
                );
            }
            Ok(event) = noita_events.recv() => {
                let ctx = ctx.clone();
                mainloop_task(&mut tasks, async move { noita_event(ctx, event).await })
            },
            Ok(event) = eventsub_rx.recv() => {
                let ctx = ctx.clone();
                mainloop_task(&mut tasks, async move { eventsub_event(ctx, event).await })
            },
            else => break,
        };
    }

    // just pray nothing errored beforehand
    // where's my errdefer :(
    ctx.twitch().pause_rewards().await?;

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

async fn noita_event(ctx: AppContext, event: NoitaEvent) -> Result<()> {
    match event {
        NoitaEvent::PlayerDeath => {
            ctx.storage().modify_balance(BalanceMessage::Reset).await?;
            ctx.storage().del("best-inventory").await?;

            ctx.send("died lmao".into()).await?
            // ctx.next_run().await?
        },
        NoitaEvent::LowOxygen => ctx.send("Kinda getting low on O₂ btw HelloHowAreYouIAmUnderTheWater".into()).await?,
        NoitaEvent::Polymorphed => {
            if ctx.gate("polymorphed", Duration::from_secs(300)).await? {
                ctx.send("Polymorphed ICANT".into()).await?
            }
        },
        NoitaEvent::ItemFound(item) => ctx.send(match item {
            ItemFound::TreeTablet => "The best TABLET in the game acquired!",
            ItemFound::OtherTablet => "TABLET acquired",
            ItemFound::EvilEye => "Got the EVILEYE",
            ItemFound::EarthStone => "The final frontier before all the wacky shit, EARTHSTONE acquired! POGGIES",
            ItemFound::TouchOfGold => "TOUCHOFGOLD - Infinite money glitch? Midas at home? A boss-killer even ( Clueless )?",
            ItemFound::Taikasauva => "Got the SUMMONTAIKASAUVA , the whole world is in your hands now",
        }.into()).await?,
        NoitaEvent::PillarCompleted(pillar) => ctx.send(format!("A new pillar level was erected! '{pillar}' is complete! shadowWizardJAM")).await?,
        NoitaEvent::NewSpellCast(flag, name) => ctx.send(format!("A new spell was cast: {name} ({flag})")).await?,
        NoitaEvent::OtherPermanentFlag(flag) => ctx.send(format!("A permanent flag was set: {flag}")).await?,
        _ => {}
    }
    Ok(())
}

#[instrument(skip_all)]
async fn eventsub_event(ctx: AppContext, event: Event) -> Result<()> {
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
            let fulfilled = match data.reward.title.as_str() {
                "hello there" => {
                    ctx.send("hiii".into()).await?;
                    true
                }
                "BLESS THE RUN" => {
                    ctx.storage().modify_balance(BalanceMessage::Bless).await?;
                    true
                }
                "CURSE THE RUN" => {
                    ctx.storage().modify_balance(BalanceMessage::Curse).await?;
                    true
                }
                _ => false,
            };
            if fulfilled {
                let id = &data.id;
                let reward_id = &data.reward.id;
                ctx.twitch()
                    .caster_call(async |t| {
                        let request =
                            UpdateRedemptionStatusRequest::new(t.caster_id, reward_id, id);
                        let body = UpdateRedemptionStatusBody::status(
                            CustomRewardRedemptionStatus::Fulfilled,
                        );
                        t.helix.req_patch(request, body, &t.token).await?;
                        Ok(())
                    })
                    .await?;
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
            sleep(Duration::from_secs(data.duration_seconds as _)).await;
            ctx.send("ADS over".into()).await?;
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
                user.login = data.user_login.as_deref().map(|u| u.as_str()),
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
                user.login = data.user_login.as_deref().map(|u| u.as_str()),
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
                .call(async |t| {
                    let request = SendAShoutoutRequest::new(
                        t.caster_id,
                        &data.from_broadcaster_user_id,
                        t.token.user_id.clone(),
                    );
                    t.helix
                        .req_post(request, Default::default(), &t.token)
                        .await?;
                    Ok(())
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
