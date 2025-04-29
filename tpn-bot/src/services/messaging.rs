use std::time::Duration;

use anyhow::Result;
use strum::{EnumIter, EnumMessage, IntoStaticStr};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, BufReader},
    sync::mpsc::UnboundedReceiver,
    time::sleep,
};
use twitch_irc::{
    ClientConfig, SecureTCPTransport, TwitchIRCClient,
    message::{Badge, ServerMessage},
};

use super::twitch::Twitch;

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

pub fn connect_to_twitch(twitch: Twitch) -> (MessageSource, MessagingClient) {
    let channel = twitch.target().login.to_string();
    let (incoming, client) = TwitchIRCClient::new(ClientConfig::new_simple(twitch));
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
        } else if badges.iter().any(|b| "moderator".contains(&*b.name)) {
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

pub enum MessagingClient {
    Twitch {
        client: TwitchIRCClient<SecureTCPTransport, Twitch>,
        channel: String,
    },
    Mock,
}

impl MessagingClient {
    pub async fn send(&self, message: impl Into<String>) -> Result<()> {
        let text = message.into();

        match self {
            MessagingClient::Twitch {
                client, channel, ..
            } => {
                send_chunked(text, |chunk| async {
                    Ok(client.say(channel.to_string(), chunk).await?)
                })
                .await?
            }
            MessagingClient::Mock => tracing::info!(text, "mock send"),
        }
        Ok(())
    }

    pub async fn reply(&self, message: &Message, text: impl Into<String>) -> Result<()> {
        let text = text.into();
        match self {
            MessagingClient::Twitch {
                client, channel, ..
            } => {
                send_chunked(text, |chunk| async {
                    Ok(client
                        .say_in_reply_to(&(channel, &message.id), chunk)
                        .await?)
                })
                .await?
            }
            MessagingClient::Mock => tracing::info!(message = text, "mock reply"),
        }
        Ok(())
    }
}

async fn send_chunked<R>(text: String, mut the_send: impl FnMut(String) -> R) -> Result<()>
where
    R: Future<Output = Result<()>>,
{
    let chunks = chunk_text(text, 420);
    let len = chunks.len();
    for (i, chunk) in chunks.into_iter().enumerate() {
        let chunk = if len != 1 {
            format!("{chunk} ({}/{len})", i + 1)
        } else {
            chunk
        };
        the_send(chunk).await?;
        if i != 0 && i != len - 1 {
            sleep(Duration::from_millis(300)).await;
        }
    }
    Ok(())
}

fn chunk_text(text: String, max_length: usize) -> Vec<String> {
    if text.len() < max_length {
        return vec![text];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        let delimiter = if current.is_empty() { "" } else { " " };
        let next_length = current.len() + delimiter.len() + word.len();

        if next_length > max_length && !current.is_empty() {
            chunks.push(current);
            current = word.to_string();
        } else {
            current.push_str(delimiter);
            current.push_str(word);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}
