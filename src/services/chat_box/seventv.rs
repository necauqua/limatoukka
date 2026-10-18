//! 7TV: the emote sets (REST API v3), the badges, paints and personal
//! emotes of the chatters (GraphQL API v4), and the live changes of the
//! channel emote set (EventAPI v3).

use std::{fmt::Write as _, time::Duration};

use anyhow::{Context as _, Result, bail};
use futures::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

use super::providers::{BadgeImg, EmoteDef, EmoteMap, get_json};

/// The 7TV data of the channel
#[derive(Debug, Clone, Default)]
pub struct Channel {
    /// The 7TV user id of the channel
    pub user_id: Option<String>,
    pub set_id: Option<String>,
    pub emotes: EmoteMap,
}

/// The cosmetics of a chatter
#[derive(Debug, Clone, Default)]
pub struct User {
    pub badge: Option<BadgeImg>,
    /// The CSS of the name
    pub paint: Option<String>,
    pub emotes: EmoteMap,
}

/// `flags` of an emote in a set
const ACTIVE_ZERO_WIDTH: u32 = 1 << 0;
/// `data.flags` of an emote
const EMOTE_ZERO_WIDTH: u32 = 1 << 8;

#[derive(Deserialize)]
struct V3Set {
    id: Option<String>,
    #[serde(default)]
    emotes: Option<Vec<V3ActiveEmote>>,
}

#[derive(Deserialize)]
struct V3ActiveEmote {
    name: String,
    #[serde(default)]
    flags: u32,
    data: Option<V3Emote>,
}

#[derive(Deserialize)]
struct V3Emote {
    #[serde(default)]
    flags: u32,
    host: V3Host,
}

#[derive(Deserialize)]
struct V3Host {
    url: String,
    #[serde(default)]
    files: Vec<V3File>,
}

#[derive(Deserialize)]
struct V3File {
    name: String,
}

fn v3_emotes(set: Option<V3Set>) -> EmoteMap {
    set.and_then(|set| set.emotes)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|emote| {
            let data = emote.data?;
            // the largest WEBP; it is animated when the emote is
            let file = ["4x.webp", "3x.webp", "2x.webp", "1x.webp"]
                .into_iter()
                .find(|name| data.host.files.iter().any(|file| file.name == *name))
                .unwrap_or("4x.webp");
            let base = data.host.url.trim_start_matches("https:");
            let def = EmoteDef {
                zero_width: emote.flags & ACTIVE_ZERO_WIDTH != 0
                    || data.flags & EMOTE_ZERO_WIDTH != 0,
                ..EmoteDef::new(&emote.name, format!("https:{base}/{file}"))
            };
            Some((emote.name, def))
        })
        .collect()
}

pub async fn channel(http: &reqwest::Client, channel_id: &str) -> Result<Channel> {
    #[derive(Deserialize)]
    struct Connection {
        emote_set: Option<V3Set>,
        user: Option<V3User>,
    }
    #[derive(Deserialize)]
    struct V3User {
        id: String,
    }
    let url = format!("https://7tv.io/v3/users/twitch/{channel_id}");
    let Some(connection) = get_json::<Connection>(http, &url).await? else {
        return Ok(Default::default());
    };
    Ok(Channel {
        user_id: connection.user.map(|user| user.id),
        set_id: connection.emote_set.as_ref().and_then(|set| set.id.clone()),
        emotes: v3_emotes(connection.emote_set),
    })
}

pub async fn global(http: &reqwest::Client) -> Result<EmoteMap> {
    let set = get_json::<V3Set>(http, "https://7tv.io/v3/emote-sets/global").await?;
    Ok(v3_emotes(set))
}

const USER_QUERY: &str = r#"
query($id: String!) { users { userByConnection(platform: TWITCH, platformId: $id) {
  style {
    activeBadge { name images { url mime scale frameCount } }
    activePaint { data {
      layers { opacity ty { __typename
        ... on PaintLayerTypeSingleColor { color { hex } }
        ... on PaintLayerTypeLinearGradient { angle repeating stops { at color { hex } } }
        ... on PaintLayerTypeRadialGradient { repeating shape stops { at color { hex } } }
        ... on PaintLayerTypeImage { images { url mime scale frameCount } }
      } }
      shadows { color { hex } offsetX offsetY blur }
    } }
  }
  personalEmoteSet { emotes { items { alias flags { zeroWidth } emote { id } } } }
} } }
"#;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlUser {
    style: Option<GqlStyle>,
    personal_emote_set: Option<GqlEmoteSet>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlStyle {
    active_badge: Option<GqlBadge>,
    active_paint: Option<GqlPaint>,
}

#[derive(Deserialize)]
struct GqlBadge {
    name: String,
    images: Vec<GqlImage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlImage {
    url: String,
    mime: String,
    scale: u32,
    frame_count: u32,
}

/// The largest animated WEBP, else the largest WEBP.
fn best_image(images: &[GqlImage]) -> Option<&str> {
    images
        .iter()
        .filter(|image| image.mime == "image/webp")
        .max_by_key(|image| (image.frame_count > 1, image.scale))
        .map(|image| &*image.url)
}

#[derive(Deserialize)]
struct GqlPaint {
    data: GqlPaintData,
}

#[derive(Deserialize)]
struct GqlPaintData {
    layers: Vec<GqlLayer>,
    shadows: Vec<GqlShadow>,
}

#[derive(Deserialize)]
struct GqlLayer {
    ty: GqlLayerType,
    #[serde(default = "full_opacity")]
    opacity: f64,
}

fn full_opacity() -> f64 {
    1.0
}

#[derive(Deserialize)]
struct GqlColor {
    hex: String,
}

#[derive(Deserialize)]
struct GqlStop {
    at: f64,
    color: GqlColor,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum GqlLayerType {
    #[serde(rename = "PaintLayerTypeSingleColor")]
    SingleColor { color: GqlColor },
    #[serde(rename = "PaintLayerTypeLinearGradient")]
    LinearGradient {
        angle: i64,
        repeating: bool,
        stops: Vec<GqlStop>,
    },
    #[serde(rename = "PaintLayerTypeRadialGradient")]
    RadialGradient {
        repeating: bool,
        shape: String,
        stops: Vec<GqlStop>,
    },
    #[serde(rename = "PaintLayerTypeImage")]
    Image { images: Vec<GqlImage> },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlShadow {
    color: GqlColor,
    offset_x: f64,
    offset_y: f64,
    blur: f64,
}

#[derive(Deserialize)]
struct GqlEmoteSet {
    emotes: GqlEmotes,
}

#[derive(Deserialize)]
struct GqlEmotes {
    items: Vec<GqlActiveEmote>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlActiveEmote {
    alias: String,
    flags: GqlEmoteFlags,
    emote: Option<GqlEmote>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlEmoteFlags {
    zero_width: bool,
}

#[derive(Deserialize)]
struct GqlEmote {
    id: String,
}

/// The cosmetics of a Twitch user; `None` if the user has no 7TV account.
pub async fn user(http: &reqwest::Client, twitch_id: &str) -> Result<Option<User>> {
    #[derive(Deserialize)]
    struct Response {
        data: Option<Data>,
        #[serde(default)]
        errors: Option<serde_json::Value>,
    }
    #[derive(Deserialize)]
    struct Data {
        users: Users,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Users {
        user_by_connection: Option<GqlUser>,
    }
    let response: Response = http
        .post("https://7tv.io/v4/gql")
        .json(&json!({ "query": USER_QUERY, "variables": { "id": twitch_id } }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let Some(data) = response.data else {
        bail!("7TV GraphQL errors: {:?}", response.errors);
    };
    let Some(user) = data.users.user_by_connection else {
        return Ok(None);
    };
    let style = user.style;
    let badge = style
        .as_ref()
        .and_then(|style| style.active_badge.as_ref())
        .and_then(|badge| {
            Some(BadgeImg {
                url: best_image(&badge.images)?.to_owned(),
                title: badge.name.clone(),
                background: None,
            })
        });
    let paint = style
        .as_ref()
        .and_then(|style| style.active_paint.as_ref())
        .and_then(|paint| paint_css(&paint.data));
    let emotes = user
        .personal_emote_set
        .map(|set| set.emotes.items)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| {
            let def = EmoteDef {
                zero_width: item.flags.zero_width,
                ..EmoteDef::new(
                    &item.alias,
                    format!("https://cdn.7tv.app/emote/{}/4x.webp", item.emote?.id),
                )
            };
            Some((item.alias, def))
        })
        .collect();
    Ok(Some(User {
        badge,
        paint,
        emotes,
    }))
}

fn is_hex_color(color: &str) -> bool {
    color.strip_prefix('#').is_some_and(|hex| {
        matches!(hex.len(), 3 | 4 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

/// A color with its alpha multiplied by the opacity of the layer, as
/// `#RRGGBBAA`.
fn with_opacity(color: &str, opacity: f64) -> Option<String> {
    if !is_hex_color(color) {
        return None;
    }
    if opacity >= 1.0 {
        return Some(color.to_owned());
    }
    let hex = &color[1..];
    let expand = |s: &str| s.chars().flat_map(|c| [c, c]).collect::<String>();
    let full = match hex.len() {
        3 => expand(hex) + "ff",
        4 => expand(hex),
        6 => format!("{hex}ff"),
        _ => hex.to_owned(),
    };
    let alpha = u8::from_str_radix(&full[6..8], 16).ok()? as f64;
    let alpha = (alpha * opacity.max(0.0)).round() as u8;
    Some(format!("#{}{alpha:02x}", &full[..6]))
}

fn stops(stops: &[GqlStop], opacity: f64) -> Option<String> {
    let mut css = String::new();
    for stop in stops {
        let color = with_opacity(&stop.color.hex, opacity)?;
        _ = write!(css, ", {color} {:.1}%", stop.at * 100.0);
    }
    Some(css)
}

/// A URL in CSS `url("...")`.
pub fn css_url(url: &str) -> String {
    format!(
        "url(\"{}\")",
        url.replace('\\', "%5C")
            .replace('"', "%22")
            .replace('\n', "%0A")
    )
}

/// 7TV gives the shadow sizes in px for its 13 px chat font; they scale with
/// the font here.
const SHADOW_SCALE: f64 = 1.0 / 13.0;

/// The CSS of a name with a paint: the layers clipped to the text, and the
/// shadows as `drop-shadow` filters (a `text-shadow` paints over a clipped
/// background). As on the 7TV website, the first layer is at the bottom, all
/// layers are `100% 100%`, and the name color is under them. The opacity of
/// image layers is not used.
fn paint_css(paint: &GqlPaintData) -> Option<String> {
    let mut images = Vec::new();
    for layer in &paint.layers {
        let opacity = layer.opacity;
        let image = match &layer.ty {
            GqlLayerType::SingleColor { color } => {
                let Some(color) = with_opacity(&color.hex, opacity) else {
                    continue;
                };
                format!("linear-gradient({color}, {color})")
            }
            GqlLayerType::LinearGradient {
                angle,
                repeating,
                stops: s,
            } if !s.is_empty() => {
                let repeating = if *repeating { "repeating-" } else { "" };
                format!(
                    "{repeating}linear-gradient({angle}deg{})",
                    stops(s, opacity)?
                )
            }
            GqlLayerType::RadialGradient {
                repeating,
                shape,
                stops: s,
            } if !s.is_empty() => {
                let repeating = if *repeating { "repeating-" } else { "" };
                let shape = if shape == "CIRCLE" {
                    "circle"
                } else {
                    "ellipse"
                };
                format!("{repeating}radial-gradient({shape}{})", stops(s, opacity)?)
            }
            GqlLayerType::Image { images } => {
                let Some(image) = best_image(images) else {
                    continue;
                };
                css_url(&super::images::local_url(image))
            }
            _ => continue,
        };
        images.push(image);
    }
    if images.is_empty() {
        return None;
    }
    // CSS lists the top background first
    images.reverse();
    let mut filter = String::new();
    for shadow in &paint.shadows {
        if is_hex_color(&shadow.color.hex) {
            _ = write!(
                filter,
                "drop-shadow({:.3}em {:.3}em {:.3}em {}) ",
                shadow.offset_x * SHADOW_SCALE,
                shadow.offset_y * SHADOW_SCALE,
                shadow.blur * SHADOW_SCALE,
                shadow.color.hex
            );
        }
    }
    // the outline of all the text, see `body` in chat-box.css
    filter.push_str("drop-shadow(0 0 1px #000) drop-shadow(0 0 2px #000)");
    Some(format!(
        "background-image: {}; background-size: 100% 100%; background-color: currentColor; \
         -webkit-background-clip: text; background-clip: text; \
         -webkit-text-fill-color: transparent; text-shadow: none; filter: {filter};",
        images.join(", "),
    ))
}

#[derive(Deserialize)]
struct EventMessage {
    op: u32,
    #[serde(default)]
    d: serde_json::Value,
}

/// A change from the 7TV EventAPI
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    /// The channel emote set, or the active set of the channel
    Channel,
    /// The badge, paint or personal emotes of the users with these Twitch ids
    Cosmetics(Vec<String>),
}

/// The Twitch ids of the user of an entitlement dispatch.
fn entitlement_users(body: &serde_json::Value) -> Vec<String> {
    body.pointer("/object/user/connections")
        .and_then(|connections| connections.as_array())
        .into_iter()
        .flatten()
        .filter(|connection| connection["platform"] == "TWITCH")
        .filter_map(|connection| Some(connection["id"].as_str()?.to_owned()))
        .collect()
}

fn change(dispatch: &serde_json::Value) -> Option<Change> {
    let kind = dispatch["type"].as_str()?;
    if kind.starts_with("entitlement.") {
        let users = entitlement_users(&dispatch["body"]);
        return (!users.is_empty()).then_some(Change::Cosmetics(users));
    }
    matches!(kind, "emote_set.update" | "user.update").then_some(Change::Channel)
}

/// Watches on the 7TV EventAPI: the channel emote set, the channel user (a
/// change of the active set), and the cosmetics of the chatters in the
/// channel (`channel_id` is the Twitch id). Calls `on_change` on each
/// change. Returns when the connection ends; the caller reconnects.
pub async fn watch(
    set_id: &str,
    user_id: &str,
    channel_id: &str,
    mut on_change: impl FnMut(Change),
) -> Result<()> {
    let (mut ws, _) = tokio_tungstenite::connect_async("wss://events.7tv.io/v3").await?;
    let mut heartbeat = Duration::from_secs(45);
    loop {
        // the server sends a heartbeat each interval
        let message = tokio::time::timeout(heartbeat * 3, ws.next())
            .await
            .context("7TV EventAPI: no heartbeat")?;
        let Some(message) = message else {
            return Ok(());
        };
        let text = match message? {
            Message::Text(text) => text,
            Message::Close(frame) => {
                tracing::info!(?frame, "7TV EventAPI closed");
                return Ok(());
            }
            _ => continue,
        };
        let message: EventMessage = serde_json::from_str(&text)?;
        match message.op {
            // hello
            1 => {
                if let Some(ms) = message.d["heartbeat_interval"].as_u64() {
                    heartbeat = Duration::from_millis(ms);
                }
                let channel = json!({ "ctx": "channel", "platform": "TWITCH", "id": channel_id });
                let subscriptions = [
                    ("emote_set.update", json!({ "object_id": set_id })),
                    ("user.update", json!({ "object_id": user_id })),
                    ("entitlement.create", channel.clone()),
                    ("entitlement.delete", channel),
                ];
                for (kind, condition) in subscriptions {
                    let subscribe = json!({
                        "op": 35,
                        "d": { "type": kind, "condition": condition },
                    });
                    ws.send(Message::text(subscribe.to_string())).await?;
                }
                tracing::info!(set_id, "7TV EventAPI: watching the channel");
            }
            // dispatch
            0 => {
                if let Some(change) = change(&message.d) {
                    on_change(change);
                }
            }
            // reconnect, end of stream
            4 | 7 => {
                tracing::info!(op = message.op, d = ?message.d, "7TV EventAPI asks to reconnect");
                return Ok(());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v3_set() {
        let set: V3Set = serde_json::from_value(json!({
            "id": "set",
            "emotes": [
                { "name": "RATS", "flags": 1, "data": { "flags": 256, "host": {
                    "url": "//cdn.7tv.app/emote/A",
                    "files": [{ "name": "1x.webp" }, { "name": "2x.webp" }],
                } } },
                { "name": "ICANT", "flags": 0, "data": { "flags": 0, "host": {
                    "url": "//cdn.7tv.app/emote/B", "files": [],
                } } },
                { "name": "gone", "flags": 0, "data": null },
            ],
        }))
        .unwrap();
        let emotes = v3_emotes(Some(set));
        assert_eq!(emotes.len(), 2);
        assert_eq!(emotes["RATS"].url, "https://cdn.7tv.app/emote/A/2x.webp");
        assert!(emotes["RATS"].zero_width);
        assert_eq!(emotes["ICANT"].url, "https://cdn.7tv.app/emote/B/4x.webp");
        assert!(!emotes["ICANT"].zero_width);
    }

    #[test]
    fn paint() {
        let paint: GqlPaintData = serde_json::from_value(json!({
            "layers": [
                { "opacity": 1.0, "ty": { "__typename": "PaintLayerTypeRadialGradient",
                    "repeating": false, "shape": "CIRCLE",
                    "stops": [{ "at": 0.0, "color": { "hex": "#FFD5FAFF" } },
                              { "at": 1.0, "color": { "hex": "#8384DFFF" } }] } },
                { "opacity": 1.0, "ty": { "__typename": "PaintLayerTypeSomethingNew" } },
            ],
            "shadows": [{ "color": { "hex": "#8384DFFF" }, "offsetX": 0.0, "offsetY": 0.0, "blur": 0.5 }],
        }))
        .unwrap();
        let css = paint_css(&paint).unwrap();
        assert!(css.contains("radial-gradient(circle, #FFD5FAFF 0.0%, #8384DFFF 100.0%)"));
        assert!(css.contains("drop-shadow(0.000em 0.000em 0.038em #8384DFFF)"));
        assert!(css.contains("background-clip: text"));
    }

    #[test]
    fn paint_layers() {
        let paint: GqlPaintData = serde_json::from_value(json!({
            "layers": [
                { "opacity": 1.0, "ty": { "__typename": "PaintLayerTypeSingleColor",
                    "color": { "hex": "#FF0000" } } },
                { "opacity": 0.5, "ty": { "__typename": "PaintLayerTypeLinearGradient",
                    "angle": 90, "repeating": true,
                    "stops": [{ "at": 0.0, "color": { "hex": "#00FF00FF" } },
                              { "at": 1.0, "color": { "hex": "#0000FF" } }] } },
            ],
            "shadows": [],
        }))
        .unwrap();
        let css = paint_css(&paint).unwrap();
        // the last layer is on top, so it is first in CSS; with the opacity
        assert!(
            css.contains(
                "background-image: repeating-linear-gradient(90deg, #00FF0080 0.0%, #0000FF80 100.0%), \
                 linear-gradient(#FF0000, #FF0000);"
            ),
            "{css}"
        );
    }

    #[test]
    fn changes() {
        let dispatch = json!({
            "type": "entitlement.create",
            "body": { "object": { "kind": "PAINT", "user": { "connections": [
                { "platform": "YOUTUBE", "id": "y" },
                { "platform": "TWITCH", "id": "160410682" },
            ] } } },
        });
        assert_eq!(
            change(&dispatch),
            Some(Change::Cosmetics(vec!["160410682".into()]))
        );
        assert_eq!(
            change(&json!({ "type": "emote_set.update" })),
            Some(Change::Channel)
        );
        assert_eq!(change(&json!({ "type": "cosmetic.create" })), None);
    }

    #[test]
    fn bad_colors_are_dropped() {
        let paint: GqlPaintData = serde_json::from_value(json!({
            "layers": [{ "ty": { "__typename": "PaintLayerTypeSingleColor",
                "color": { "hex": "red; background: url(x)" } } }],
            "shadows": [],
        }))
        .unwrap();
        assert_eq!(paint_css(&paint), None);
    }

    #[tokio::test]
    #[ignore = "manual test"]
    async fn real_user() -> Result<()> {
        let http = reqwest::Client::new();
        let user = user(&http, "160410682").await?.unwrap();
        println!("{user:#?}");
        let channel = channel(&http, "160410682").await?;
        println!(
            "{:?} {:?} {}",
            channel.user_id,
            channel.set_id,
            channel.emotes.len()
        );
        Ok(())
    }

    /// Connects to the EventAPI and watches the channel for 10 s.
    #[tokio::test]
    #[ignore = "manual test"]
    async fn real_events() -> Result<()> {
        let channel = channel(&reqwest::Client::new(), "160410682").await?;
        let (set_id, user_id) = (channel.set_id.unwrap(), channel.user_id.unwrap());
        let watch = watch(&set_id, &user_id, "160410682", |change| {
            println!("{change:?}")
        });
        match tokio::time::timeout(Duration::from_secs(10), watch).await {
            Err(_) => Ok(()), // still connected
            Ok(result) => {
                result?;
                bail!("the connection ended")
            }
        }
    }
}
