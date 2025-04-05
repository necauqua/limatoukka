use anyhow::Result;
use strum::{EnumIter, EnumMessage, IntoStaticStr};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, BufReader},
    sync::mpsc::UnboundedReceiver,
};
use twitch_irc::{
    ClientConfig, SecureTCPTransport, TwitchIRCClient,
    login::StaticLoginCredentials,
    message::{Badge, ServerMessage},
};

#[derive(Debug, Clone)]
pub struct Sender {
    pub id: String,
    pub login: String,
    pub name: String,
    pub level: PermissionLevel,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub id: String,
    pub sender: Sender,
    pub text: String,
}

pub fn connect_to_twitch(
    creds: StaticLoginCredentials,
    channel: String,
) -> (MessageSource, MessagingClient) {
    let (incoming, client) = TwitchIRCClient::new(ClientConfig::new_simple(creds));
    client.join(channel.clone()).unwrap(); // panic on invalid channel
    (
        MessageSource::Twitch(incoming),
        MessagingClient::Twitch { client, channel },
    )
}

pub async fn connect_to_mock() -> Result<(MessageSource, MessagingClient)> {
    let input = BufReader::new(File::open("/tmp/tpn-bot.fifo").await?);
    let sender = Sender {
        id: "mock".into(),
        login: "mock".into(),
        name: "mock".into(),
        level: PermissionLevel::Caster,
    };
    Ok((MessageSource::Mock(sender, input, 0), MessagingClient::Mock))
}

/// Currently used for not having to deal with twitch when testing, but we
/// could add something like Discord here in the future
pub enum MessageSource {
    Twitch(UnboundedReceiver<ServerMessage>),
    Mock(Sender, BufReader<File>, u64),
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, EnumIter, EnumMessage, IntoStaticStr,
)]
#[repr(u8)]
pub enum PermissionLevel {
    /// Only the broadcaster can use this command.
    Caster = 7,
    /// Channel moderators and above can use this command.
    Moderator = 6,
    /// Twitch administrators and above can use this command.
    TwitchAdmin = 5,
    /// Twitch staff and above can use this command.
    TwitchStaff = 4,
    /// Channel VIPs and above can use this command.
    Vip = 3,
    /// Verified users (partners) and above can use this command.
    Verified = 2,
    /// Channel subscribers and above can use this command.
    Subscriber = 1,
    /// Anyone can use this command.
    Viewer = 0,
}

impl PermissionLevel {
    pub fn from_badges(badges: &[Badge]) -> Self {
        if badges.iter().any(|b| b.name == "broadcaster") {
            PermissionLevel::Caster
        } else if badges.iter().any(|b| "moderator".contains(&&*b.name)) {
            PermissionLevel::Moderator
        } else if badges.iter().any(|b| b.name == "admin") {
            PermissionLevel::TwitchAdmin
        } else if badges.iter().any(|b| b.name == "staff") {
            PermissionLevel::TwitchStaff
        } else if badges.iter().any(|b| b.name == "vip") {
            PermissionLevel::Vip
        } else if badges.iter().any(|b| b.name == "partner") {
            PermissionLevel::Verified
        } else if badges.iter().any(|b| b.name == "subscriber") {
            PermissionLevel::Subscriber
        } else {
            PermissionLevel::Viewer
        }
    }
}

impl MessageSource {
    pub async fn recv(&mut self) -> Option<Message> {
        Some(match self {
            MessageSource::Twitch(incoming) => loop {
                let incoming = incoming.recv().await?;
                let ServerMessage::Privmsg(msg) = incoming else {
                    continue;
                };

                break Message {
                    id: msg.message_id,
                    sender: Sender {
                        id: msg.sender.id,
                        login: msg.sender.login,
                        name: msg.sender.name,
                        level: PermissionLevel::from_badges(&msg.badges),
                    },
                    text: msg.message_text,
                };
            },
            MessageSource::Mock(sender, fifo, count) => {
                let mut text = String::new();
                if fifo.read_line(&mut text).await.ok()? == 0 {
                    return None;
                }
                text.pop(); // remove newline
                *count += 1;
                Message {
                    id: format!("mock-{}", count),
                    sender: Sender {
                        // meh
                        id: format!("{}-{}", sender.id, count),
                        login: format!("{}-{}", sender.login, count),
                        name: format!("{}-{}", sender.name, count),
                        level: PermissionLevel::Caster,
                    },
                    text,
                }
            }
        })
    }
}

#[derive(Clone)]
pub enum MessagingClient {
    Twitch {
        client: TwitchIRCClient<SecureTCPTransport, StaticLoginCredentials>,
        channel: String,
    },
    Mock,
}

impl MessagingClient {
    pub async fn send(&self, message: String) -> Result<()> {
        Ok(match self {
            MessagingClient::Twitch {
                client, channel, ..
            } => client.say(channel.to_owned(), message.to_owned()).await?,
            MessagingClient::Mock => tracing::info!(message, "mock send"),
        })
    }

    pub async fn reply(&self, message_id: &str, message: String) -> Result<()> {
        Ok(match self {
            MessagingClient::Twitch {
                client, channel, ..
            } => {
                client
                    .say_in_reply_to(&(channel, message_id), message)
                    .await?
            }
            MessagingClient::Mock => tracing::info!(message, "mock reply"),
        })
    }
}
