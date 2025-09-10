use std::{borrow::Cow, sync::Arc, time::Duration};

use anyhow::Result;
use futures::{FutureExt, StreamExt};
use opentelemetry::trace::Status;
use rustis::{
    client::Client as ValkeyClient,
    commands::{PubSubCommands, SetCondition, SetExpiration, StringCommands},
};
use tokio::{task::JoinSet, time::sleep};
use tpn_bot::{
    commands::{CommandTag, discover_declared_commands, runner::Runner},
    config::Config,
    context::app::{AppContext, InterruptKind},
    integration::{eventsub::EventSub, twitch_api::TwitchApi},
    logging,
    services::{
        Injector,
        charges::{Charges, ChargesService, ChargesServiceExt, ChargesServiceRedis},
        chat_log::{ChatLogService, ChatLogServiceElastic},
        gates::{GateService, GateServiceRedis},
        messaging::{self, MessagingService},
        music::{MusicService, MusicServiceImpl},
        noita::{ItemFound, NoitaEvent, NoitaHandle, NoitaHandleExt},
        sounds::{SoundService, SoundServiceExt, SoundServiceImpl},
        status_wall::{StatusService, StatusWall},
        storage::{StorageService, StorageServiceExt, StorageServiceRedis},
        storage_old::Storage,
        tts::{TtsService, TtsServiceExt, TtsServiceImpl},
        twitch::{TwitchService, TwitchServiceExt, TwitchServiceImpl},
    },
};

use tracing::{Instrument, Span, instrument};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use twitch_api::{
    eventsub::{Event, Message, Payload},
    types::SubscriptionTier,
};

async fn run(config: Config) -> Result<()> {
    let valkey = ValkeyClient::connect(&*config.valkey).await?;

    let twitch_api = TwitchApi::new(&config).await?;

    let eventsub = EventSub::new(twitch_api.clone());
    let (mut incoming, messaging) = messaging::connect_to_twitch(twitch_api.clone());

    let mut eventsub_rx = eventsub.subscribe();
    let eventsub_init = eventsub.wait_for_full_init();

    let status_wall = Arc::new(StatusWall::default());

    let services = Injector::new()
        .with::<dyn MessagingService>(messaging.into())
        .with::<dyn StorageService>(Arc::new(StorageServiceRedis::new(valkey.clone())))
        .with::<dyn ChargesService>(Arc::new(ChargesServiceRedis::new(valkey.clone())))
        .with::<dyn GateService>(Arc::new(GateServiceRedis::new(valkey.clone())))
        .with::<dyn TwitchService>(Arc::new(TwitchServiceImpl::new(twitch_api.clone())))
        .with::<dyn ChatLogService>(Arc::new(
            ChatLogServiceElastic::new(
                &config.elastic.url,
                &config.elastic.api_key,
                &config.elastic.index,
            )
            .await?,
        ))
        .with::<dyn SoundService>(Arc::new(SoundServiceImpl::default()))
        .with::<dyn MusicService>(Arc::new(MusicServiceImpl::new(
            "http://localhost:26538".into(),
        )))
        .with::<dyn TtsService>(Arc::new(TtsServiceImpl::default()))
        .with::<dyn StatusService>(status_wall.clone())
        // todo make it into a dyn service ofc
        .with(Arc::new(NoitaHandle::default()))
        // todo most of storage usage should be replaced with separate services
        .with(Arc::new(Storage::new(valkey.clone())))
        // config is *only* used in voting, todo remove/refactor it
        .with(Arc::new(config));

    let ctx = AppContext::new(services)
        .with_caster_id(twitch_api.caster_id().to_owned())
        .with_bot_id(twitch_api.bot_id().to_owned());

    let mut commands = discover_declared_commands();

    // todo make this less cringe
    commands.retain(|_, v| !v.is(CommandTag::NoitaControl));

    let runner = Runner::new(commands);

    tokio::spawn(eventsub.run(ctx.clone()));
    tokio::spawn(status_wall.start(&ctx.config().browser_source_bind));
    tokio::spawn(NoitaHandle::poll_state_updates(ctx.clone()));

    let mut noita_events = ctx.noita().subscribe();

    eventsub_init.await;
    // after eventsub init so we can receive redemptions
    twitch_api.unpause_rewards().await?;

    ctx.storage_old().publish("bot-restart", "1").await?;

    let mut restart_signal = ctx.storage_old().subscribe("bot-restart").await?;
    let mut tasks = JoinSet::new();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = restart_signal.next() => {
                tracing::info!("received a restart signal from new instance");

                // I think this is technicaly racey?
                // but the chance is so slim we dont care ig
                let mut interrupt_signal = ctx.storage_old().subscribe("interrupt").await?;
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
                    .storage_old()
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
    twitch_api.pause_rewards().await?;

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
            ctx.storage().del("best-inventory").await?;

            let won = ctx.noita().with(|n| {
                Ok(n.get_world_state()?
                    .map(|ws| anyhow::Ok(ws.flags.read_storage(n.proc())?.iter().any(|f| f == "ending_game_completed")))
                    .transpose()?
                    .unwrap_or_default())
            }).await?;

            if won {
                ctx.send("won GIGACHAD".into()).await?
            } else {
                ctx.send("died lmao".into()).await?
            }
            // ctx.next_run().await?
        },
        NoitaEvent::LowOxygen => {
            if ctx.gate("low-oxygen", Duration::from_secs(60)).await? {
                ctx.send("Kinda getting low on O₂ btw HelloHowAreYouIAmUnderTheWater".into()).await?
            }
        },
        NoitaEvent::Polymorphed => {
            if ctx.gate("polymorphed", Duration::from_secs(60)).await? {
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
                "Buy 1 charge" => {
                    ctx.charges()
                        .add(data.user_id.as_str(), Charges::ONE)
                        .await?;
                    true
                }
                "Buy 5 charges" => {
                    ctx.charges()
                        .add(data.user_id.as_str(), Charges::new(5, 0))
                        .await?;
                    true
                }
                "Buy 10 charges" => {
                    ctx.charges()
                        .add(data.user_id.as_str(), Charges::new(10, 0))
                        .await?;
                    true
                }
                "Buy 50 charges" => {
                    ctx.charges()
                        .add(data.user_id.as_str(), Charges::new(50, 0))
                        .await?;
                    true
                }
                "Buy 100 charges" => {
                    ctx.charges()
                        .add(data.user_id.as_str(), Charges::new(100, 0))
                        .await?;
                    true
                }
                _ => false,
            };
            if fulfilled {
                ctx.twitch()
                    .fulfill_redemption(data.reward.id.as_str(), data.id.as_str())
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
            ctx.tts().tts(&data.message.text, None).await?;
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
            if data.bits >= 25 {
                ctx.tts().tts(&data.message, None).await?;
            }
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

            let sound_service = ctx.sounds();
            let sound = tokio::spawn(async move { sound_service.play_builtin("RAID").await });

            ctx.send(format!(
                "VoHiYo Thanks for the raid @{}, and welcome raiders TwitchUnity",
                data.from_broadcaster_user_name
            ))
            .await?;

            ctx.twitch()
                .shout_out(data.from_broadcaster_user_id.as_str())
                .await?;

            sound.await??;
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
        Event::StreamOnlineV1(_) => ctx.send("→ stream start cutoff ←".into()).await?,
        Event::StreamOfflineV1(_) => {
            // todo maybe have some generic "persisted until end of stream" data store
            ctx.storage().del("last-pinger").await?;

            ctx.send("→ stream end cutoff ←".into()).await?
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
