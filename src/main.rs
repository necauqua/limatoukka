use std::{borrow::Cow, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use futures::FutureExt;
use lazy_regex::regex_replace;
use limatoukka::{
    commands::{CommandTag, discover_declared_commands, runner::Runner},
    config::Config,
    context::app::{AppContext, InterruptKind},
    defs::betting::{BetCancelError, BetCloseError, close_bet, do_cancel_bet},
    integration::{
        eventsub::EventSub,
        kofi::{KofiPayload, KofiType},
        ntfy::NtfyTopic,
        twitch_api::TwitchApi,
        yt_music_api::YouTubeMusic,
    },
    logging,
    services::{
        Injector,
        banishes::{BanishService, BanishServiceValkey},
        caches::{CacheService, CacheServiceExt, CacheServiceValkey},
        charges::{Charges, ChargesService, ChargesServiceExt, ChargesServiceValkey},
        chat_log::{ChatLogService, ChatLogServiceElastic},
        display::{DisplayServer, DisplayService, DisplayServiceAux},
        gates::{GateService, GateServiceValkey},
        ipc::{IpcService, IpcServiceExt, IpcServiceValkey},
        messaging::{self, MessagingService},
        music::{MusicService, YouTubeMusicPlayer},
        noita::{ItemFound, NoitaEvent, NoitaHandle, NoitaService, NoitaServiceExt, WinState},
        sounds::{SoundService, SoundServiceExt, SoundServiceImpl},
        stats::{StatsService, StatsServiceElastic},
        status_wall::{StatusService, StatusWall},
        storage::{StorageService, StorageServiceExt, StorageServiceValkey},
        tts::{TtsService, TtsServiceExt, TtsServiceImpl},
        twitch::{TwitchService, TwitchServiceExt, TwitchServiceImpl},
        variables::{VariableStorage, VariableStorageValkey},
    },
};
use rustis::client::Client as ValkeyClient;
use tokio::{task::JoinSet, time::sleep};

use tracing::{Instrument, instrument};
use twitch_api::{
    eventsub::{
        Event, Message, Payload,
        channel::*,
        stream::{StreamOfflineV1, StreamOnlineV1},
    },
    types::SubscriptionTier,
};

async fn run() -> Result<()> {
    let config = Config::load()?;

    let valkey = ValkeyClient::connect(&*config.valkey)
        .await
        .context("connecting to db")?;

    let twitch_api = TwitchApi::new(&config).await?;

    let mut eventsub = EventSub::new(twitch_api.clone());

    let id = twitch_api.caster_id();
    eventsub.listen_to(ChannelAdBreakBeginV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelPointsCustomRewardRedemptionAddV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelSubscribeV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelSubscriptionGiftV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelSubscriptionMessageV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelCheerV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelRaidV1::to_broadcaster_user_id(id));
    eventsub.listen_to(ChannelHypeTrainBeginV1::broadcaster_user_id(id));
    eventsub.listen_to(ChannelHypeTrainEndV1::broadcaster_user_id(id));
    eventsub.listen_to(StreamOnlineV1::broadcaster_user_id(id));
    eventsub.listen_to(StreamOfflineV1::broadcaster_user_id(id));

    let (mut incoming, messaging) = messaging::connect_to_twitch(twitch_api.clone());

    let mut eventsub_rx = eventsub.subscribe();
    let eventsub_init = eventsub.wait_for_full_init();

    let display_server = Arc::new(DisplayServer::default());
    let noita_handle = Arc::new(NoitaHandle::default());

    let music_player = Arc::new(
        YouTubeMusicPlayer::new(
            YouTubeMusic::new(config.youtube.api_key, config.youtube.country_code),
            &config.youtube.playlist,
            valkey.clone(),
            display_server.clone(),
        )
        .await?,
    );

    let services = Injector::new()
        .with::<dyn MessagingService>(messaging.into())
        .with::<dyn StorageService>(Arc::new(StorageServiceValkey::new(valkey.clone())))
        .with::<dyn VariableStorage>(Arc::new(VariableStorageValkey::new(valkey.clone())))
        .with::<dyn CacheService>(Arc::new(CacheServiceValkey::new(valkey.clone())))
        .with::<dyn IpcService>(Arc::new(IpcServiceValkey::new(valkey.clone())))
        .with::<dyn BanishService>(Arc::new(BanishServiceValkey::new(valkey.clone())))
        .with::<dyn ChargesService>(Arc::new(ChargesServiceValkey::new(valkey.clone())))
        .with::<dyn GateService>(Arc::new(GateServiceValkey::new(valkey.clone())))
        .with::<dyn TwitchService>(Arc::new(TwitchServiceImpl::new(twitch_api.clone())))
        .with::<dyn StatsService>(Arc::new(StatsServiceElastic::new(
            &config.stats.url,
            &config.stats.api_key,
            &config.stats.index,
        )?))
        .with::<dyn ChatLogService>(Arc::new(ChatLogServiceElastic::new(
            &config.elastic.url,
            &config.elastic.api_key,
            &config.elastic.index,
        )?))
        .with::<dyn SoundService>(Arc::new(SoundServiceImpl::default()))
        .with::<dyn TtsService>(Arc::new(TtsServiceImpl::default()))
        .with::<dyn StatusService>(Arc::new(StatusWall::new(display_server.wrap("status"))))
        .with::<dyn DisplayService>(display_server.clone())
        .with::<dyn NoitaService>(noita_handle.clone())
        .with::<dyn MusicService>(music_player.clone());

    let ctx = AppContext::new(services)
        .with_caster_id(twitch_api.caster_id().to_owned())
        .with_bot_id(twitch_api.bot_id().to_owned());

    let mut commands = discover_declared_commands();

    // todo make this less cringe
    commands.retain(|_, v| !v.is(CommandTag::NoitaControl));

    let runner = Runner::new(discover_declared_commands(), &ctx);

    tokio::spawn(eventsub.run(ctx.clone()));
    tokio::spawn(display_server.start(&config.browser_source_bind));
    tokio::spawn(music_player.start(&config.music_player_bind));

    let mut noita_events = noita_handle.subscribe();
    tokio::spawn(noita_handle.poll_state_updates(ctx.clone()));

    let kofi = NtfyTopic::new(&config.ntfy, "kofi");
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
                mainloop_task(&mut tasks, async move { noita_event(ctx, event).await })
            },
            Ok(event) = eventsub_rx.recv() => {
                let ctx = ctx.clone();
                mainloop_task(&mut tasks, async move { eventsub_event(ctx, event).await })
            },
            Ok(event) = kofi_events.recv() => {
                let ctx = ctx.clone();
                mainloop_task(&mut tasks, async move { kofi_event(ctx, event).await })
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

async fn noita_event(ctx: AppContext, event: NoitaEvent) -> Result<()> {
    match event {
        NoitaEvent::PortalEntered => {
            match close_bet(&ctx, true).await {
                Ok(()) => {
                    ctx.send("[!!!] Bet was auto-closed".into()).await?;
                },
                Err(BetCloseError::Internal(e)) => return Err(e),
                _ => {},
            }
        }
        NoitaEvent::PlayerDeath { killed_by } => {
            match ctx.noita().get_win_state().await? {
                WinState::Loss => {
                    match do_cancel_bet(&ctx, true).await {
                        Ok(()) => {
                            ctx.send("[!!!] Bet was auto-cancelled cuz skill issue lmao ICANT".into()).await?
                        },
                        Err(BetCancelError::Internal(e)) => return Err(e),
                        _ => {
                            let killed_by = killed_by.trim_matches(|ch: char| ch.is_ascii_whitespace() || ch == '|');
                            ctx.send(if killed_by.is_empty() {
                                "died lmao".into()
                            } else {
                                format!("died lmao (death reason: {killed_by})")
                            }).await?
                        },
                    }
                },
                WinState::Win { cheese: false } => ctx.send("won GIGACHAD".into()).await?,
                WinState::Win { cheese: true } => ctx.send("won StinkyCheese".into()).await?,
            }
        },
        NoitaEvent::WormSummoned => ctx.send("WORM ayo".into()).await?,
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
        NoitaEvent::OneHpClutch => {
            ctx.send("1 hp is all we needed EZ Clap".into()).await?
        },
        NoitaEvent::ItemFound(item) => ctx.send(match item {
            ItemFound::TreeTablet => "The best TABLET in the game acquired!",
            ItemFound::OtherTablet => "TABLET acquired",
            ItemFound::EvilEye => "Got the EVILEYE",
            ItemFound::EarthStone => "The final frontier before all the wacky shit, EARTHSTONE acquired! POGGIES",
            ItemFound::TouchOfGold => "TOUCHOFGOLD - Infinite money glitch? Midas at home? A boss-killer even ( Clueless )?",
            ItemFound::Taikasauva => "Got the SUMMONTAIKASAUVA , the whole world is in your hands now",
            ItemFound::CircleOfVigour => "POGGIES ADDCOVS",
            ItemFound::TenSeven => "10-7 acquired",
        }.into()).await?,
        NoitaEvent::PillarCompleted(pillar) => ctx.send(format!("A new pillar level was erected! '{pillar}' is complete! shadowWizardJAM")).await?,
        NoitaEvent::OtherPermanentFlag(flag) => ctx.send(format!("A permanent flag was set: {flag}")).await?,
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
                        .add(data.user_id.as_str(), Charges::whole(5))
                        .await?;
                    true
                }
                "Buy 10 charges" => {
                    ctx.charges()
                        .add(data.user_id.as_str(), Charges::whole(10))
                        .await?;
                    true
                }
                "Buy 50 charges" => {
                    ctx.charges()
                        .add(data.user_id.as_str(), Charges::whole(50))
                        .await?;
                    true
                }
                "Buy 100 charges" => {
                    ctx.send("HOLY OILER".into()).await?;

                    ctx.charges()
                        .add(data.user_id.as_str(), Charges::whole(100))
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
                let text = regex_replace!(r"\bcheer\d+\b"i, &data.message, "");
                ctx.tts().tts(&text, None).await?;
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

            let raid_sound = match data.from_broadcaster_user_id.as_str() {
                "39063397" => "lasiace-raid",
                "669474121" => "nutty-raid",
                _ => "RAID",
            };
            let sound_service = ctx.sounds();
            let sound = tokio::spawn(async move { sound_service.play_builtin(raid_sound).await });

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
        Event::StreamOnlineV1(_) => {
            tracing::info!("stream online");
            ctx.storage().set("stream-online", "1").await?;
            ctx.send("→ stream start cutoff ←".into()).await?
        }
        Event::StreamOfflineV1(_) => {
            tracing::info!("stream offline");

            // todo maybe have some generic "persisted until end of stream" data store
            ctx.storage().del("last-pinger").await?;
            ctx.storage().del("stream-online").await?;
            ctx.storage().del("noita:state").await?;

            ctx.send("→ stream end cutoff ←".into()).await?
        }
        event => tracing::info!(?event, "unhandled eventsub event"),
    }
    Ok(())
}

#[instrument(skip_all)]
async fn kofi_event(ctx: AppContext, event: Arc<str>) -> Result<()> {
    let event = KofiPayload::parse_payload(&event)?;

    tracing::debug!(?event, "kofi event");

    // obviously dont shout out non-public ones
    if !event.is_public {
        return Ok(());
    }

    if event.is_first_subscription_payment {
        tracing::info!(
            amount = event.amount,
            currency = event.currency,
            name = event.from_name,
            "kofi sub"
        );
        ctx.send(format!(
            "[+{} {}] Yooo, thanks for subscribing on Ko-fi, {} <3",
            event.amount, event.currency, event.from_name
        ))
        .await?;
    } else if event.is_subscription_payment {
        tracing::info!(
            amount = event.amount,
            currency = event.currency,
            name = event.from_name,
            "kofi resub"
        );
        ctx.send(format!(
            "[+{} {}] Thanks for the continued Ko-fi support, {} <3 <3 <3",
            event.amount, event.currency, event.from_name
        ))
        .await?;
    } else if matches!(event.event_type, KofiType::Donation) {
        tracing::info!(
            amount = event.amount,
            currency = event.currency,
            name = event.from_name,
            "kofi dono"
        );
        ctx.send(format!(
            "[+{} {}] Tysm for a Ko-fi dono, {} <3",
            event.amount, event.currency, event.from_name
        ))
        .await?;
    }

    if let Some(msg) = &event.message {
        // todo admin interrupt for this and bits lmao
        ctx.tts().tts(msg, None).await?;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let _guard = logging::init()?;
    tracing::info!("starting up");
    run().await
}
