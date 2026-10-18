//! The chat messages and notices, from IRC or from the chat log.

use std::collections::HashMap;

use twitch_irc::message::{
    Badge, Emote, IRCMessage, IRCPrefix, PrivmsgMessage, RGBColor, ServerMessage,
};

/// The message that a reply is to
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub login: String,
    pub name: String,
    pub text: String,
}

/// A `USERNOTICE`: a sub, resub, gift, raid, announcement, watch streak etc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// The `msg-id`, e.g. `resub`, `announcement`, `viewermilestone`; for a
    /// notice from another channel in a shared chat, its own `msg-id`
    pub kind: String,
    /// The text from Twitch, e.g. "X subscribed at Tier 1."; empty for
    /// announcements
    pub system: String,
    /// The color of an announcement: `PRIMARY`, `BLUE`, `GREEN`, `ORANGE`,
    /// `PURPLE`
    pub color: Option<String>,
}

/// A chat message or a notice.
#[derive(Debug, Clone, Default)]
pub struct ChatMessage {
    pub id: String,
    pub user_id: String,
    pub login: String,
    pub name: String,
    pub color: Option<RGBColor>,
    pub badges: Vec<Badge>,
    /// Without the mention at the start of a reply; empty for a notice
    /// without a message
    pub text: String,
    /// Twitch emotes, by character (not byte) ranges of `text`
    pub emotes: Vec<Emote>,
    pub bits: Option<u64>,
    /// `/me`
    pub is_action: bool,
    /// The first message of the user in the channel
    pub first: bool,
    pub reply: Option<Reply>,
    pub notice: Option<Notice>,
    /// "Highlight My Message" (channel points)
    pub highlighted: bool,
    /// The Power-up that makes the last emote large
    pub giant_emote: bool,
    /// The Power-up message effect (`animation-id`), e.g. `simmer`
    pub effect: Option<String>,
    /// The channel id of a shared chat message from another channel
    pub source_room: Option<String>,
}

impl ChatMessage {
    /// A message without badges, color and Twitch emotes, with a new id.
    pub fn new(user_id: &str, login: &str, name: &str, text: &str) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            user_id: user_id.to_owned(),
            login: login.to_owned(),
            name: name.to_owned(),
            text: text.to_owned(),
            ..Default::default()
        }
    }

    /// A `USERNOTICE`. It is parsed as a `PRIVMSG` with the same tags, so
    /// that it does not need the `msg-param-*` tags of each kind (twitch-irc
    /// drops the notices with unusual ones).
    ///
    /// `None` for a bad message, and for the notices that Twitch does not
    /// show: the single gifts of a gift of many subs (there is a notice for
    /// all of them).
    pub fn from_notice(irc: &IRCMessage) -> Option<Self> {
        let tags = Tags(&irc.tags.0);
        let msg_id = tags.get("msg-id")?;
        let kind = match msg_id {
            "sharedchatnotice" => tags.get("source-msg-id").unwrap_or(msg_id),
            _ => msg_id,
        };
        if kind == "subgift" && tags.get("msg-param-community-gift-id").is_some() {
            return None;
        }
        let notice = Notice {
            kind: kind.to_owned(),
            system: tags.get("system-msg").unwrap_or_default().to_owned(),
            color: tags.get("msg-param-color").map(ToOwned::to_owned),
        };

        let mut as_privmsg = irc.clone();
        as_privmsg.command = "PRIVMSG".into();
        as_privmsg.prefix = Some(IRCPrefix::Full {
            nick: tags.get("login")?.to_owned(),
            user: None,
            host: None,
        });
        as_privmsg.params.resize(2, String::new());
        // the PRIVMSG meanings of these do not apply
        for tag in ["msg-id", "first-msg", "bits"] {
            as_privmsg.tags.0.remove(tag);
        }
        let ServerMessage::Privmsg(privmsg) = ServerMessage::try_from(as_privmsg)
            .inspect_err(|error| tracing::warn!(?error, "bad notice"))
            .ok()?
        else {
            return None;
        };
        Some(Self {
            notice: Some(notice),
            ..privmsg.into()
        })
    }
}

struct Tags<'a>(&'a HashMap<String, String>);

impl<'a> Tags<'a> {
    fn get(&self, key: &str) -> Option<&'a str> {
        self.0.get(key).map(|v| &**v).filter(|v| !v.is_empty())
    }

    /// The channel of a shared chat message from another channel
    fn source_room(&self, room: &str) -> Option<String> {
        self.get("source-room-id")
            .filter(|source| *source != room)
            .map(ToOwned::to_owned)
    }

    /// The badges in the source channel of a shared chat message
    fn badges(&self, source_room: &Option<String>) -> Option<Vec<Badge>> {
        source_room.as_ref()?;
        let badges = self.get("source-badges").unwrap_or_default();
        Some(
            badges
                .split(',')
                .filter_map(|badge| {
                    let (name, version) = badge.split_once('/')?;
                    Some(Badge {
                        name: name.into(),
                        version: version.into(),
                    })
                })
                .collect(),
        )
    }
}

/// Removes the mention of the parent user at the start of a reply (Twitch
/// adds it), and moves the emote ranges.
fn strip_reply_mention(text: &mut String, emotes: &mut Vec<Emote>, names: &[&str]) {
    let Some(rest) = text.strip_prefix('@') else {
        return;
    };
    let Some(name) = names.iter().find(|name| {
        rest.get(..name.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(name))
            && matches!(rest[name.len()..].chars().next(), None | Some(' '))
    }) else {
        return;
    };
    let mut stripped = &rest[name.len()..];
    stripped = stripped.strip_prefix(' ').unwrap_or(stripped);
    let removed = text.chars().count() - stripped.chars().count();
    *text = stripped.to_owned();
    emotes.retain_mut(|emote| {
        if emote.char_range.start < removed {
            return false;
        }
        emote.char_range = emote.char_range.start - removed..emote.char_range.end - removed;
        true
    });
}

impl From<PrivmsgMessage> for ChatMessage {
    fn from(msg: PrivmsgMessage) -> Self {
        let tags = Tags(&msg.source.tags.0);
        let msg_id = tags.get("msg-id");
        let first = tags.get("first-msg") == Some("1");
        let effect = (msg_id == Some("animated-message"))
            .then(|| tags.get("animation-id").unwrap_or("unknown").to_owned());
        let source_room = tags.source_room(&msg.channel_id);
        let badges = tags.badges(&source_room).unwrap_or(msg.badges);

        let mut text = msg.message_text;
        let mut emotes = msg.emotes;
        let reply = msg.reply_parent.map(|parent| {
            let user = parent.reply_parent_user;
            strip_reply_mention(&mut text, &mut emotes, &[&user.login, &user.name]);
            Reply {
                login: user.login,
                name: user.name,
                text: parent.message_text,
            }
        });

        Self {
            id: msg.message_id,
            user_id: msg.sender.id,
            login: msg.sender.login,
            name: msg.sender.name,
            color: msg.name_color,
            badges,
            text,
            emotes,
            bits: msg.bits,
            is_action: msg.is_action,
            first,
            reply,
            notice: None,
            highlighted: msg_id == Some("highlighted-message"),
            giant_emote: msg_id == Some("gigantified-emote-message"),
            effect,
            source_room,
        }
    }
}

#[cfg(test)]
mod tests {
    use twitch_irc::message::{IRCMessage, ServerMessage};

    use super::*;

    fn parse(raw: &str) -> ServerMessage {
        ServerMessage::try_from(IRCMessage::parse(raw).unwrap()).unwrap()
    }

    fn privmsg(tags: &str, text: &str) -> ChatMessage {
        let raw = format!(
            "@badge-info=;badges=vip/1;color=#FF69B4;display-name=Rosa;emotes=;id=m;room-id=1;\
             tmi-sent-ts=1;user-id=2;{tags} :rosa!rosa@rosa.tmi.twitch.tv PRIVMSG #chan :{text}"
        );
        let ServerMessage::Privmsg(msg) = parse(&raw) else {
            unreachable!()
        };
        msg.into()
    }

    #[test]
    fn reply() {
        let msg = privmsg(
            "emotes=555555584:9-10;reply-parent-display-name=dargfox;\
             reply-parent-msg-body=now\\syou\\sunderstand;reply-parent-msg-id=p;\
             reply-parent-user-id=3;reply-parent-user-login=dargfox",
            "@dargfox <3",
        );
        assert_eq!(msg.text, "<3");
        assert_eq!(msg.emotes[0].char_range, 0..2);
        assert_eq!(
            msg.reply,
            Some(Reply {
                login: "dargfox".into(),
                name: "dargfox".into(),
                text: "now you understand".into()
            })
        );
    }

    #[test]
    fn reply_without_the_mention() {
        let mut text = "@dargfoxes hi".to_owned();
        let mut emotes = vec![];
        strip_reply_mention(&mut text, &mut emotes, &["dargfox"]);
        assert_eq!(text, "@dargfoxes hi");
        let mut text = "@DargFox".to_owned();
        strip_reply_mention(&mut text, &mut emotes, &["dargfox"]);
        assert_eq!(text, "");
    }

    #[test]
    fn privmsg_tags() {
        let msg = privmsg("first-msg=1;msg-id=highlighted-message", "hi");
        assert!(msg.first && msg.highlighted && !msg.giant_emote);
        let msg = privmsg("msg-id=animated-message;animation-id=simmer", "hi");
        assert_eq!(msg.effect.as_deref(), Some("simmer"));
        let msg = privmsg("msg-id=gigantified-emote-message", "hi");
        assert!(msg.giant_emote);
    }

    #[test]
    fn shared_chat() {
        // from this channel: the normal badges
        let msg = privmsg("source-room-id=1;source-badges=founder/0", "hi");
        assert_eq!(msg.source_room, None);
        assert_eq!(msg.badges[0].name, "vip");
        // from another channel: its badges
        let msg = privmsg("source-room-id=9;source-badges=founder/0,bits/100", "hi");
        assert_eq!(msg.source_room.as_deref(), Some("9"));
        let badges = msg.badges.iter().map(|b| &*b.name).collect::<Vec<_>>();
        assert_eq!(badges, ["founder", "bits"]);
    }

    fn notice(tags: &str, text: Option<&str>) -> Option<ChatMessage> {
        let text = text.map(|t| format!(" :{t}")).unwrap_or_default();
        let raw = format!(
            "@badge-info=;badges=;color=;display-name=Rosa;emotes=;id=n;login=rosa;room-id=1;\
             tmi-sent-ts=1;user-id=2;{tags} :tmi.twitch.tv USERNOTICE #chan{text}"
        );
        ChatMessage::from_notice(&IRCMessage::parse(&raw).unwrap())
    }

    #[test]
    fn notices() {
        let msg = notice(
            "msg-id=resub;msg-param-cumulative-months=14;msg-param-months=0;\
             msg-param-should-share-streak=0;msg-param-sub-plan=1000;\
             msg-param-sub-plan-name=Sub;system-msg=Rosa\\ssubscribed.",
            Some("hi chat"),
        )
        .unwrap();
        let resub = msg.notice.unwrap();
        assert_eq!(resub.kind, "resub");
        assert_eq!(resub.system, "Rosa subscribed.");
        assert_eq!(msg.text, "hi chat");

        let msg = notice(
            "msg-id=announcement;msg-param-color=GREEN;system-msg=",
            Some("kek"),
        )
        .unwrap();
        assert_eq!(msg.notice.unwrap().color.as_deref(), Some("GREEN"));

        // the single gifts of a gift of many subs
        let gift = "msg-id=subgift;msg-param-months=1;msg-param-recipient-display-name=A;\
                    msg-param-recipient-id=5;msg-param-recipient-user-name=a;\
                    msg-param-sub-plan=1000;msg-param-sub-plan-name=Sub;\
                    msg-param-gift-months=1;system-msg=Rosa\\sgifted\\sa\\ssub";
        assert!(notice(gift, None).is_some());
        assert!(notice(&format!("{gift};msg-param-community-gift-id=7"), None).is_none());

        // without the tags of its kind, and of an unknown kind
        let msg = notice("msg-id=submysterygift;system-msg=Rosa\\sgifts", None).unwrap();
        assert_eq!(msg.notice.unwrap().system, "Rosa gifts");
        assert_eq!(msg.text, "");
        assert!(notice("msg-id=somethingnew;system-msg=new", Some("hi")).is_some());
    }

    #[test]
    fn archived_notice() {
        let doc = serde_json::json!({
            "@timestamp": 1i64,
            "channel": "necauqua",
            "name": "mocro07",
            "message": "mocro07 subscribed at Tier 1. They've subscribed for 14 months!",
            "tags": {
                "room-id": "160410682",
                "user-id": "137847272",
                "login": "mocro07",
                "system-msg": "mocro07 subscribed at Tier 1. They've subscribed for 14 months!",
                "msg-id": "resub",
                "badges": { "founder": 0 },
            },
            "irc.nick": "tmi.twitch.tv",
            "irc.cmd": "USERNOTICE",
        });
        let irc = crate::services::chat_log::irc_from_doc("n1", &doc).unwrap();
        let msg = ChatMessage::from_notice(&irc).unwrap();
        assert_eq!(msg.name, "mocro07");
        assert_eq!(msg.text, "");
        assert_eq!(msg.badges[0].name, "founder");
        assert_eq!(msg.notice.unwrap().kind, "resub");
    }
}
