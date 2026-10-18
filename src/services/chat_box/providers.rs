//! The emote, badge and cheermote data of Twitch, 7TV, BTTV and FFZ.

use std::collections::HashMap;

use anyhow::Result;
use bitflags::bitflags;
use reqwest::StatusCode;
use serde::{Deserialize, de::DeserializeOwned};
use twitch_api::helix::{
    bits::GetCheermotesRequest,
    chat::{BadgeSet, GetChannelChatBadgesRequest, GetGlobalChatBadgesRequest},
};

use super::seventv;
use crate::integration::twitch_api::TwitchApi;

bitflags! {
    /// The changes that BTTV and FFZ modifiers make to an emote
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Mods: u32 {
        const FLIP_X = 1 << 0;
        const FLIP_Y = 1 << 1;
        const WIDE = 1 << 2;
        const ROTATE_LEFT = 1 << 3;
        const ROTATE_RIGHT = 1 << 4;
        const CURSED = 1 << 5;
        const RAINBOW = 1 << 6;
        const SHAKE = 1 << 7;
        /// No space before the emote
        const ZERO_SPACE = 1 << 8;
        const SLIDE = 1 << 9;
        const ARRIVE = 1 << 10;
        const LEAVE = 1 << 11;
        const SPIN = 1 << 12;
        const HYPER_RED = 1 << 13;
        const JAM = 1 << 14;
        const BOUNCE = 1 << 15;
    }
}

impl Mods {
    /// The CSS classes of the emote
    pub fn classes(self) -> impl Iterator<Item = &'static str> {
        const CLASSES: &[(Mods, &str)] = &[
            (Mods::FLIP_X, "mod-flip-x"),
            (Mods::FLIP_Y, "mod-flip-y"),
            (Mods::WIDE, "mod-wide"),
            (Mods::ROTATE_LEFT, "mod-rotate-left"),
            (Mods::ROTATE_RIGHT, "mod-rotate-right"),
            (Mods::CURSED, "mod-cursed"),
            (Mods::RAINBOW, "mod-rainbow"),
            (Mods::SHAKE, "mod-shake"),
            (Mods::SLIDE, "mod-slide"),
            (Mods::ARRIVE, "mod-arrive"),
            (Mods::LEAVE, "mod-leave"),
            (Mods::SPIN, "mod-spin"),
            (Mods::HYPER_RED, "mod-hyper-red"),
            (Mods::JAM, "mod-jam"),
            (Mods::BOUNCE, "mod-bounce"),
        ];
        CLASSES
            .iter()
            .filter(move |(mods, _)| self.contains(*mods))
            .map(|(_, class)| *class)
    }

    /// The FFZ `modifier_flags`
    fn from_ffz(flags: u32) -> Self {
        const FLAGS: &[(u32, Mods)] = &[
            (1 << 1, Mods::FLIP_X),
            (1 << 2, Mods::FLIP_Y),
            (1 << 3, Mods::WIDE),
            (1 << 4, Mods::SLIDE),
            (1 << 5, Mods::ARRIVE),
            (1 << 6, Mods::LEAVE),
            (1 << 7, Mods::SPIN),
            (1 << 11, Mods::RAINBOW),
            (1 << 12, Mods::HYPER_RED),
            (1 << 13, Mods::SHAKE),
            (1 << 14, Mods::CURSED),
            (1 << 15, Mods::JAM),
            (1 << 16, Mods::BOUNCE),
        ];
        FLAGS
            .iter()
            .filter(|(flag, _)| flags & flag != 0)
            .fold(Mods::empty(), |mods, (_, m)| mods | *m)
    }

    /// A BTTV modifier code (`w!` etc.)
    fn from_bttv(code: &str) -> Option<Self> {
        Some(match code {
            "w!" => Mods::WIDE,
            "h!" => Mods::FLIP_X,
            "v!" => Mods::FLIP_Y,
            "z!" => Mods::ZERO_SPACE,
            "l!" => Mods::ROTATE_LEFT,
            "r!" => Mods::ROTATE_RIGHT,
            "c!" => Mods::CURSED,
            "p!" => Mods::RAINBOW,
            "s!" => Mods::SHAKE,
            _ => return None,
        })
    }
}

/// An emote that is not shown, but changes another emote
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Modifier {
    pub mods: Mods,
    /// BTTV modifiers change the next emote, FFZ modifiers the one before
    pub before: bool,
}

/// An emote that is matched by its name (all except the Twitch ones).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmoteDef {
    pub name: String,
    pub url: String,
    /// Shown on top of the emote before it
    pub zero_width: bool,
    pub modifier: Option<Modifier>,
}

impl EmoteDef {
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            zero_width: false,
            modifier: None,
        }
    }
}

pub type EmoteMap = HashMap<String, EmoteDef>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadgeImg {
    pub url: String,
    pub title: String,
    /// FFZ badges are an icon on a colored background
    pub background: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CheerTier {
    pub min_bits: u64,
    pub color: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct Cheermote {
    /// Lowercase
    pub prefix: String,
    /// Ascending `min_bits`
    pub tiers: Vec<CheerTier>,
}

impl Cheermote {
    /// The tier of a cheer amount.
    pub fn tier(&self, bits: u64) -> Option<&CheerTier> {
        self.tiers.iter().rev().find(|tier| tier.min_bits <= bits)
    }
}

#[derive(Debug, Clone, Default)]
pub struct FfzRoom {
    pub emotes: EmoteMap,
    /// A custom moderator badge of the channel
    pub mod_badge: Option<BadgeImg>,
    /// A custom VIP badge of the channel
    pub vip_badge: Option<BadgeImg>,
    /// User id → the FFZ badge ids that the channel gave
    pub user_badges: HashMap<String, Vec<u64>>,
}

#[derive(Debug, Clone)]
pub struct FfzBadge {
    pub img: BadgeImg,
    /// The Twitch badge that this badge is shown instead of
    pub replaces: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FfzBadges {
    pub badges: HashMap<u64, FfzBadge>,
    /// User id → badge ids
    pub users: HashMap<String, Vec<u64>>,
}

/// FFZ global emote sets that only some users have (e.g. the animated
/// modifiers of the FFZ subscribers)
#[derive(Debug, Clone, Default)]
pub struct FfzUserSets {
    /// Set id → emotes
    pub sets: HashMap<String, EmoteMap>,
    /// Login → set ids
    pub users: HashMap<String, Vec<String>>,
}

impl FfzUserSets {
    pub fn of<'a>(&'a self, login: &str) -> impl Iterator<Item = &'a EmoteMap> {
        self.users
            .get(login)
            .into_iter()
            .flatten()
            .filter_map(|set| self.sets.get(set))
    }
}

/// (set id, version) → image
pub type TwitchBadges = HashMap<(String, String), BadgeImg>;

/// All the data that the chat box needs to show a message. When a source
/// fails, its previous data stays.
#[derive(Debug, Clone, Default)]
pub struct Providers {
    pub twitch_global_badges: TwitchBadges,
    /// They replace the global ones
    pub twitch_channel_badges: TwitchBadges,
    pub cheermotes: Vec<Cheermote>,
    pub seventv_channel: seventv::Channel,
    pub seventv_global: EmoteMap,
    pub bttv_channel: EmoteMap,
    pub bttv_global: EmoteMap,
    /// User id → badge
    pub bttv_badges: HashMap<String, BadgeImg>,
    pub ffz_room: FfzRoom,
    pub ffz_global: EmoteMap,
    pub ffz_user_sets: FfzUserSets,
    pub ffz_badges: FfzBadges,
    /// User id → badges
    pub chatterino_badges: HashMap<String, Vec<BadgeImg>>,
}

fn keep<T>(what: &str, result: Result<T>, old: &T) -> T
where
    T: Clone,
{
    result.unwrap_or_else(|error| {
        tracing::warn!(
            ?error,
            "chat box: failed to get the {what}, the old data stays"
        );
        old.clone()
    })
}

impl Providers {
    /// Gets all the data again.
    pub async fn refreshed(
        &self,
        http: &reqwest::Client,
        twitch: Option<&TwitchApi>,
        channel_id: &str,
    ) -> Self {
        let twitch_global_badges = async {
            match twitch {
                Some(api) => twitch_global_badges(api).await,
                None => Ok(Default::default()),
            }
        };
        let twitch_channel_badges = async {
            match twitch {
                Some(api) => twitch_channel_badges(api, channel_id).await,
                None => Ok(Default::default()),
            }
        };
        let cheermotes = async {
            match twitch {
                Some(api) => cheermotes(api, channel_id).await,
                None => Ok(Default::default()),
            }
        };
        let (
            twitch_global_badges,
            twitch_channel_badges,
            cheermotes,
            seventv_channel,
            seventv_global,
            bttv_channel,
            bttv_global,
            bttv_badges,
            ffz_room,
            ffz_global,
            ffz_badges,
            chatterino_badges,
        ) = tokio::join!(
            twitch_global_badges,
            twitch_channel_badges,
            cheermotes,
            seventv::channel(http, channel_id),
            seventv::global(http),
            bttv_channel(http, channel_id),
            bttv_global(http),
            bttv_badges(http),
            ffz_room(http, channel_id),
            ffz_global(http),
            ffz_badges(http),
            chatterino_badges(http),
        );
        let (ffz_global, ffz_user_sets) = match ffz_global {
            Ok((global, user_sets)) => (Ok(global), Ok(user_sets)),
            Err(error) => (Err(error), Err(anyhow::anyhow!("see the FFZ globals"))),
        };
        Self {
            twitch_global_badges: keep(
                "Twitch global badges",
                twitch_global_badges,
                &self.twitch_global_badges,
            ),
            twitch_channel_badges: keep(
                "Twitch channel badges",
                twitch_channel_badges,
                &self.twitch_channel_badges,
            ),
            cheermotes: keep("cheermotes", cheermotes, &self.cheermotes),
            seventv_channel: keep("7TV channel", seventv_channel, &self.seventv_channel),
            seventv_global: keep("7TV globals", seventv_global, &self.seventv_global),
            bttv_channel: keep("BTTV channel", bttv_channel, &self.bttv_channel),
            bttv_global: keep("BTTV globals", bttv_global, &self.bttv_global),
            bttv_badges: keep("BTTV badges", bttv_badges, &self.bttv_badges),
            ffz_room: keep("FFZ room", ffz_room, &self.ffz_room),
            ffz_global: keep("FFZ globals", ffz_global, &self.ffz_global),
            ffz_user_sets: keep("FFZ user sets", ffz_user_sets, &self.ffz_user_sets),
            ffz_badges: keep("FFZ badges", ffz_badges, &self.ffz_badges),
            chatterino_badges: keep(
                "Chatterino badges",
                chatterino_badges,
                &self.chatterino_badges,
            ),
        }
    }
}

/// A GET of JSON; `None` on 404 (e.g. a channel that does not use the
/// service).
pub(super) async fn get_json<T: DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
) -> Result<Option<T>> {
    let response = http.get(url).send().await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(response.error_for_status()?.json().await?))
}

async fn twitch_global_badges(api: &TwitchApi) -> Result<TwitchBadges> {
    let sets: Vec<BadgeSet> = api
        .call(async |t| {
            t.helix
                .req_get(GetGlobalChatBadgesRequest::new(), &t.token)
                .await
                .map(|r| r.data)
        })
        .await?;
    Ok(badge_map(sets))
}

/// The badges of a channel (also of another channel in a shared chat).
pub async fn twitch_channel_badges(api: &TwitchApi, channel_id: &str) -> Result<TwitchBadges> {
    let sets: Vec<BadgeSet> = api
        .call(async |t| {
            t.helix
                .req_get(
                    GetChannelChatBadgesRequest::broadcaster_id(channel_id),
                    &t.token,
                )
                .await
                .map(|r| r.data)
        })
        .await?;
    Ok(badge_map(sets))
}

fn badge_map(sets: Vec<BadgeSet>) -> TwitchBadges {
    let mut badges = HashMap::new();
    for set in sets {
        for version in set.versions {
            badges.insert(
                (
                    set.set_id.as_str().to_owned(),
                    version.id.as_str().to_owned(),
                ),
                BadgeImg {
                    url: version.image_url_4x,
                    title: version.title,
                    background: None,
                },
            );
        }
    }
    badges
}

async fn cheermotes(api: &TwitchApi, channel_id: &str) -> Result<Vec<Cheermote>> {
    let cheermotes = api
        .call(async |t| {
            t.helix
                .req_get(GetCheermotesRequest::broadcaster_id(channel_id), &t.token)
                .await
                .map(|r| r.data)
        })
        .await?;
    Ok(cheermotes
        .into_iter()
        .map(|cheermote| {
            let mut tiers = cheermote
                .tiers
                .into_iter()
                .map(|tier| CheerTier {
                    min_bits: tier.min_bits.max(0) as u64,
                    color: tier.color,
                    url: tier.images.dark.animated.url_4x,
                })
                .collect::<Vec<_>>();
            tiers.sort_by_key(|tier| tier.min_bits);
            Cheermote {
                prefix: cheermote.prefix.to_lowercase(),
                tiers,
            }
        })
        .collect())
}

/// The old BTTV overlay emotes; BTTV does not mark them in its API.
const BTTV_ZERO_WIDTH: &[&str] = &[
    "SoSnowy",
    "IceCold",
    "SantaHat",
    "TopHat",
    "ReinDeer",
    "CandyCane",
    "cvMask",
    "cvHazmat",
];

#[derive(Deserialize)]
struct BttvEmote {
    id: String,
    code: String,
    /// The modifiers (`w!`, `h!` etc.)
    #[serde(default)]
    modifier: bool,
}

fn bttv_emotes(emotes: impl IntoIterator<Item = BttvEmote>) -> EmoteMap {
    emotes
        .into_iter()
        .filter_map(|emote| {
            let modifier = if emote.modifier {
                let mods = Mods::from_bttv(&emote.code)?;
                Some(Modifier { mods, before: true })
            } else {
                None
            };
            Some((emote, modifier))
        })
        .map(|(emote, modifier)| {
            let def = EmoteDef {
                zero_width: BTTV_ZERO_WIDTH.contains(&&*emote.code),
                modifier,
                ..EmoteDef::new(
                    &emote.code,
                    format!("https://cdn.betterttv.net/emote/{}/3x.webp", emote.id),
                )
            };
            (emote.code, def)
        })
        .collect()
}

async fn bttv_channel(http: &reqwest::Client, channel_id: &str) -> Result<EmoteMap> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct User {
        #[serde(default)]
        channel_emotes: Vec<BttvEmote>,
        #[serde(default)]
        shared_emotes: Vec<BttvEmote>,
    }
    let url = format!("https://api.betterttv.net/3/cached/users/twitch/{channel_id}");
    let Some(user) = get_json::<User>(http, &url).await? else {
        return Ok(Default::default());
    };
    Ok(bttv_emotes(
        user.shared_emotes.into_iter().chain(user.channel_emotes),
    ))
}

async fn bttv_global(http: &reqwest::Client) -> Result<EmoteMap> {
    let emotes: Vec<BttvEmote> = get_json(http, "https://api.betterttv.net/3/cached/emotes/global")
        .await?
        .unwrap_or_default();
    Ok(bttv_emotes(emotes))
}

async fn bttv_badges(http: &reqwest::Client) -> Result<HashMap<String, BadgeImg>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct User {
        provider_id: String,
        badge: Badge,
    }
    #[derive(Deserialize)]
    struct Badge {
        description: String,
        svg: String,
    }
    let users: Vec<User> = get_json(http, "https://api.betterttv.net/3/cached/badges/twitch")
        .await?
        .unwrap_or_default();
    Ok(users
        .into_iter()
        .map(|user| {
            let badge = BadgeImg {
                url: user.badge.svg,
                title: user.badge.description,
                background: None,
            };
            (user.provider_id, badge)
        })
        .collect())
}

/// The background of the Twitch moderator badges. An FFZ badge that is shown
/// instead of them gets it (e.g. the bot badge of a moderator is a green
/// gear).
pub const MODERATOR_GREEN: &str = "#00AD03";
/// The background of the Twitch VIP badge
const VIP_PINK: &str = "#E005B9";

/// FFZ image URLs by scale (`"1"`, `"2"`, `"4"`)
type FfzUrls = HashMap<String, String>;

fn ffz_best(urls: &FfzUrls) -> Option<String> {
    ["4", "2", "1"]
        .iter()
        .find_map(|scale| urls.get(*scale))
        .map(|url| match url.strip_prefix("//") {
            Some(rest) => format!("https://{rest}"),
            None => url.clone(),
        })
}

#[derive(Deserialize)]
struct FfzEmote {
    name: String,
    urls: FfzUrls,
    #[serde(default)]
    animated: Option<FfzUrls>,
    #[serde(default)]
    modifier: bool,
    #[serde(default)]
    modifier_flags: u32,
}

#[derive(Deserialize)]
struct FfzSet {
    #[serde(default)]
    emoticons: Vec<FfzEmote>,
}

fn ffz_emotes<'a>(sets: impl IntoIterator<Item = &'a FfzSet>) -> EmoteMap {
    sets.into_iter()
        .flat_map(|set| &set.emoticons)
        .filter_map(|emote| {
            let url = emote
                .animated
                .as_ref()
                .and_then(ffz_best)
                .or_else(|| ffz_best(&emote.urls))?;
            let def = EmoteDef {
                modifier: emote.modifier.then(|| Modifier {
                    mods: Mods::from_ffz(emote.modifier_flags),
                    before: false,
                }),
                ..EmoteDef::new(&emote.name, url)
            };
            Some((emote.name.clone(), def))
        })
        .collect()
}

async fn ffz_room(http: &reqwest::Client, channel_id: &str) -> Result<FfzRoom> {
    #[derive(Deserialize)]
    struct Response {
        room: Room,
        #[serde(default)]
        sets: HashMap<String, FfzSet>,
    }
    #[derive(Deserialize)]
    struct Room {
        #[serde(default)]
        mod_urls: Option<FfzUrls>,
        #[serde(default)]
        vip_badge: Option<FfzUrls>,
        /// badge id → user ids
        #[serde(default)]
        user_badge_ids: Option<HashMap<String, Vec<u64>>>,
    }
    let url = format!("https://api.frankerfacez.com/v1/room/id/{channel_id}");
    let Some(response) = get_json::<Response>(http, &url).await? else {
        return Ok(Default::default());
    };
    let badge = |urls: Option<FfzUrls>, title: &str, background: &str| {
        Some(BadgeImg {
            url: ffz_best(&urls?)?,
            title: title.into(),
            background: Some(background.into()),
        })
    };
    let mut user_badges: HashMap<String, Vec<u64>> = HashMap::new();
    for (badge, users) in response.room.user_badge_ids.unwrap_or_default() {
        let Ok(badge) = badge.parse() else { continue };
        for user in users {
            user_badges.entry(user.to_string()).or_default().push(badge);
        }
    }
    Ok(FfzRoom {
        emotes: ffz_emotes(response.sets.values()),
        // the colors of the Twitch badges that they replace
        mod_badge: badge(response.room.mod_urls, "Moderator", MODERATOR_GREEN),
        vip_badge: badge(response.room.vip_badge, "VIP", VIP_PINK),
        user_badges,
    })
}

/// The global emotes of everyone, and the sets of some users.
async fn ffz_global(http: &reqwest::Client) -> Result<(EmoteMap, FfzUserSets)> {
    #[derive(Deserialize)]
    struct Response {
        default_sets: Vec<u64>,
        sets: HashMap<String, FfzSet>,
        /// Set id → logins
        #[serde(default)]
        users: HashMap<String, Vec<String>>,
    }
    let Some(response) =
        get_json::<Response>(http, "https://api.frankerfacez.com/v1/set/global").await?
    else {
        return Ok(Default::default());
    };
    let global = ffz_emotes(
        response
            .default_sets
            .iter()
            .filter_map(|id| response.sets.get(&id.to_string())),
    );
    let mut user_sets = FfzUserSets::default();
    for (set, logins) in response.users {
        let Some(emotes) = response.sets.get(&set) else {
            continue;
        };
        user_sets.sets.insert(set.clone(), ffz_emotes([emotes]));
        for login in logins {
            user_sets
                .users
                .entry(login.to_lowercase())
                .or_default()
                .push(set.clone());
        }
    }
    Ok((global, user_sets))
}

async fn chatterino_badges(http: &reqwest::Client) -> Result<HashMap<String, Vec<BadgeImg>>> {
    #[derive(Deserialize)]
    struct Response {
        badges: Vec<Badge>,
    }
    #[derive(Deserialize)]
    struct Badge {
        tooltip: String,
        image3: String,
        users: Vec<String>,
    }
    let Some(response) = get_json::<Response>(http, "https://api.chatterino.com/badges").await?
    else {
        return Ok(Default::default());
    };
    let mut users: HashMap<String, Vec<BadgeImg>> = HashMap::new();
    for badge in response.badges {
        let img = BadgeImg {
            url: badge.image3,
            title: badge.tooltip,
            background: None,
        };
        for user in badge.users {
            users.entry(user).or_default().push(img.clone());
        }
    }
    Ok(users)
}

async fn ffz_badges(http: &reqwest::Client) -> Result<FfzBadges> {
    #[derive(Deserialize)]
    struct Response {
        badges: Vec<Badge>,
        /// badge id → user ids
        users: HashMap<String, Vec<u64>>,
    }
    #[derive(Deserialize)]
    struct Badge {
        id: u64,
        title: String,
        #[serde(default)]
        replaces: Option<String>,
        #[serde(default)]
        color: Option<String>,
        urls: FfzUrls,
    }
    let Some(response) =
        get_json::<Response>(http, "https://api.frankerfacez.com/v1/badges/ids").await?
    else {
        return Ok(Default::default());
    };
    let badges = response
        .badges
        .into_iter()
        .filter_map(|badge| {
            let img = BadgeImg {
                url: ffz_best(&badge.urls)?,
                title: badge.title,
                background: badge.color,
            };
            let replaces = badge.replaces;
            Some((badge.id, FfzBadge { img, replaces }))
        })
        .collect();
    let mut users: HashMap<String, Vec<u64>> = HashMap::new();
    for (badge, ids) in response.users {
        let Ok(badge) = badge.parse() else { continue };
        for id in ids {
            users.entry(id.to_string()).or_default().push(badge);
        }
    }
    Ok(FfzBadges { badges, users })
}
