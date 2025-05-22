use std::{
    collections::HashMap,
    fmt::Debug,
    ops::{Deref, DerefMut},
    option::Option::Some,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use axum::{Router, extract::Query, routing::get};
use futures::StreamExt;
use reqwest::StatusCode;
use rustis::commands::{SetCondition, SetExpiration, StringCommands};
use serde::{Deserialize, Serialize};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{
        Notify, RwLock,
        broadcast::{Receiver, Sender},
        oneshot,
    },
    task::JoinHandle,
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite};
use tracing::instrument;
use twitch_api::{
    HelixClient, TWITCH_EVENTSUB_WEBSOCKET_URL,
    eventsub::{self, Event, EventsubWebsocketData, ReconnectPayload, SessionData, WelcomePayload},
    helix::{
        ClientRequestError, HelixRequestDeleteError, HelixRequestGetError, HelixRequestPatchError,
        HelixRequestPostError, HelixRequestPutError, Scope,
    },
    twitch_oauth2::{
        AccessToken, RefreshToken, TwitchToken as _, UserToken, UserTokenBuilder, url::Url,
    },
};
use twitch_irc::login::{CredentialsPair, LoginCredentials};

use crate::{config::Config, context::app::AppContext};

type TwitchClient = twitch_api::TwitchClient<'static, reqwest::Client>;

struct Inner {
    client: TwitchClient,
    bot_id: String,
    caster_id: String,
    caster_login: String,
    bot_token: TwitchToken,
    caster_token: TwitchToken,
}

#[derive(Clone)]
pub struct Twitch {
    inner: Arc<Inner>,
}

pub struct TwitchToken {
    kind: &'static str,
    token: RwLock<UserToken>,
}

impl TwitchToken {
    pub fn new(kind: &'static str, token: UserToken) -> Self {
        Self {
            kind,
            token: token.into(),
        }
    }

    // the twitch-api crate is pretty awful, so we have to do things like this,
    // but eh its way better than not having it, at least we got payload types
    //
    // .. ok also this allows us to trace all twitch calls I guess lmao
    #[instrument(name = "twitch-api-call", skip_all)]
    async fn call<'a, F, R, T>(
        &'a self,
        helix: &'a HelixClient<'static, reqwest::Client>,
        caster_id: &'a str,
        mut f: F,
    ) -> Result<T>
    where
        F: FnMut(TwitchRefs<'a>) -> R,
        R: Future<Output = Result<T, ClientRequestError<reqwest::Error>>>,
    {
        // clone the whole token because twitch-api async functions
        // unnecessarily capture arg lifetime, causing the references to not
        // work :(
        //  cant event blame twitch-api being bad, it's Rust auto-capture being
        //  too broad and the use<> thing being real new (and still annoying)
        //  although they could've implemented TwitchToken like for Arc, who
        //  needs a Box impl lmao
        let res = f(TwitchRefs {
            helix,
            caster_id,
            token: self.token.read().await.clone(),
        })
        .await;

        let Err(e) = &res else {
            return Ok(res?);
        };
        if !is_auth_error(e) {
            return Ok(res?);
        }

        tracing::info!("{} token expired, refreshing", self.kind);
        let mut token = self.token.write().await;
        token.refresh_token(helix.get_client()).await?;
        Ok(f(TwitchRefs {
            helix,
            caster_id,
            token: token.clone(),
        })
        .await?)
    }
}

impl Debug for Twitch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Twitch")
    }
}

#[async_trait]
impl LoginCredentials for Twitch {
    type Error = anyhow::Error;

    async fn get_credentials(&self) -> Result<CredentialsPair, Self::Error> {
        let mut token = self.inner.bot_token.token.write().await;
        // idk about the time
        if token.expires_in() < Duration::from_secs(1800) {
            tracing::info!("(irc) token close to expiration, refreshing");
            token
                .refresh_token(self.inner.client.helix.get_client())
                .await?;
        }
        Ok(CredentialsPair {
            login: token.login.clone().take(),
            token: Some(token.token().clone().take()),
        })
    }
}

pub struct TwitchRefs<'a> {
    pub helix: &'a HelixClient<'static, reqwest::Client>,
    pub caster_id: &'a str,
    pub token: UserToken,
}

impl Twitch {
    pub fn bot_id(&self) -> &str {
        &self.inner.bot_id
    }

    pub fn caster_login(&self) -> &str {
        &self.inner.caster_login
    }

    pub async fn new(config: &Config) -> Result<Self> {
        let client = TwitchClient::new();

        let bot_token = read_token(config, &client, "bot").await?;
        let caster_token = read_token(config, &client, "caster").await?;

        tracing::info!("caster is {}({})", caster_token.login, caster_token.user_id);

        Ok(Self {
            inner: Arc::new(Inner {
                client,
                bot_id: bot_token.user_id.clone().take(),
                caster_id: caster_token.user_id.clone().take(),
                caster_login: caster_token.login.clone().take(),
                bot_token: TwitchToken::new("bot", bot_token),
                caster_token: TwitchToken::new("caster", caster_token),
            }),
        })
    }

    // the twitch-api crate is pretty awful, so we have to do things like this,
    // but eh its way better than not having it, at least we got payload types
    //
    // .. ok also this allows us to trace all twitch calls I guess lmao
    #[instrument(name = "calling Twitch API (bot)", level = "debug", skip_all)]
    pub async fn call<'a, F, R, T>(&'a self, f: F) -> Result<T>
    where
        R: Future<Output = Result<T, ClientRequestError<reqwest::Error>>> + 'a,
        F: FnMut(TwitchRefs<'a>) -> R,
    {
        self.inner
            .bot_token
            .call(&self.inner.client.helix, &self.inner.caster_id, f)
            .await
    }

    #[instrument(name = "calling Twitch API (caster)", level = "debug", skip_all)]
    pub async fn caster_call<'a, F, R, T>(&'a self, f: F) -> Result<T>
    where
        R: Future<Output = Result<T, ClientRequestError<reqwest::Error>>> + 'a,
        F: FnMut(TwitchRefs<'a>) -> R,
    {
        self.inner
            .caster_token
            .call(&self.inner.client.helix, &self.inner.caster_id, f)
            .await
    }
}

pub struct EventSub {
    twitch: Twitch,
    tx: Sender<Event>,
    prev: Option<WebSocket>,
    on_subscribed: Option<Arc<Notify>>,
    canary: Option<JoinHandle<()>>,
}

impl EventSub {
    pub fn new(twitch: Twitch) -> Self {
        Self {
            twitch,
            tx: Sender::new(16),
            prev: None,
            on_subscribed: Some(Default::default()),
            canary: None,
        }
    }

    async fn process_welcome_message(&mut self, data: SessionData<'_>) -> Result<()> {
        tracing::info!("got a welcome message");

        if let Some(mut prev) = self.prev.take() {
            prev.close(None).await?;
            return Ok(());
        }

        let transport = eventsub::Transport::websocket(data.id.clone());

        macro_rules! subscribe {
            ($group:ident::$event:ident $method:ident) => {
                self.twitch.caster_call(|t| {
                    let transport = transport.clone();
                    async move {
                        t.helix.create_eventsub_subscription(
                            eventsub::$group::$event::$method(t.caster_id),
                            transport,
                            &t.token,
                        ).await
                    }
                })
                .await?;

                tracing::info!("subscribing to {}", stringify!($event));
            };
            ($group:ident::$event:ident) => {
                subscribe!($group::$event broadcaster_user_id);
            };
            ($($group:ident::$event:ident $($method:ident)?),* $(,)?) => {
                $(
                    subscribe!($group::$event $($method)?);
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

        if let Some(on_subscribed) = self.on_subscribed.take() {
            on_subscribed.notify_waiters();
        }

        Ok(())
    }

    async fn keep_alive(&mut self) -> Result<()> {
        if let Some(canary) = self.canary.take() {
            canary.abort();
        }
        self.canary = Some(tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(20)).await;
            tracing::error!("received no keepalive");
        }));
        Ok(())
    }

    pub fn wait_for_full_init(&self) -> impl Future<Output = ()> + use<> {
        let notif = self.on_subscribed.clone();
        async {
            if let Some(notif) = notif {
                notif.notified().await;
            }
        }
    }

    pub fn subscribe(&self) -> Receiver<Event> {
        self.tx.subscribe()
    }

    pub async fn run(mut self, ctx: AppContext) -> ! {
        loop {
            if let Err(e) = self.run_iteration(&ctx).await {
                tracing::warn!(error=?e, "twitch eventsub fail");
            }
            self.prev = None;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn run_iteration(&mut self, ctx: &AppContext) -> Result<()> {
        let mut s = websocket_connect(&TWITCH_EVENTSUB_WEBSOCKET_URL).await?;

        loop {
            tokio::select! {
                Some(msg) = async {
                    match &mut self.prev {
                        Some(prev) => prev.next().await,
                        None => None,
                    }
                } => match msg {
                    Ok(msg) => {
                        self.process_message(msg, ctx).await?;
                    },
                    Err(e) => tracing::warn!(error=?e, "old websocket error (probably closed)"),
                },
                Some(msg) = s.next() => {
                    if let MessageResult::Reconnect { url } = self.process_message(msg?, ctx).await? {
                        self.prev = Some(std::mem::replace(&mut s, websocket_connect(&url).await?));
                    }
                }
                else => bail!("websocket closed without reconnect event"),
            }
        }
    }

    async fn process_message(
        &mut self,
        msg: tungstenite::Message,
        ctx: &AppContext,
    ) -> Result<MessageResult> {
        match msg {
            tungstenite::Message::Text(s) => {
                match Event::parse_websocket(&s)? {
                    EventsubWebsocketData::Welcome {
                        payload: WelcomePayload { session },
                        ..
                    } => self.process_welcome_message(session).await?,
                    EventsubWebsocketData::Reconnect {
                        payload: ReconnectPayload { session },
                        ..
                    } => {
                        let url = session
                            .reconnect_url
                            .as_deref()
                            .and_then(|url| url.parse().ok())
                            .unwrap_or_else(|| TWITCH_EVENTSUB_WEBSOCKET_URL.clone());
                        tracing::info!(%url, "reconnect event");
                        return Ok(MessageResult::Reconnect { url });
                    }
                    EventsubWebsocketData::Notification { metadata, payload } => {
                        let new = ctx
                            .storage()
                            .set_with_options(
                                format!("seen:eventsub:{}", metadata.message_id),
                                "1",
                                SetCondition::NX,
                                SetExpiration::Ex(600),
                                false,
                            )
                            .await?;
                        if new {
                            _ = self.tx.send(payload);
                        } else {
                            tracing::info!(
                                "received duplicate eventsub event, payload: {payload:?}"
                            );
                        }
                    }
                    EventsubWebsocketData::Revocation {
                        metadata,
                        payload: _,
                    } => tracing::warn!(?metadata, "got a revocation event!"),
                    _ => {}
                }
                // twitch sends a keepalive every 10 seconds when *no other events are received*
                self.keep_alive().await?;
            }
            tungstenite::Message::Close(frame) => {
                tracing::warn!(?frame, "websocket closed")
            }
            _ => {}
        }
        Ok(MessageResult::Ok)
    }
}

async fn websocket_connect(url: &Url) -> Result<WebSocket> {
    tracing::info!(%url, "websocket connect");
    let (stream, _) = tokio_tungstenite::connect_async_with_config(
        url.clone(),
        Some(
            tungstenite::protocol::WebSocketConfig::default()
                .max_message_size(Some(64 << 20)) // 64 MiB
                .max_frame_size(Some(16 << 20)) // 16 MiB
                .accept_unmasked_frames(false),
        ),
        false,
    )
    .await?;
    Ok(WebSocket {
        stream,
        url: url.to_string(),
    })
}

struct WebSocket {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    url: String,
}

impl Drop for WebSocket {
    fn drop(&mut self) {
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::info!(url = self.url, "dropped websocket at:\n{backtrace:?}");
    }
}

impl Deref for WebSocket {
    type Target = WebSocketStream<MaybeTlsStream<TcpStream>>;

    fn deref(&self) -> &Self::Target {
        &self.stream
    }
}

impl DerefMut for WebSocket {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.stream
    }
}

enum MessageResult {
    Ok,
    Reconnect { url: Url },
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

async fn read_token(config: &Config, client: &TwitchClient, variant: &str) -> Result<UserToken> {
    let entry = keyring::Entry::new("limatoukka-the-twitch-bot", variant)?;
    tracing::info!("reading {variant} token");
    let token = match entry.get_password() {
        Ok(p) => {
            let data: TokenPair = serde_json::from_str(&p)?;
            UserToken::from_existing_or_refresh_token(
                client.get_client(),
                data.at,
                data.rt,
                config.twitch.client_id.clone(),
                Some(config.twitch.client_secret.clone()),
            )
            .await?
        }
        Err(keyring::Error::NoEntry) => full_auth(config, client, &entry, variant).await?,
        Err(e) => bail!(e),
    };
    Ok(token)
}

async fn full_auth(
    config: &Config,
    client: &TwitchClient,
    entry: &keyring::Entry,
    variant: &str,
) -> Result<UserToken> {
    let url: Url = config.twitch.redirect_url.parse()?;

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

    let mut builder = UserTokenBuilder::new(
        &*config.twitch.client_id,
        &*config.twitch.client_secret,
        url,
    )
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

#[cfg(test)]
mod tests {
    use std::env;

    use rustis::commands::ConnectionCommands;
    use tracing_subscriber::{
        EnvFilter, Layer as _,
        fmt::{Layer, time::LocalTime},
        layer::SubscriberExt,
        util::SubscriberInitExt,
    };
    use twitch_api::helix::points::{CreateCustomRewardBody, CreateCustomRewardRequest};

    use crate::services::{Services, storage::Storage};

    use super::*;

    fn setup_logging() {
        let fmt_layer = Layer::new()
            .with_timer(LocalTime::rfc_3339())
            .with_filter(EnvFilter::new(
                env::var(tracing_subscriber::EnvFilter::DEFAULT_ENV)
                    .as_deref()
                    .unwrap_or("tpn_bot=trace"),
            ));

        _ = tracing_subscriber::registry().with(fmt_layer).try_init();
    }

    #[tokio::test]
    #[ignore] // manual
    async fn test_eventsub() -> Result<()> {
        do_test_eventsub().await
    }

    async fn do_test_eventsub() -> Result<()> {
        setup_logging();

        let config = Config::load()?;
        let eventsub = EventSub::new(Twitch::new(&config).await?);
        let services = Services::mock().with_storage(Storage::new(&config).await?);
        services.storage().select(1).await?;

        let ctx = AppContext::new(config, services);

        let mut rx = eventsub.subscribe();
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                tracing::info!("got event: {event:?}");
            }
        });

        eventsub.run(ctx).await
    }

    async fn create_reward(twitch: &Twitch, body: CreateCustomRewardBody<'_>) -> Result<bool> {
        let res = twitch
            .caster_call(async |t| {
                let request = CreateCustomRewardRequest::broadcaster_id(t.caster_id);
                t.helix.req_post(request, body.clone(), &t.token).await?;
                Ok(())
            })
            .await;

        let Err(e) = res else {
            return Ok(true);
        };
        match e.downcast_ref::<ClientRequestError<reqwest::Error>>() {
            Some(ClientRequestError::HelixRequestPostError(HelixRequestPostError::Error {
                status: StatusCode::BAD_REQUEST,
                message,
                ..
            })) if message == "CREATE_CUSTOM_REWARD_DUPLICATE_REWARD" => Ok(false),
            _ => Err(e),
        }
    }

    #[tokio::test]
    #[ignore]
    async fn bootstrap_rewards() -> Result<()> {
        do_bootstrap_rewards().await
    }

    async fn do_bootstrap_rewards() -> Result<()> {
        let twitch = Twitch::new(&Config::load()?).await?;

        let created_hello = create_reward(
            &twitch,
            CreateCustomRewardBody::builder()
                .title("hello there")
                .prompt(Some("hiii".into()))
                .cost(1)
                .background_color(Some("#3F3F3F".into()))
                .is_max_per_user_per_stream_enabled(true)
                .max_per_user_per_stream(1)
                .build(),
        )
        .await?;

        let created_bless = create_reward(
            &twitch,
            CreateCustomRewardBody::builder()
                .title("BLESS THE RUN")
                .prompt(Some("thx! :)".into()))
                .cost(500)
                .background_color(Some("#E600D3".into()))
                .is_global_cooldown_enabled(true)
                .global_cooldown_seconds(180)
                .build(),
        )
        .await?;

        let created_curse = create_reward(
            &twitch,
            CreateCustomRewardBody::builder()
                .title("CURSE THE RUN")
                .prompt(Some("why? :(".into()))
                .cost(500)
                .background_color(Some("#5C16C5".into()))
                .is_global_cooldown_enabled(true)
                .global_cooldown_seconds(180)
                .build(),
        )
        .await?;

        dbg!(created_hello);
        dbg!(created_curse);
        dbg!(created_bless);

        Ok(())
    }
}
