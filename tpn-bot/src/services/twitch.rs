use std::{
    collections::HashMap,
    fmt::Debug,
    option::Option::Some,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use axum::{Router, extract::Query, routing::get};
use futures::StreamExt;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{
        Notify, RwLock,
        broadcast::{Receiver, Sender},
        oneshot,
    },
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite};
use twitch_api::{
    HelixClient,
    eventsub::{self, Event, EventsubWebsocketData, ReconnectPayload, SessionData, WelcomePayload},
    helix::{
        ClientRequestError, HelixRequestDeleteError, HelixRequestGetError, HelixRequestPatchError,
        HelixRequestPostError, HelixRequestPutError, Scope, users::User,
    },
    twitch_oauth2::{
        AccessToken, ClientId, ClientSecret, RefreshToken, TwitchToken, UserToken,
        UserTokenBuilder, url::Url,
    },
};
use twitch_irc::login::{CredentialsPair, LoginCredentials};

#[derive(Deserialize)]
pub struct TwitchApp {
    pub client_id: ClientId,
    pub client_secret: ClientSecret,
    pub redirect_url: String,
    pub target_channel: String,
}

#[derive(Serialize, Deserialize)]
struct TokenPair {
    at: AccessToken,
    rt: RefreshToken,
}

impl From<&UserToken> for TokenPair {
    fn from(value: &UserToken) -> Self {
        Self {
            at: value.access_token.as_str().into(),
            rt: value
                .refresh_token
                .as_deref()
                .expect("user token had no refresh token")
                .into(),
        }
    }
}

async fn read_token(config: &TwitchApp, client: &TwitchClient, variant: &str) -> Result<UserToken> {
    let entry = keyring::Entry::new("limatoukka-the-twitch-bot", variant)?;
    tracing::info!("reading {variant} token");
    let token = match entry.get_password() {
        Ok(p) => {
            let data: TokenPair = serde_json::from_str(&p)?;
            UserToken::from_existing_or_refresh_token(
                client.get_client(),
                data.at,
                data.rt,
                config.client_id.clone(),
                Some(config.client_secret.clone()),
            )
            .await?
        }
        Err(keyring::Error::NoEntry) => full_auth(config, client, &entry, variant).await?,
        Err(e) => bail!(e),
    };
    Ok(token)
}

type TwitchClient = twitch_api::TwitchClient<'static, reqwest::Client>;

struct Inner {
    client: TwitchClient,
    token: RwLock<UserToken>,
    pub target: User,
    pub bot: String,
}

pub struct TwitchRefs<'a> {
    pub helix: &'a HelixClient<'static, reqwest::Client>,
    pub target: &'a User,
    pub token: UserToken,
}

#[derive(Clone)]
pub struct Twitch {
    inner: Arc<Inner>,
}

impl Debug for Twitch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Twitch")
            .field("bot", &self.inner.bot)
            .field("target", &self.inner.target.login)
            .finish()
    }
}

#[async_trait]
impl LoginCredentials for Twitch {
    type Error = anyhow::Error;

    async fn get_credentials(&self) -> Result<CredentialsPair, Self::Error> {
        let mut token = self.inner.token.write().await;
        // idk about the time
        if token.expires_in() < Duration::from_secs(1800) {
            tracing::info!("(irc) token close to expiration, refreshing");
            token
                .refresh_token(self.inner.client.helix.get_client())
                .await?;
        }
        Ok(CredentialsPair {
            login: self.inner.bot.to_owned(),
            token: Some(token.token().clone().take()),
        })
    }
}

impl Twitch {
    pub fn bot(&self) -> &str {
        &self.inner.bot
    }

    pub fn target(&self) -> &User {
        &self.inner.target
    }

    pub async fn new(config: &TwitchApp) -> Result<(Self, TwitchEventSub)> {
        let client = TwitchClient::new();

        let token = read_token(config, &client, "bot").await?;
        let caster_token = read_token(config, &client, "caster").await?;

        let target = client
            .helix
            .get_user_from_login(&config.target_channel, &token)
            .await?
            .with_context(|| format!("twitch user {} not found", config.target_channel))?;

        tracing::info!("{} is {}", target.display_name, target.id);

        let t = Self {
            inner: Arc::new(Inner {
                client,
                bot: token.login.as_str().into(),
                token: token.into(),
                target,
            }),
        };
        let (tx, _) = tokio::sync::broadcast::channel(1);

        let eventsub = TwitchEventSub {
            twitch: t.clone(),
            connect_url: twitch_api::TWITCH_EVENTSUB_WEBSOCKET_URL.clone(),
            caster_token,
            first_welcome: true,
            tx,
        };

        Ok((t, eventsub))
    }

    // the twitch-api crate is pretty awful, so we have to do things like this,
    // but eh its way better than not having it, at least we got payload types
    pub async fn call<'a, F, R, T>(&'a self, mut f: F) -> Result<T>
    where
        R: Future<Output = Result<T, ClientRequestError<reqwest::Error>>>,
        F: FnMut(TwitchRefs<'a>) -> R,
    {
        let res = f(TwitchRefs {
            helix: &self.inner.client.helix,
            target: &self.inner.target,
            // clone the token for every request because twitch-api lifetimes are shit
            //  (also that's what causes the turbo-annoying async move { api.call().await } constructs too)
            token: self.inner.token.read().await.clone(),
        })
        .await;

        let Err(e) = &res else {
            return Ok(res?);
        };
        if !is_auth_error(e) {
            return Ok(res?);
        }

        tracing::info!(error = ?e, "token expired, refreshing");
        let mut token = self.inner.token.write().await;
        token.refresh_token(self.inner.client.get_client()).await?;
        let t = token.clone();
        drop(token);

        Ok(f(TwitchRefs {
            helix: &self.inner.client.helix,
            target: &self.inner.target,
            token: t,
        })
        .await?)
    }
}

pub struct TwitchEventSub {
    twitch: Twitch,
    connect_url: Url,
    caster_token: UserToken,
    first_welcome: bool,
    tx: Sender<Event>,
}

impl TwitchEventSub {
    async fn process_welcome_message(&mut self, data: SessionData<'_>) -> Result<()> {
        if let Some(url) = data.reconnect_url {
            self.connect_url = url.parse()?;
        }

        if self.first_welcome {
            self.first_welcome = false;
        } else {
            self.caster_token
                .validate_token(self.twitch.inner.client.get_client())
                .await?;
        }

        let helix = &self.twitch.inner.client.helix;

        let transport = eventsub::Transport::websocket(data.id.clone());
        macro_rules! subscribe {
            ($first:ident $(:: $s:ident)* $method:ident) => {
                helix
                    .create_eventsub_subscription(
                        eventsub::$first$(::$s)*::$method(
                            self.caster_token.user_id.clone(),
                        ),
                        transport.clone(),
                        &self.caster_token,
                    )
                    .await?;
            };
            ($first:ident $(:: $s:ident)*) => {
                subscribe!($first $(:: $s)* broadcaster_user_id);
            };
            ($($first:ident $(:: $s:ident)* $($method:ident)?),* $(,)?) => {
                $(
                    subscribe!($first $(:: $s)* $($method)?);
                )*
            };
        }

        subscribe![
            channel::ChannelAdBreakBeginV1,
            channel::ChannelPointsCustomRewardRedemptionAddV1,
            channel::ChannelSubscribeV1,
            channel::ChannelSubscriptionGiftV1,
            channel::ChannelSubscriptionMessageV1,
            channel::ChannelCheerV1,
            channel::ChannelRaidV1 to_broadcaster_user_id,
            channel::ChannelHypeTrainBeginV1,
            channel::ChannelHypeTrainEndV1,
        ];

        Ok(())
    }

    pub fn subscribe(&self) -> Receiver<Event> {
        self.tx.subscribe()
    }

    pub async fn run(mut self) -> ! {
        loop {
            if let Err(e) = self.run_iteration().await {
                tracing::warn!(error=?e, "twitch eventsub fail");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn run_iteration(&mut self) -> Result<()> {
        let mut s = self.connect().await?;
        while let Some(msg) = s.next().await {
            if !self.process_message(msg?).await? {
                s = self.connect().await?;
            }
        }
        Ok(())
    }

    async fn connect(&self) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        tracing::info!("connecting to twitch");
        let (stream, _) = tokio_tungstenite::connect_async_with_config(
            &self.connect_url,
            Some(
                tungstenite::protocol::WebSocketConfig::default()
                    .max_message_size(Some(64 << 20)) // 64 MiB
                    .max_frame_size(Some(16 << 20)) // 16 MiB
                    .accept_unmasked_frames(false),
            ),
            false,
        )
        .await?;
        Ok(stream)
    }

    /// Should reconnect if Ok(false) is returned
    async fn process_message(&mut self, msg: tungstenite::Message) -> Result<bool> {
        match msg {
            tungstenite::Message::Text(s) => {
                tracing::debug!("{s}");
                match Event::parse_websocket(&s)? {
                    EventsubWebsocketData::Welcome {
                        payload: WelcomePayload { session },
                        ..
                    } => {
                        self.process_welcome_message(session).await?;
                        Ok(true)
                    }
                    EventsubWebsocketData::Reconnect {
                        payload: ReconnectPayload { session },
                        ..
                    } => {
                        if let Some(url) = session.reconnect_url {
                            self.connect_url = url.parse()?;
                        }
                        Ok(false)
                    }
                    EventsubWebsocketData::Notification {
                        metadata: _,
                        payload,
                    } => {
                        _ = self.tx.send(payload);
                        Ok(true)
                    }
                    EventsubWebsocketData::Revocation {
                        metadata,
                        payload: _,
                    } => {
                        tracing::warn!(?metadata, "got revocation event");
                        Ok(false)
                    }
                    _ => Ok(true),
                }
            }
            tungstenite::Message::Close(frame) => {
                tracing::warn!(?frame, "received a close frame");
                Ok(false)
            }
            _ => Ok(true),
        }
    }
}

async fn full_auth(
    config: &TwitchApp,
    client: &TwitchClient,
    entry: &keyring::Entry,
    variant: &str,
) -> Result<UserToken> {
    let url: Url = config.redirect_url.parse()?;

    let port = url.port().context("redirect_url had no port")?;

    let shutdown = Arc::new(Notify::new());
    let (tx, rx) = oneshot::channel();
    let tx = Arc::new(Mutex::new(Some(tx)));

    let app = Router::new().route(
        "/",
        get({
            let shutdown = Arc::clone(&shutdown);
            move |Query(mut params): Query<HashMap<String, String>>| async move {
                shutdown.notify_waiters();

                let tx = tx.lock().unwrap().take().unwrap();

                if let Some((state, code)) = params.remove("state").zip(params.remove("code")) {
                    tx.send(anyhow::Ok((state, code))).unwrap();
                    "Success!".into()
                } else if let Some((error, error_description)) = params
                    .remove("error")
                    .zip(params.remove("error_description"))
                {
                    tx.send(Err(anyhow!("twitch error: {error} - {error_description}")))
                        .unwrap();
                    format!("Twitch error: {error} - {error_description}!")
                } else {
                    tx.send(Err(anyhow!("Invalid URL"))).unwrap();
                    "Invalid URL".into()
                }
            }
        }),
    );

    let s = tokio::spawn(async move {
        let listener = TcpListener::bind(("localhost", port)).await?;
        tracing::info!("listening on {}", listener.local_addr()?);
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.notified().await })
            .await?;
        anyhow::Ok(rx.await??)
    });

    let mut builder = UserTokenBuilder::new(&*config.client_id, &*config.client_secret, url)
        .set_scopes(Scope::all());

    let (url, _) = builder.generate_url();

    println!("\nFull {variant} auth needed, go to: {url}\n");

    let (state, code) = s.await??;
    let token = builder.get_user_token(client, &state, &code).await?;
    entry.set_password(&serde_json::to_string(&TokenPair::from(&token))?)?;
    Ok(token)
}

fn is_auth_error(error: &ClientRequestError<reqwest::Error>) -> bool {
    // 🤦🤦🤦🤦🤦 separate error types for every HTTP method ICANT
    matches!(
        error,
        ClientRequestError::HelixRequestGetError(HelixRequestGetError::Error {
            status: StatusCode::UNAUTHORIZED,
            ..
        }) | ClientRequestError::HelixRequestPostError(HelixRequestPostError::Error {
            status: StatusCode::UNAUTHORIZED,
            ..
        }) | ClientRequestError::HelixRequestPutError(HelixRequestPutError::Error {
            status: StatusCode::UNAUTHORIZED,
            ..
        }) | ClientRequestError::HelixRequestPatchError(HelixRequestPatchError::Error {
            status: StatusCode::UNAUTHORIZED,
            ..
        }) | ClientRequestError::HelixRequestDeleteError(HelixRequestDeleteError::Error {
            status: StatusCode::UNAUTHORIZED,
            ..
        })
    )
}
