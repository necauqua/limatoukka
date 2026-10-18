//! A chat message as HTML, in the structure of the Streamlabs chat box (so
//! that its CSS works), with each message a table row group: the reply or
//! notice line, then the line with the name and the message.

use maud::{Markup, html};
use twitch_irc::message::RGBColor;

use super::{
    colors::{default_color, readable},
    emoji::{self, Run},
    images::local_url,
    message::ChatMessage,
    providers::{BadgeImg, EmoteDef, MODERATOR_GREEN, Mods, Providers, TwitchBadges},
    seventv::{self, css_url},
};

/// The other channel of a shared chat
#[derive(Debug, Clone, Default)]
pub struct Room {
    pub name: String,
    pub avatar: Option<String>,
    pub badges: TwitchBadges,
}

/// All that the rendering of a message needs, except the message.
pub struct Context<'a> {
    pub providers: &'a Providers,
    /// The 7TV cosmetics of the sender
    pub user: Option<&'a seventv::User>,
    /// The source channel of a shared chat message from another channel
    pub room: Option<&'a Room>,
    /// The name color of a login, if it is known
    pub mention_color: &'a dyn Fn(&str) -> Option<RGBColor>,
}

fn name_color(msg: &ChatMessage) -> String {
    let color = msg.color.unwrap_or_else(|| default_color(&msg.login));
    readable(color).to_string()
}

struct ShownBadge<'a> {
    class: String,
    url: &'a str,
    title: &'a str,
    background: Option<&'a str>,
}

impl<'a> ShownBadge<'a> {
    fn new(class: impl Into<String>, img: &'a BadgeImg) -> Self {
        Self {
            class: class.into(),
            url: &img.url,
            title: &img.title,
            background: img.background.as_deref(),
        }
    }
}

/// Whether an FFZ badge that replaces `replaces` replaces this Twitch badge.
fn ffz_replaces(replaces: &str, twitch: &str) -> bool {
    replaces == twitch || (replaces == "moderator" && twitch == "lead_moderator")
}

fn badges<'a>(msg: &ChatMessage, ctx: &Context<'a>) -> Vec<ShownBadge<'a>> {
    let providers = ctx.providers;
    let ffz = &providers.ffz_badges;
    let ffz_ids = ffz
        .users
        .get(&msg.user_id)
        .into_iter()
        .chain(providers.ffz_room.user_badges.get(&msg.user_id))
        .flatten()
        .filter_map(|id| Some((*id, ffz.badges.get(id)?)))
        .collect::<Vec<_>>();
    let mut replaced = Vec::new();

    let mut shown = Vec::new();
    // the badges are of the other channel; the FFZ channel badges do not apply
    let other_room = msg.source_room.is_some();
    if other_room
        && let Some(room) = ctx.room
        && let Some(avatar) = &room.avatar
    {
        shown.push(ShownBadge {
            class: "shared-chat-icon".into(),
            url: avatar,
            title: &room.name,
            background: None,
        });
    }
    for badge in &msg.badges {
        let ffz_replacement = ffz_ids.iter().find(|(_, b)| {
            b.replaces
                .as_deref()
                .is_some_and(|replaces| ffz_replaces(replaces, &badge.name))
        });
        if let Some((id, b)) = ffz_replacement {
            replaced.push(*id);
            let mut shown_badge = ShownBadge::new(format!("{}-icon ffz-icon", badge.name), &b.img);
            if matches!(&*badge.name, "moderator" | "lead_moderator") {
                shown_badge.background = Some(MODERATOR_GREEN);
            }
            shown.push(shown_badge);
            continue;
        }
        let key = (badge.name.clone(), badge.version.clone());
        let channel_badges = if other_room {
            ctx.room.map(|room| &room.badges)
        } else {
            Some(&providers.twitch_channel_badges)
        };
        let ffz_custom = match &*badge.name {
            _ if other_room => None,
            "moderator" => providers.ffz_room.mod_badge.as_ref(),
            "vip" => providers.ffz_room.vip_badge.as_ref(),
            _ => None,
        };
        let img = ffz_custom
            .or_else(|| channel_badges.and_then(|badges| badges.get(&key)))
            .or_else(|| providers.twitch_global_badges.get(&key));
        if let Some(img) = img {
            shown.push(ShownBadge::new(format!("{}-icon", badge.name), img));
        }
    }
    if let Some(img) = ctx.user.and_then(|user| user.badge.as_ref()) {
        shown.push(ShownBadge::new("seventv-icon", img));
    }
    for (id, badge) in ffz_ids {
        if !replaced.contains(&id) {
            shown.push(ShownBadge::new("ffz-icon", &badge.img));
        }
    }
    if let Some(img) = providers.bttv_badges.get(&msg.user_id) {
        shown.push(ShownBadge::new("bttv-icon", img));
    }
    for img in providers
        .chatterino_badges
        .get(&msg.user_id)
        .into_iter()
        .flatten()
    {
        shown.push(ShownBadge::new("chatterino-icon", img));
    }
    shown
}

/// One image of an emote
#[derive(Debug, Clone, PartialEq, Eq)]
struct Layer {
    url: String,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EmotePiece {
    /// The first layer is the emote, the others are zero-width emotes on it
    layers: Vec<Layer>,
    mods: Mods,
    /// The gigantify Power-up
    giant: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Text(String),
    Mention {
        name: String,
        color: Option<String>,
    },
    Emoji {
        url: String,
        alt: String,
    },
    Emote(EmotePiece),
    Cheer {
        url: String,
        bits: u64,
        color: String,
    },
}

/// Builds the pieces. BTTV modifiers wait for the next emote; when no emote
/// comes, they are text.
#[derive(Default)]
struct Pieces {
    pieces: Vec<Piece>,
    pending: Mods,
    /// The modifiers and the spaces after them, as text
    pending_text: String,
}

impl Pieces {
    fn push_text(&mut self, text: &str) {
        if let Some(Piece::Text(last)) = self.pieces.last_mut() {
            last.push_str(text);
        } else {
            self.pieces.push(Piece::Text(text.to_owned()));
        }
    }

    fn flush(&mut self) {
        self.pending = Mods::empty();
        if !self.pending_text.is_empty() {
            let text = std::mem::take(&mut self.pending_text);
            self.push_text(&text);
        }
    }

    fn space(&mut self) {
        if self.pending_text.is_empty() {
            self.push_text(" ");
        } else {
            self.pending_text.push(' ');
        }
    }

    fn word(&mut self, word: &str) {
        self.flush();
        self.push_text(word);
    }

    fn other(&mut self, piece: Piece) {
        self.flush();
        self.pieces.push(piece);
    }

    /// Removes the whitespace at the end.
    fn trim_end(&mut self) {
        if let Some(Piece::Text(text)) = self.pieces.last_mut() {
            text.truncate(text.trim_end().len());
            if text.is_empty() {
                self.pieces.pop();
            }
        }
    }

    /// The emote before, if there is only whitespace between.
    fn emote_before(&mut self) -> Option<&mut EmotePiece> {
        let space_between = matches!(
            &*self.pieces,
            [.., Piece::Emote(_), Piece::Text(space)] if space.trim().is_empty()
        );
        if space_between {
            self.pieces.pop();
        }
        match self.pieces.last_mut() {
            Some(Piece::Emote(emote)) => Some(emote),
            _ => None,
        }
    }

    /// A BTTV modifier: it changes the next emote.
    fn modifier_before(&mut self, word: &str, mods: Mods) {
        self.pending |= mods;
        self.pending_text.push_str(word);
    }

    /// An FFZ modifier: it changes the emote before it.
    fn modifier_after(&mut self, mods: Mods) {
        self.flush();
        if let Some(emote) = self.emote_before() {
            emote.mods |= mods;
        } else if let Some(Piece::Text(text)) = self.pieces.last_mut() {
            // the space before the modifier goes away with it
            if text.ends_with(' ') {
                text.pop();
                if text.is_empty() {
                    self.pieces.pop();
                }
            }
        }
    }

    fn emote(&mut self, layer: Layer, zero_width: bool) {
        let mods = std::mem::take(&mut self.pending);
        self.pending_text.clear();
        if zero_width
            && mods.is_empty()
            && let Some(emote) = self.emote_before()
        {
            emote.layers.push(layer);
            return;
        }
        if mods.contains(Mods::ZERO_SPACE) {
            self.trim_end();
        }
        self.pieces.push(Piece::Emote(EmotePiece {
            layers: vec![layer],
            mods: mods - Mods::ZERO_SPACE,
            giant: false,
        }));
    }

    fn finish(mut self) -> Vec<Piece> {
        self.flush();
        self.pieces
    }
}

struct Lookup<'a> {
    ctx: &'a Context<'a>,
    login: &'a str,
    bits: bool,
}

impl Lookup<'_> {
    /// The channel emotes first, then the personal ones, then the global ones.
    fn emote(&self, word: &str) -> Option<&EmoteDef> {
        let p = self.ctx.providers;
        [
            &p.seventv_channel.emotes,
            &p.bttv_channel,
            &p.ffz_room.emotes,
        ]
        .into_iter()
        .chain(self.ctx.user.map(|user| &user.emotes))
        .chain(p.ffz_user_sets.of(self.login))
        .chain([&p.seventv_global, &p.bttv_global, &p.ffz_global])
        .find_map(|emotes| emotes.get(word))
    }

    fn cheer(&self, word: &str) -> Option<Piece> {
        if !self.bits {
            return None;
        }
        let lower = word.to_lowercase();
        self.ctx.providers.cheermotes.iter().find_map(|cheermote| {
            let amount = lower.strip_prefix(&cheermote.prefix)?;
            if amount.is_empty() || !amount.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let bits = amount.parse().ok()?;
            let tier = cheermote.tier(bits)?;
            Some(Piece::Cheer {
                url: tier.url.clone(),
                bits,
                color: tier.color.clone(),
            })
        })
    }

    /// `@name` and the text after the name (e.g. `,`).
    fn mention<'w>(&self, word: &'w str) -> Option<(Piece, &'w str)> {
        let rest = word.strip_prefix('@')?;
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        let name = &rest[..end];
        let color = (self.ctx.mention_color)(&name.to_lowercase()).map(|c| readable(c).to_string());
        let piece = Piece::Mention {
            name: name.to_owned(),
            color,
        };
        Some((piece, &rest[end..]))
    }

    fn text(&self, pieces: &mut Pieces, text: &str) {
        for run in emoji::split(text) {
            match run {
                Run::Text(text) => pieces.word(text),
                Run::Emoji(emoji) => pieces.other(Piece::Emoji {
                    url: emoji::twemoji_url(emoji),
                    alt: emoji.to_owned(),
                }),
            }
        }
    }

    fn words(&self, pieces: &mut Pieces, text: &str) {
        for (i, word) in text.split(' ').enumerate() {
            if i != 0 {
                pieces.space();
            }
            if word.is_empty() {
                continue;
            }
            if let Some(cheer) = self.cheer(word) {
                pieces.other(cheer);
            } else if let Some(emote) = self.emote(word) {
                match emote.modifier {
                    Some(modifier) if modifier.before => {
                        pieces.modifier_before(word, modifier.mods)
                    }
                    Some(modifier) => pieces.modifier_after(modifier.mods),
                    None => {
                        let layer = Layer {
                            url: emote.url.clone(),
                            name: emote.name.clone(),
                        };
                        pieces.emote(layer, emote.zero_width);
                    }
                }
            } else if let Some((mention, rest)) = self.mention(word) {
                pieces.other(mention);
                self.text(pieces, rest);
            } else {
                self.text(pieces, word);
            }
        }
    }
}

fn twitch_emote_url(id: &str) -> String {
    format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/3.0")
}

fn pieces(msg: &ChatMessage, ctx: &Context) -> Vec<Piece> {
    let lookup = Lookup {
        ctx,
        login: &msg.login,
        bits: msg.bits.is_some_and(|bits| bits > 0),
    };
    let chars = msg.text.chars().collect::<Vec<_>>();
    let mut pieces = Pieces::default();
    let mut pos = 0;
    for emote in &msg.emotes {
        let range = emote.char_range.clone();
        if range.start < pos || range.start >= chars.len() {
            continue;
        }
        let before = chars[pos..range.start].iter().collect::<String>();
        lookup.words(&mut pieces, &before);
        let layer = Layer {
            url: twitch_emote_url(&emote.id),
            name: emote.code.clone(),
        };
        pieces.emote(layer, false);
        pos = range.end.min(chars.len());
    }
    let rest = chars[pos..].iter().collect::<String>();
    lookup.words(&mut pieces, &rest);
    let mut pieces = pieces.finish();
    if msg.giant_emote
        && let Some(Piece::Emote(emote)) = pieces
            .iter_mut()
            .rev()
            .find(|piece| matches!(piece, Piece::Emote(_)))
    {
        emote.giant = true;
    }
    pieces
}

fn render_emote(emote: &EmotePiece) -> Markup {
    let base = &emote.layers[0];
    let names = emote
        .layers
        .iter()
        .map(|layer| &*layer.name)
        .collect::<Vec<_>>()
        .join(" ");
    let mut class = String::from("emote");
    for mod_class in emote.mods.classes() {
        class.push(' ');
        class.push_str(mod_class);
    }
    if emote.giant {
        class.push_str(" giant");
    }
    let src = local_url(&base.url);
    html! {
        span class=(class) title=(names) {
            // the invisible images give the size, see chat-box.js
            img.spacer src=(src) alt=(names);
            @if emote.mods.contains(Mods::WIDE) {
                img.spacer src=(src) alt="";
            }
            span.layers {
                span.fx {
                    span.base style={ "background-image: " (css_url(&src)) } {}
                    @for layer in &emote.layers[1..] {
                        img.zw src=(local_url(&layer.url)) alt="";
                    }
                }
            }
        }
    }
}

fn render_piece(piece: &Piece) -> Markup {
    match piece {
        Piece::Text(text) => html! { (text) },
        Piece::Mention { name, color } => html! {
            span.mention style=[color.as_ref().map(|c| format!("color: {c}"))] { "@" (name) }
        },
        Piece::Emoji { url, alt } => html! {
            img.emoji src=(local_url(url)) alt=(alt);
        },
        Piece::Emote(emote) => render_emote(emote),
        Piece::Cheer { url, bits, color } => {
            let emote = EmotePiece {
                layers: vec![Layer {
                    url: url.clone(),
                    name: "cheer".into(),
                }],
                mods: Mods::empty(),
                giant: false,
            };
            html! {
                span.cheer {
                    (render_emote(&emote))
                    span.cheer-amount style={ "color: " (color) } { (bits) }
                }
            }
        }
    }
}

/// A CSS class name from a Twitch value, or nothing.
fn class_part(value: &str) -> Option<String> {
    let lower = value.to_lowercase();
    lower
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        .then_some(lower)
}

/// The colors of the announcements
fn announcement_color(color: &str) -> &'static str {
    match color {
        "BLUE" => "#00D6D6",
        "GREEN" => "#00DB84",
        "ORANGE" => "#FFB31A",
        "PURPLE" => "#9146FF",
        _ => "#9147FF",
    }
}

/// The display name; with the login when the name is localized (e.g.
/// `ネカ (neca_jp)`), as on Twitch.
fn display_name(name: &str, login: &str) -> String {
    if name.eq_ignore_ascii_case(login) || login.is_empty() {
        name.to_owned()
    } else {
        format!("{name} ({login})")
    }
}

/// A message in the structure of the Streamlabs chat box.
pub fn render(msg: &ChatMessage, ctx: &Context, deleted: bool) -> Markup {
    let color = name_color(msg);
    let notice = msg.notice.as_ref();
    let mut classes = Vec::new();
    for (on, class) in [
        (msg.is_action, "action"),
        (msg.first, "first"),
        (msg.highlighted, "highlighted"),
        (msg.giant_emote, "gigantified"),
        (msg.source_room.is_some(), "shared"),
        (notice.is_some(), "notice"),
        (msg.effect.is_some(), "effect"),
        (deleted, "deleted"),
    ] {
        if on {
            classes.push(class.to_owned());
        }
    }
    if let Some(kind) = notice.and_then(|n| class_part(&n.kind)) {
        classes.push(format!("notice-{kind}"));
    }
    if let Some(effect) = msg.effect.as_deref().and_then(class_part) {
        classes.push(format!("effect-{effect}"));
    }
    let class = (!classes.is_empty()).then(|| classes.join(" "));
    let notice_color = notice
        .and_then(|n| n.color.as_deref())
        .map(|c| format!("--notice-color: {}", announcement_color(c)));

    let paint = ctx.user.and_then(|user| user.paint.as_deref());
    let name = display_name(&msg.name, &msg.login);
    // a notice without a message is only the text from Twitch
    let system_only = notice.filter(|_| msg.text.trim().is_empty());
    let system_line = notice.filter(|_| system_only.is_none()).map(|n| {
        if n.kind == "announcement" && n.system.is_empty() {
            "Announcement"
        } else {
            &*n.system
        }
    });
    let reply_color = msg.reply.as_ref().and_then(|reply| {
        let color = (ctx.mention_color)(&reply.login.to_lowercase())?;
        Some(format!("color: {}", readable(color)))
    });
    html! {
        div class=[class] style=[notice_color] data-from=(msg.name) data-id=(msg.id) data-user-id=(msg.user_id) {
            @if let Some(reply) = &msg.reply {
                div.above-row {
                    span.meta {}
                    span.above.reply {
                        span.reply-arrow { "↪" } " "
                        span.reply-name style=[reply_color] { "@" (display_name(&reply.name, &reply.login)) }
                        ": " (reply.text)
                    }
                }
            }
            @if let Some(system) = system_line.filter(|s| !s.is_empty()) {
                div.above-row {
                    span.meta {}
                    span.above.system { (system) }
                }
            }
            div.line {
                @if let Some(notice) = system_only {
                    span.meta {}
                    span.message.system { (notice.system) }
                } @else {
                    span.meta style={ "color: " (color) } {
                        span.badges {
                            @for badge in badges(msg, ctx) {
                                img class={ "badge " (badge.class) }
                                    src=(local_url(badge.url))
                                    title=(badge.title)
                                    style=[badge.background.map(|c| format!("background-color: {c}"))];
                            }
                        }
                        span.name style=[paint] { (name) }
                    }
                    span.message style=[msg.is_action.then(|| format!("color: {color}"))] {
                        @for piece in pieces(msg, ctx) {
                            (render_piece(&piece))
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use twitch_irc::message::{Badge, Emote};

    use super::*;
    use crate::services::chat_box::{
        message::{Notice, Reply},
        providers::{CheerTier, Cheermote, FfzBadge, Modifier},
    };

    fn message(text: &str) -> ChatMessage {
        ChatMessage {
            id: "m".into(),
            ..ChatMessage::new("1", "chatter", "Chatter", text)
        }
    }

    fn emote(name: &str, zero_width: bool) -> (String, EmoteDef) {
        let def = EmoteDef {
            zero_width,
            ..EmoteDef::new(name, format!("https://cdn.7tv.app/emote/{name}/4x.webp"))
        };
        (name.to_owned(), def)
    }

    fn modifier(name: &str, mods: Mods, before: bool) -> (String, EmoteDef) {
        let def = EmoteDef {
            modifier: Some(Modifier { mods, before }),
            ..EmoteDef::new(name, "https://cdn.betterttv.net/emote/x/3x.webp")
        };
        (name.to_owned(), def)
    }

    fn providers() -> Providers {
        let mut providers = Providers::default();
        providers.seventv_channel.emotes =
            HashMap::from([emote("ICANT", false), emote("RATS", true)]);
        providers.bttv_global = HashMap::from([
            emote("ICANT", false),
            emote("catJAM", false),
            modifier("w!", Mods::WIDE, true),
            modifier("z!", Mods::ZERO_SPACE, true),
            modifier("h!", Mods::FLIP_X, true),
        ]);
        providers.ffz_global = HashMap::from([modifier("ffzX", Mods::FLIP_X, false)]);
        providers.cheermotes = vec![Cheermote {
            prefix: "cheer".into(),
            tiers: vec![
                CheerTier {
                    min_bits: 1,
                    color: "#979797".into(),
                    url: "https://d3aqoihi2n8ty8.cloudfront.net/1.gif".into(),
                },
                CheerTier {
                    min_bits: 100,
                    color: "#9c3ee8".into(),
                    url: "https://d3aqoihi2n8ty8.cloudfront.net/100.gif".into(),
                },
            ],
        }];
        providers
    }

    fn no_colors(_: &str) -> Option<RGBColor> {
        None
    }

    fn context(providers: &Providers) -> Context<'_> {
        Context {
            providers,
            user: None,
            room: None,
            mention_color: &no_colors,
        }
    }

    fn pieces_of(text: &str) -> Vec<Piece> {
        pieces(&message(text), &context(&providers()))
    }

    fn layer(name: &str) -> Layer {
        Layer {
            url: format!("https://cdn.7tv.app/emote/{name}/4x.webp"),
            name: name.into(),
        }
    }

    fn emote_piece(layers: &[&str], mods: Mods) -> Piece {
        Piece::Emote(EmotePiece {
            layers: layers.iter().map(|name| layer(name)).collect(),
            mods,
            giant: false,
        })
    }

    fn text(text: &str) -> Piece {
        Piece::Text(text.into())
    }

    #[test]
    fn words_and_emotes() {
        assert_eq!(
            pieces_of("hi ICANT  catJAM x"),
            [
                text("hi "),
                emote_piece(&["ICANT"], Mods::empty()),
                text("  "),
                emote_piece(&["catJAM"], Mods::empty()),
                text(" x"),
            ]
        );
    }

    #[test]
    fn zero_width() {
        assert_eq!(
            pieces_of("ICANT RATS RATS"),
            [emote_piece(&["ICANT", "RATS", "RATS"], Mods::empty())]
        );
        // without an emote before it, it is a normal emote
        assert_eq!(
            pieces_of("hi RATS"),
            [text("hi "), emote_piece(&["RATS"], Mods::empty())]
        );
    }

    #[test]
    fn bttv_modifiers() {
        assert_eq!(
            pieces_of("a w! h! ICANT b"),
            [
                text("a "),
                emote_piece(&["ICANT"], Mods::WIDE | Mods::FLIP_X),
                text(" b"),
            ]
        );
        // without an emote after them, they are text
        assert_eq!(pieces_of("w! h! hi"), [text("w! h! hi")]);
        assert_eq!(pieces_of("hi w!"), [text("hi w!")]);
        // zero space
        assert_eq!(
            pieces_of("ICANT z! ICANT"),
            [
                emote_piece(&["ICANT"], Mods::empty()),
                emote_piece(&["ICANT"], Mods::empty()),
            ]
        );
    }

    #[test]
    fn ffz_modifiers() {
        assert_eq!(
            pieces_of("ICANT ffzX b"),
            [emote_piece(&["ICANT"], Mods::FLIP_X), text(" b")]
        );
        // without an emote before it, it is not shown
        assert_eq!(pieces_of("a ffzX b"), [text("a b")]);
    }

    #[test]
    fn twitch_emotes_by_chars() {
        // "ü" is 2 bytes; the ranges are in characters
        let mut msg = message("ü w! Kappa RATS");
        msg.emotes = vec![Emote {
            id: "25".into(),
            char_range: 5..10,
            code: "Kappa".into(),
        }];
        let pieces = pieces(&msg, &context(&providers()));
        assert_eq!(
            pieces,
            [
                text("ü "),
                Piece::Emote(EmotePiece {
                    layers: vec![
                        Layer {
                            url: twitch_emote_url("25"),
                            name: "Kappa".into()
                        },
                        layer("RATS")
                    ],
                    // BTTV modifiers change Twitch emotes too
                    mods: Mods::WIDE,
                    giant: false,
                }),
            ]
        );
    }

    #[test]
    fn giant_emote() {
        let mut msg = message("ICANT catJAM hi");
        msg.giant_emote = true;
        let pieces = pieces(&msg, &context(&providers()));
        assert!(matches!(&pieces[0], Piece::Emote(e) if !e.giant));
        assert!(matches!(&pieces[2], Piece::Emote(e) if e.giant));
    }

    #[test]
    fn cheers_only_with_bits() {
        assert_eq!(pieces_of("Cheer100"), [text("Cheer100")]);
        let mut msg = message("Cheer150 nice");
        msg.bits = Some(150);
        let pieces = pieces(&msg, &context(&providers()));
        assert_eq!(
            pieces[0],
            Piece::Cheer {
                url: "https://d3aqoihi2n8ty8.cloudfront.net/100.gif".into(),
                bits: 150,
                color: "#9c3ee8".into(),
            }
        );
    }

    #[test]
    fn mentions_and_emoji() {
        let colors = |login: &str| (login == "necauqua").then_some(RGBColor { r: 255, g: 0, b: 0 });
        let providers = providers();
        let ctx = Context {
            mention_color: &colors,
            ..context(&providers)
        };
        let pieces = pieces(&message("@Necauqua, hi👋 @x @"), &ctx);
        assert_eq!(
            pieces,
            [
                Piece::Mention {
                    name: "Necauqua".into(),
                    color: Some("#FF0000".into())
                },
                text(", hi"),
                Piece::Emoji {
                    url: emoji::twemoji_url("👋"),
                    alt: "👋".into()
                },
                text(" "),
                Piece::Mention {
                    name: "x".into(),
                    color: None
                },
                text(" @"),
            ]
        );
    }

    #[test]
    fn personal_emotes() {
        let user = seventv::User {
            emotes: HashMap::from([emote("mine", false)]),
            ..Default::default()
        };
        let providers = providers();
        let ctx = Context {
            user: Some(&user),
            ..context(&providers)
        };
        let pieces = pieces(&message("mine"), &ctx);
        assert_eq!(pieces, [emote_piece(&["mine"], Mods::empty())]);
    }

    #[test]
    fn ffz_user_sets() {
        let mut providers = providers();
        providers
            .ffz_user_sets
            .sets
            .insert("7".into(), HashMap::from([emote("subOnly", false)]));
        providers
            .ffz_user_sets
            .users
            .insert("chatter".into(), vec!["7".into()]);
        let ctx = context(&providers);
        assert_eq!(
            pieces(&message("subOnly"), &ctx),
            [emote_piece(&["subOnly"], Mods::empty())]
        );
        let mut other = message("subOnly");
        other.login = "other".into();
        assert_eq!(pieces(&other, &ctx), [text("subOnly")]);
    }

    #[test]
    fn html() {
        let mut msg = message("<b> ICANT RATS");
        msg.is_action = true;
        msg.badges = vec![Badge {
            name: "moderator".into(),
            version: "1".into(),
        }];
        let mut providers = providers();
        providers.twitch_global_badges.insert(
            ("moderator".into(), "1".into()),
            BadgeImg {
                url: "https://static-cdn.jtvnw.net/badges/v1/x/3".into(),
                title: "Moderator".into(),
                background: None,
            },
        );
        let ctx = context(&providers);
        let html = render(&msg, &ctx, false).0;
        assert!(html.contains("&lt;b&gt;"), "{html}");
        assert!(html.contains(r#"class="action""#), "{html}");
        assert!(
            html.contains(
                r#"<img class="badge moderator-icon" src="/chat/img/static-cdn.jtvnw.net/badges/v1/x/3""#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<img class="zw" src="/chat/img/cdn.7tv.app/emote/RATS/4x.webp" alt="">"#
            ),
            "{html}"
        );
        assert!(
            render(&msg, &ctx, true)
                .0
                .contains(r#"class="action deleted""#)
        );
    }

    #[test]
    fn ffz_bot_badge_of_a_moderator() {
        let mut providers = providers();
        let bot = BadgeImg {
            url: "https://cdn.frankerfacez.com/badge/2/4".into(),
            title: "Bot".into(),
            background: Some("#595959".into()),
        };
        providers.ffz_badges.badges.insert(
            2,
            FfzBadge {
                img: bot,
                replaces: Some("moderator".into()),
            },
        );
        providers.ffz_badges.users.insert("1".into(), vec![2]);
        let ctx = context(&providers);

        let mut msg = message("beep");
        for name in ["moderator", "lead_moderator"] {
            msg.badges = vec![Badge {
                name: name.into(),
                version: "1".into(),
            }];
            let html = render(&msg, &ctx, false).0;
            assert_eq!(html.matches("<img class=\"badge").count(), 1, "{html}");
            assert!(
                html.contains(&format!(r#"class="badge {name}-icon ffz-icon""#)),
                "{html}"
            );
            assert!(html.contains("background-color: #00AD03"), "{html}");
        }
        // not a moderator: the gray badge after the others
        msg.badges = vec![];
        let html = render(&msg, &ctx, false).0;
        assert!(html.contains(r#"class="badge ffz-icon""#), "{html}");
        assert!(html.contains("background-color: #595959"), "{html}");
    }

    #[test]
    fn shared_chat_badges() {
        let mut providers = providers();
        let img = |url: &str| BadgeImg {
            url: url.into(),
            title: "Subscriber".into(),
            background: None,
        };
        let key = ("subscriber".to_owned(), "0".to_owned());
        providers
            .twitch_channel_badges
            .insert(key.clone(), img("https://static-cdn.jtvnw.net/ours"));
        let room = Room {
            name: "Other".into(),
            avatar: Some("https://static-cdn.jtvnw.net/avatar.png".into()),
            badges: HashMap::from([(key, img("https://static-cdn.jtvnw.net/theirs"))]),
        };
        let mut msg = message("hi");
        msg.badges = vec![Badge {
            name: "subscriber".into(),
            version: "0".into(),
        }];
        msg.source_room = Some("9".into());
        let ctx = Context {
            room: Some(&room),
            ..context(&providers)
        };
        let html = render(&msg, &ctx, false).0;
        assert!(html.contains("shared-chat-icon"), "{html}");
        assert!(html.contains("/theirs"), "{html}");
        assert!(!html.contains("/ours"), "{html}");
    }

    #[test]
    fn first_message() {
        let mut msg = message("hi");
        msg.first = true;
        msg.is_action = true;
        let html = render(&msg, &context(&providers()), true).0;
        assert!(html.contains(r#"class="action first deleted""#), "{html}");
    }

    #[test]
    fn replies_and_notices() {
        let mut msg = message("thanks");
        msg.reply = Some(Reply {
            login: "dargfox".into(),
            name: "dargfox".into(),
            text: "parent <i>".into(),
        });
        let html = render(&msg, &context(&providers()), false).0;
        assert!(html.contains(r#"<span class="above reply">"#), "{html}");
        assert!(html.contains("parent &lt;i&gt;"), "{html}");

        let mut msg = message("");
        msg.notice = Some(Notice {
            kind: "resub".into(),
            system: "Chatter subscribed.".into(),
            color: None,
        });
        let html = render(&msg, &context(&providers()), false).0;
        assert!(html.contains(r#"class="notice notice-resub""#), "{html}");
        assert!(
            html.contains(r#"<span class="message system">Chatter subscribed.</span>"#),
            "{html}"
        );
        assert!(!html.contains("above"), "{html}");

        let mut msg = message("kek");
        msg.notice = Some(Notice {
            kind: "announcement".into(),
            system: "".into(),
            color: Some("GREEN".into()),
        });
        let html = render(&msg, &context(&providers()), false).0;
        assert!(html.contains("--notice-color: #00DB84"), "{html}");
        assert!(
            html.contains(r#"<span class="above system">Announcement</span>"#),
            "{html}"
        );
    }

    #[test]
    fn localized_names() {
        assert_eq!(display_name("Necauqua", "necauqua"), "Necauqua");
        assert_eq!(display_name("ネカ", "neca_jp"), "ネカ (neca_jp)");
    }
}
