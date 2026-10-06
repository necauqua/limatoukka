//! Handlers of the events that do not come from the chat: Noita, Twitch
//! EventSub and Ko-fi.

use std::{borrow::Cow, time::Duration};

use anyhow::Result;
use lazy_regex::regex_replace;
use tokio::time::sleep;
use tracing::instrument;
use twitch_api::{
    eventsub::{Event, Message, Payload},
    types::SubscriptionTier,
};

use crate::{
    context::app::AppContext,
    defs::betting::{BetCancelError, BetCloseError, close_bet, do_cancel_bet},
    integration::kofi::{KofiPayload, KofiType},
    services::{
        charges::{Charges, ChargesServiceExt},
        noita::{ItemFound, NoitaEvent, WinState},
        sounds::SoundServiceExt,
        storage::StorageServiceExt,
        tts::TtsServiceExt,
        twitch::TwitchServiceExt,
    },
};

pub async fn handle_noita_event(ctx: AppContext, event: NoitaEvent) -> Result<()> {
    use ItemFound as I;
    use NoitaEvent as E;
    match event {
        E::PortalEntered => {
            match close_bet(&ctx, true).await {
                Ok(()) => {
                    ctx.send("[!!!] Bet was auto-closed".into()).await?;
                },
                Err(BetCloseError::Internal(e)) => return Err(e),
                _ => {},
            }
        }
        E::PlayerDeath { killed_by, run_duration, had_ambro, win_state } => {
            match win_state {
                WinState::Loss => {
                    match do_cancel_bet(&ctx, true).await {
                        Ok(()) => {
                            ctx.send("[!!!] Bet was auto-cancelled cuz skill issue lmao ICANT".into()).await?
                        },
                        Err(BetCancelError::Internal(e)) => return Err(e),
                        _ => {
                            let killed_by = killed_by.trim_matches(|ch: char| ch.is_ascii_whitespace() || ch == '|');
                            let ambro = if had_ambro { " AmbroFU" } else { "" };
                            ctx.send(if killed_by.is_empty() {
                                format!("died lmao {ambro}")
                            } else {
                                format!("died lmao (death reason: {killed_by}){ambro}")
                            }).await?
                        },
                    }
                },
                WinState::Win { cheese: false } => ctx.send(format!("won GIGACHAD (took {run_duration})")).await?,
                WinState::Win { cheese: true } => ctx.send(format!("won StinkyCheese (took {run_duration})")).await?,
            }
        },
        E::WormSummoned => ctx.send("WORM ayo".into()).await?,
        E::LowOxygen => {
            if ctx.gate("low-oxygen", Duration::from_secs(60)).await? {
                ctx.send("Kinda getting low on O₂ btw HelloHowAreYouIAmUnderTheWater".into()).await?
            }
        },
        E::Polymorphed => {
            if ctx.gate("polymorphed", Duration::from_secs(60)).await? {
                ctx.send("Polymorphed ICANT".into()).await?
            }
        },
        E::OneHpClutch => {
            ctx.send("1 hp is all we needed EZ Clap".into()).await?
        },
        E::Blinded => {
            ctx.send("Now you see me, now you don't Blindge".into()).await?
        },
        E::ItemFound(item) => ctx.send(match item {
            I::TreeTablet => "The best TABLET in the game acquired!",
            I::OtherTablet => "TABLET acquired",
            I::EvilEye => "Got the EVILEYE",
            I::EarthStone => "The final frontier before all the wacky shit, EARTHSTONE acquired! POGGIES",
            I::TouchOfGold => "TOUCHOFGOLD - Infinite money glitch? Midas at home? A boss-killer even ( Clueless )?",
            I::Taikasauva => "Got the SUMMONTAIKASAUVA , the whole world is in your hands now",
            I::CircleOfVigour => "POGGIES ADDCOVS",
            I::TenSeven => "10-7 acquired",
            I::Brine => "BRINEbtw",
        }.into()).await?,
        E::PillarCompleted(pillar) => ctx.send(format!("A new pillar level was erected! '{pillar}' is complete! shadowWizardJAM")).await?,
        E::OtherPermanentFlag(flag) => ctx.send(format!("A permanent flag was set: {flag}")).await?,
    }
    Ok(())
}

/// A Twitch user in an event.
#[derive(Debug, Clone)]
pub struct EventUser {
    pub id: String,
    pub login: String,
    pub name: String,
}

/// The channel events that the bot handles. Made from the EventSub events
/// with [`ChannelEvent::from_eventsub`].
#[derive(Debug, Clone)]
pub enum ChannelEvent {
    Redemption {
        user: EventUser,
        reward_title: String,
        reward_id: String,
        redemption_id: String,
    },
    AdBreak {
        duration_seconds: i32,
        automatic: bool,
    },
    Sub {
        user: EventUser,
        tier: SubscriptionTier,
        gifted: bool,
    },
    SubGift {
        /// `None` for an anonymous gift
        user: Option<EventUser>,
        tier: SubscriptionTier,
        total: i64,
        cumulative_total: Option<i64>,
    },
    Resub {
        user: EventUser,
        tier: SubscriptionTier,
        cumulative_months: i64,
        streak_months: Option<i64>,
        text: String,
    },
    Cheer {
        /// `None` for an anonymous cheer
        user: Option<EventUser>,
        bits: i64,
        message: String,
    },
    Raid {
        from: EventUser,
        viewers: i64,
    },
    HypeTrainBegin,
    HypeTrainEnd {
        top_contributors: Vec<String>,
    },
    StreamOnline,
    StreamOffline,
}

impl ChannelEvent {
    /// Returns `None` (and logs the event) for the events that the bot does
    /// not handle.
    pub fn from_eventsub(event: Event) -> Option<Self> {
        Some(match event {
            Event::ChannelPointsCustomRewardRedemptionAddV1(Payload {
                message: Message::Notification(data),
                ..
            }) => ChannelEvent::Redemption {
                user: EventUser {
                    id: data.user_id.to_string(),
                    login: data.user_login.to_string(),
                    name: data.user_name.to_string(),
                },
                reward_title: data.reward.title,
                reward_id: data.reward.id.to_string(),
                redemption_id: data.id.to_string(),
            },
            Event::ChannelAdBreakBeginV1(Payload {
                message: Message::Notification(data),
                ..
            }) => ChannelEvent::AdBreak {
                duration_seconds: data.duration_seconds,
                automatic: data.is_automatic,
            },
            Event::ChannelSubscribeV1(Payload {
                message: Message::Notification(data),
                ..
            }) => ChannelEvent::Sub {
                user: EventUser {
                    id: data.user_id.to_string(),
                    login: data.user_login.to_string(),
                    name: data.user_name.to_string(),
                },
                tier: data.tier,
                gifted: data.is_gift,
            },
            Event::ChannelSubscriptionGiftV1(Payload {
                message: Message::Notification(data),
                ..
            }) => ChannelEvent::SubGift {
                user: optional_user(data.user_id, data.user_login, data.user_name),
                tier: data.tier,
                total: data.total,
                cumulative_total: data.cumulative_total,
            },
            Event::ChannelSubscriptionMessageV1(Payload {
                message: Message::Notification(data),
                ..
            }) => ChannelEvent::Resub {
                user: EventUser {
                    id: data.user_id.to_string(),
                    login: data.user_login.to_string(),
                    name: data.user_name.to_string(),
                },
                tier: data.tier,
                cumulative_months: data.cumulative_months,
                streak_months: data.streak_months,
                text: data.message.text,
            },
            Event::ChannelCheerV1(Payload {
                message: Message::Notification(data),
                ..
            }) => ChannelEvent::Cheer {
                user: optional_user(data.user_id, data.user_login, data.user_name),
                bits: data.bits,
                message: data.message,
            },
            Event::ChannelRaidV1(Payload {
                message: Message::Notification(data),
                ..
            }) => ChannelEvent::Raid {
                from: EventUser {
                    id: data.from_broadcaster_user_id.to_string(),
                    login: data.from_broadcaster_user_login.to_string(),
                    name: data.from_broadcaster_user_name.to_string(),
                },
                viewers: data.viewers,
            },
            Event::ChannelHypeTrainBeginV2(Payload {
                message: Message::Notification(_),
                ..
            }) => ChannelEvent::HypeTrainBegin,
            Event::ChannelHypeTrainEndV2(Payload {
                message: Message::Notification(data),
                ..
            }) => ChannelEvent::HypeTrainEnd {
                top_contributors: data
                    .top_contributions
                    .into_iter()
                    .map(|c| c.user_name.to_string())
                    .collect(),
            },
            Event::StreamOnlineV1(_) => ChannelEvent::StreamOnline,
            Event::StreamOfflineV1(_) => ChannelEvent::StreamOffline,
            event => {
                tracing::info!(?event, "unhandled eventsub event");
                return None;
            }
        })
    }
}

fn optional_user(
    id: Option<impl ToString>,
    login: Option<impl ToString>,
    name: Option<impl ToString>,
) -> Option<EventUser> {
    Some(EventUser {
        id: id?.to_string(),
        login: login?.to_string(),
        name: name?.to_string(),
    })
}

#[instrument(skip_all)]
pub async fn handle_channel_event(ctx: AppContext, event: ChannelEvent) -> Result<()> {
    match event {
        ChannelEvent::Redemption {
            user,
            reward_title,
            reward_id,
            redemption_id,
        } => {
            tracing::info!(
                user.id = user.id,
                user.login = user.login,
                reward.name = reward_title,
                reward.id = reward_id,
                "reward redemption"
            );
            let fulfilled = match reward_title.as_str() {
                "hello there" => {
                    ctx.send("hiii".into()).await?;
                    true
                }
                "Buy 1 charge" => {
                    ctx.charges().add(&user.id, Charges::ONE).await?;
                    true
                }
                "Buy 5 charges" => {
                    ctx.charges().add(&user.id, Charges::whole(5)).await?;
                    true
                }
                "Buy 10 charges" => {
                    ctx.charges().add(&user.id, Charges::whole(10)).await?;
                    true
                }
                "Buy 50 charges" => {
                    ctx.charges().add(&user.id, Charges::whole(50)).await?;
                    true
                }
                "Buy 100 charges" => {
                    ctx.send("HOLY OILER".into()).await?;

                    ctx.charges().add(&user.id, Charges::whole(100)).await?;
                    true
                }
                _ => false,
            };
            if fulfilled {
                ctx.twitch()
                    .fulfill_redemption(&reward_id, &redemption_id)
                    .await?;
            }
        }
        ChannelEvent::AdBreak {
            duration_seconds,
            automatic,
        } => {
            tracing::info!(duration = duration_seconds, auto = automatic, "ad start");
            if automatic {
                ctx.send(
                    "ADS TIME! Avoiding prerolls so people can check the stream without getting blasted. You can sub or get turbo xdd".into(),
                )
                .await?;
            } else {
                ctx.send(
                    "ADS TIME! Bro actually pressed the button to run ads, get ad'ed kek".into(),
                )
                .await?;
            }
            sleep(Duration::from_secs(duration_seconds as _)).await;
            ctx.send("ADS over".into()).await?;
        }
        ChannelEvent::Sub { user, tier, gifted } => {
            tracing::info!(
                user.id = user.id,
                user.login = user.login,
                ?tier,
                gifted,
                "sub"
            );
            if gifted {
                return Ok(());
            }
            match tier {
                SubscriptionTier::Tier1 => {
                    ctx.send(format!("Yooo, thanks for subscribing @{} <3", user.name))
                        .await?
                }
                SubscriptionTier::Tier2 => {
                    ctx.send(format!("Yooo, thanks for subscribing @{} <3 <3", user.name))
                        .await?
                }
                SubscriptionTier::Tier3 => {
                    ctx.send(format!(
                        "TIER 3 SIMP, HOOLY! Thanks for subscribing @{} <3 <3 <3",
                        user.name
                    ))
                    .await?
                }
                SubscriptionTier::Prime => {
                    ctx.send(format!(
                        "Yooo, free money! Thanks for the Prime @{} <3",
                        user.name
                    ))
                    .await?
                }
                _ => {}
            }
        }
        ChannelEvent::SubGift {
            user,
            tier,
            total,
            cumulative_total,
        } => {
            tracing::info!(
                user.id = user.as_ref().map(|u| u.id.as_str()),
                user.login = user.as_ref().map(|u| u.login.as_str()),
                ?tier,
                amount = total,
                total = cumulative_total,
                "sub gift"
            );
            let name = user.map_or(Cow::Borrowed("anon"), |u| {
                Cow::Owned(format!("@{}", u.name))
            });
            ctx.send(match total {
                1 => format!("Thanks for the gifted sub, {name} <3"),
                _ => format!("Thanks for {total} gifted subs {name} <3"),
            })
            .await?;
        }
        ChannelEvent::Resub {
            user,
            tier,
            cumulative_months,
            streak_months,
            text,
        } => {
            tracing::info!(
                user.id = user.id,
                user.login = user.login,
                ?tier,
                total = cumulative_months,
                streak = streak_months,
                text,
                "resub"
            );
            ctx.tts().tts(&text, None).await?;
        }
        ChannelEvent::Cheer {
            user,
            bits,
            message,
        } => {
            tracing::info!(
                user.id = user.as_ref().map(|u| u.id.as_str()),
                user.login = user.as_ref().map(|u| u.login.as_str()),
                amount = bits,
                text = message,
                "cheer"
            );
            if bits >= 25 {
                let text = regex_replace!(r"\bcheer\d+\b"i, &message, "");
                ctx.tts().tts(&text, None).await?;
            }
        }
        ChannelEvent::Raid { from, viewers } => {
            tracing::info!(user.id = from.id, user.login = from.login, viewers, "raid");

            let raid_sound = match from.id.as_str() {
                "39063397" => "lasiace-raid",
                "669474121" => "nutty-raid",
                _ => "RAID",
            };
            let sound_service = ctx.sounds();
            let sound = tokio::spawn(async move { sound_service.play_builtin(raid_sound).await });

            ctx.send(format!(
                "VoHiYo Thanks for the raid @{}, and welcome raiders TwitchUnity",
                from.name
            ))
            .await?;

            ctx.twitch().shout_out(&from.id).await?;

            sound.await??;
        }
        ChannelEvent::HypeTrainBegin => {
            tracing::info!("hype train start");
            ctx.send("Scam train ICANT".into()).await?;
        }
        ChannelEvent::HypeTrainEnd { top_contributors } => {
            tracing::info!("hype train end");
            let plural = match top_contributors.len() {
                1 => " was",
                _ => "s were",
            };
            let top = top_contributors.join(", ");
            ctx.send(format!(
                "Scam train over, pfew.. Top contributor{plural} {top}"
            ))
            .await?;
        }
        ChannelEvent::StreamOnline => {
            tracing::info!("stream online");
            ctx.storage().set("stream-online", "1").await?;
            ctx.send("→ stream start cutoff ←".into()).await?
        }
        ChannelEvent::StreamOffline => {
            tracing::info!("stream offline");

            // todo maybe have some generic "persisted until end of stream" data store
            ctx.storage().del("last-pinger").await?;
            ctx.storage().del("stream-online").await?;
            ctx.storage().del("noita:state").await?;

            ctx.send("→ stream end cutoff ←".into()).await?
        }
    }
    Ok(())
}

#[instrument(skip_all)]
pub async fn handle_kofi_event(ctx: AppContext, event: KofiPayload) -> Result<()> {
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
