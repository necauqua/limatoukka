//! The terminal commands: the lines that start with `/`.

use limatoukka::{
    events::{ChannelEvent, EventUser},
    integration::kofi::{KofiPayload, KofiType},
    services::{
        event_list::{PowerUp, format_money},
        messaging::PermissionLevel,
        noita::{ItemFound, NoitaEvent, WinState},
    },
};
use twitch_api::types::SubscriptionTier;

pub const HELP: &str = "\
Lines without a leading / are chat messages of the current user.
Terminal commands (events come from the current user):
  /as <login> [level]           chat as another Twitch user; level: viewer, sub,
                                verified, vip, staff, admin, mod, caster
  /redeem <reward title>        channel point redemption, e.g. /redeem Buy 5 charges
  /follow                       follow
  /sub [1|2|3|prime]            new sub
  /gift [count] [1|2|3] [anon]  gifted subs
  /resub <months> [text]        resub with a message
  /cheer <bits> [text]          cheer
  /powerup [bits] [effect|celebration|gigantify|<custom title>]
                                bits used on a Power-up (default 100, gigantify)
  /charity <dollars> [charity]  charity campaign donation
  /raid [viewers]               raid from the channel of the current user
  /ad [seconds] [manual]        ad break (default 30 s, automatic)
  /hype start|end               hype train
  /online, /offline             stream start/end
  /noita death [win|cheese] [ambro] [reason]
  /noita item <tree-tablet|tablet|evil-eye|earth-stone|touch-of-gold|
              taikasauva|cov|10-7|brine>
  /noita pillar <name>, /noita flag <name>
  /noita polymorph|oxygen|worm|portal|clutch|blind
  /kofi dono|sub|resub <amount> [message]   (currency USD)
  /help, /quit (or Ctrl-D)";

pub enum Action {
    Help,
    Quit,
    As {
        login: String,
        level: Option<PermissionLevel>,
    },
    Channel(ChannelEvent),
    Noita(NoitaEvent),
    Kofi(KofiPayload),
}

pub fn parse_level(level: &str) -> Option<PermissionLevel> {
    Some(match level.to_lowercase().as_str() {
        "viewer" => PermissionLevel::Viewer,
        "sub" | "subscriber" => PermissionLevel::Subscriber,
        "verified" | "partner" => PermissionLevel::Verified,
        "vip" => PermissionLevel::Vip,
        "staff" => PermissionLevel::TwitchStaff,
        "admin" => PermissionLevel::TwitchAdmin,
        "mod" | "moderator" => PermissionLevel::Moderator,
        "caster" | "broadcaster" => PermissionLevel::Caster,
        _ => return None,
    })
}

fn parse_tier(tier: &str) -> Option<SubscriptionTier> {
    Some(match tier.to_lowercase().as_str() {
        "1" => SubscriptionTier::Tier1,
        "2" => SubscriptionTier::Tier2,
        "3" => SubscriptionTier::Tier3,
        "prime" => SubscriptionTier::Prime,
        _ => return None,
    })
}

fn number<T: std::str::FromStr>(arg: Option<&str>, default: T, what: &str) -> Result<T, String> {
    match arg {
        None => Ok(default),
        Some(arg) => arg.parse().map_err(|_| format!("{what} must be a number")),
    }
}

/// Parse a line without the leading `/`.
pub fn parse(line: &str, user: &EventUser) -> Result<Action, String> {
    let mut words = line.split_whitespace();
    let command = words.next().unwrap_or_default();
    let args = words.collect::<Vec<_>>();
    let rest = |from: usize| args.get(from..).unwrap_or_default().join(" ");
    let fake_id = || uuid::Uuid::now_v7().to_string();

    Ok(match command {
        "help" => Action::Help,
        "quit" | "exit" => Action::Quit,
        "as" => {
            let login = args.first().ok_or("usage: /as <login> [level]")?;
            let level = match args.get(1) {
                Some(level) => Some(parse_level(level).ok_or(format!("unknown level {level}"))?),
                None => None,
            };
            Action::As {
                login: login.trim_start_matches('@').to_lowercase(),
                level,
            }
        }
        "redeem" => {
            if args.is_empty() {
                return Err("usage: /redeem <reward title>".into());
            }
            Action::Channel(ChannelEvent::Redemption {
                user: user.clone(),
                reward_title: rest(0),
                reward_id: fake_id(),
                redemption_id: fake_id(),
            })
        }
        "follow" => Action::Channel(ChannelEvent::Follow { user: user.clone() }),
        "powerup" => {
            let bits = number(args.first().copied(), 100, "bits")?;
            Action::Channel(ChannelEvent::PowerUp {
                user: user.clone(),
                bits,
                power_up: match args.get(1).copied() {
                    None | Some("gigantify") => PowerUp::GigantifyEmote,
                    Some("effect") => PowerUp::MessageEffect,
                    Some("celebration") => PowerUp::Celebration,
                    Some(_) => PowerUp::Custom { title: rest(1) },
                },
            })
        }
        "charity" => {
            let amount = args.first().ok_or("usage: /charity <dollars> [charity]")?;
            let charity = rest(1);
            Action::Channel(ChannelEvent::CharityDonation {
                user: user.clone(),
                amount: format_money(amount, "USD"),
                charity: if charity.is_empty() {
                    "Doctors Without Borders".into()
                } else {
                    charity
                },
            })
        }
        "sub" => Action::Channel(ChannelEvent::Sub {
            user: user.clone(),
            tier: match args.first() {
                Some(tier) => parse_tier(tier).ok_or("tier must be 1, 2, 3 or prime")?,
                None => SubscriptionTier::Tier1,
            },
            gifted: false,
        }),
        "gift" => {
            let anon = args.contains(&"anon");
            let args = args.iter().filter(|a| **a != "anon").collect::<Vec<_>>();
            let total = number(args.first().map(|a| **a), 1, "count")?;
            Action::Channel(ChannelEvent::SubGift {
                user: (!anon).then(|| user.clone()),
                tier: match args.get(1) {
                    Some(tier) => parse_tier(tier).ok_or("tier must be 1, 2 or 3")?,
                    None => SubscriptionTier::Tier1,
                },
                total,
                cumulative_total: Some(total),
            })
        }
        "resub" => {
            let months = number(args.first().copied(), 2, "months")?;
            Action::Channel(ChannelEvent::Resub {
                user: user.clone(),
                tier: SubscriptionTier::Tier1,
                cumulative_months: months,
                streak_months: Some(months),
                text: rest(1),
            })
        }
        "cheer" => {
            let bits = args.first().ok_or("usage: /cheer <bits> [text]")?;
            let bits = number(Some(bits), 0, "bits")?;
            Action::Channel(ChannelEvent::Cheer {
                user: Some(user.clone()),
                bits,
                message: format!("Cheer{bits} {}", rest(1)).trim_end().to_owned(),
            })
        }
        "raid" => Action::Channel(ChannelEvent::Raid {
            from: user.clone(),
            viewers: number(args.first().copied(), 10, "viewers")?,
        }),
        "ad" => {
            let manual = args.contains(&"manual");
            let seconds = args.iter().find(|a| **a != "manual").copied();
            Action::Channel(ChannelEvent::AdBreak {
                duration_seconds: number(seconds, 30, "seconds")?,
                automatic: !manual,
            })
        }
        "hype" => Action::Channel(match args.first().copied() {
            Some("start") => ChannelEvent::HypeTrainBegin,
            Some("end") => ChannelEvent::HypeTrainEnd {
                top_contributors: vec![user.name.clone()],
            },
            _ => return Err("usage: /hype start|end".into()),
        }),
        "online" => Action::Channel(ChannelEvent::StreamOnline),
        "offline" => Action::Channel(ChannelEvent::StreamOffline),
        "noita" => Action::Noita(parse_noita(&args)?),
        "kofi" => {
            let (Some(kind), Some(amount)) = (args.first(), args.get(1)) else {
                return Err("usage: /kofi dono|sub|resub <amount> [message]".into());
            };
            let (event_type, sub, first_sub) = match *kind {
                "dono" => (KofiType::Donation, false, false),
                "sub" => (KofiType::Subscription, true, true),
                "resub" => (KofiType::Subscription, true, false),
                _ => return Err("the kind must be dono, sub or resub".into()),
            };
            let message = rest(2);
            Action::Kofi(KofiPayload {
                event_type,
                is_public: true,
                from_name: user.name.clone(),
                message: (!message.is_empty()).then_some(message),
                amount: (*amount).to_owned(),
                currency: "USD".into(),
                is_subscription_payment: sub,
                is_first_subscription_payment: first_sub,
                tier_name: None,
            })
        }
        _ => return Err(format!("unknown terminal command /{command}, see /help")),
    })
}

fn parse_noita(args: &[&str]) -> Result<NoitaEvent, String> {
    let usage = "usage: /noita death|item|pillar|flag|polymorph|oxygen|worm|portal|clutch|blind";
    let rest = || args.get(1..).unwrap_or_default().join(" ");
    Ok(match args.first().copied().ok_or(usage)? {
        "death" => {
            let mut win_state = WinState::Loss;
            let mut had_ambro = false;
            let mut reason = vec![];
            for arg in &args[1..] {
                match *arg {
                    "win" if reason.is_empty() => win_state = WinState::Win { cheese: false },
                    "cheese" if reason.is_empty() => win_state = WinState::Win { cheese: true },
                    "ambro" if reason.is_empty() => had_ambro = true,
                    word => reason.push(word),
                }
            }
            NoitaEvent::PlayerDeath {
                killed_by: reason.join(" "),
                run_duration: "0:13:37".into(),
                had_ambro,
                win_state,
            }
        }
        "item" => NoitaEvent::ItemFound(match args.get(1).copied() {
            Some("tree-tablet") => ItemFound::TreeTablet,
            Some("tablet") => ItemFound::OtherTablet,
            Some("evil-eye") => ItemFound::EvilEye,
            Some("earth-stone") => ItemFound::EarthStone,
            Some("touch-of-gold") => ItemFound::TouchOfGold,
            Some("taikasauva") => ItemFound::Taikasauva,
            Some("cov") => ItemFound::CircleOfVigour,
            Some("10-7") => ItemFound::TenSeven,
            Some("brine") => ItemFound::Brine,
            _ => return Err("unknown item, see /help".into()),
        }),
        "pillar" => NoitaEvent::PillarCompleted(rest()),
        "flag" => NoitaEvent::OtherPermanentFlag(rest()),
        "polymorph" => NoitaEvent::Polymorphed,
        "oxygen" => NoitaEvent::LowOxygen,
        "worm" => NoitaEvent::WormSummoned,
        "portal" => NoitaEvent::PortalEntered,
        "clutch" => NoitaEvent::OneHpClutch,
        "blind" => NoitaEvent::Blinded,
        _ => return Err(usage.into()),
    })
}
