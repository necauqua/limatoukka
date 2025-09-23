use std::{option::Option::Some, pin::Pin, sync::Arc, time::Duration};

use anyhow::Result;
use futures::StreamExt;
use tokio::{
    net::TcpStream,
    sync::{
        Notify,
        broadcast::{Receiver, Sender},
    },
    task::JoinSet,
    time::{Instant, Sleep},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{self, protocol::CloseFrame},
};
use twitch_api::{
    TWITCH_EVENTSUB_WEBSOCKET_URL,
    eventsub::{self, Event, EventsubWebsocketData, ReconnectPayload, SessionData, WelcomePayload},
    twitch_oauth2::url::Url,
};

use crate::{
    context::app::AppContext, integration::twitch_api::TwitchApi, services::caches::CacheServiceExt,
};

pub struct EventSub {
    twitch: TwitchApi,
    tx: Sender<Event>,
    prev: Option<WebSocket>,
    on_subscribed: Arc<Notify>,
    keepalive_timeout: Duration,
    canary: Pin<Box<Sleep>>,
}

impl EventSub {
    pub fn new(twitch: TwitchApi) -> Self {
        Self {
            twitch,
            tx: Sender::new(16),
            prev: None,
            on_subscribed: Default::default(),
            keepalive_timeout: Duration::from_secs(10),
            canary: Box::pin(tokio::time::sleep(Duration::from_secs(15))),
        }
    }

    async fn process_welcome_message(&mut self, data: SessionData<'_>) -> Result<()> {
        tracing::info!("got a welcome message");

        self.keepalive_timeout = data
            .keepalive_timeout_seconds
            .map_or_else(|| Duration::from_secs(10), |s| Duration::from_secs(s as _));

        // reset the canary just in case the timeout was changed
        self.keep_alive();

        if let Some(mut prev) = self.prev.take() {
            prev.close(None).await?;
            return Ok(());
        }

        let transport = eventsub::Transport::websocket(data.id.clone());

        let mut join_set = JoinSet::new();

        macro_rules! subscribe {
            ($group:ident::$event:ident $method:ident) => {
                let twitch = self.twitch.clone();
                join_set.spawn({
                    let transport = transport.clone();
                    async move {
                        let res = twitch.caster_call(|t| {
                            let transport = transport.clone();
                            async move {
                                t.helix.create_eventsub_subscription(
                                    eventsub::$group::$event::$method(t.caster_id),
                                    transport,
                                    &t.token,
                                ).await
                            }
                        })
                        .await;
                        match res {
                            Ok(_) => tracing::info!("subscribed to {}", stringify!($event)),
                            Err(e) => tracing::error!(error=?e, "failed to subscribe to {}", stringify!($event)),
                        }
                    }
                });
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
            stream::StreamOnlineV1,
            stream::StreamOfflineV1,
        ];

        join_set.join_all().await;

        self.on_subscribed.notify_waiters();

        Ok(())
    }

    fn keep_alive(&mut self) {
        self.canary
            .as_mut()
            .reset(Instant::now() + self.keepalive_timeout + Duration::from_secs(5));
    }

    pub fn wait_for_full_init(&self) -> impl Future<Output = ()> + use<> {
        let notif = self.on_subscribed.clone();
        async move {
            notif.notified().await;
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
                    match self.process_message(msg?, ctx).await? {
                        MessageResult::Ok => {},
                        MessageResult::Reconnect { url } => {
                            tracing::info!(%url, "reconnect event");
                            self.prev = Some(std::mem::replace(&mut s, websocket_connect(&url).await?));
                        },
                        MessageResult::Closed(frame) => {
                            tracing::warn!(?frame, "websocket closed");
                            s = websocket_connect(&TWITCH_EVENTSUB_WEBSOCKET_URL).await?;
                        },
                    }
                }
                _ = &mut self.canary => {
                    tracing::warn!("websocket keepalive timeout, reconnecting");
                    self.prev = Some(std::mem::replace(&mut s, websocket_connect(&TWITCH_EVENTSUB_WEBSOCKET_URL).await?));
                }
            }
        }
    }

    async fn process_message(
        &mut self,
        msg: tungstenite::Message,
        ctx: &AppContext,
    ) -> Result<MessageResult> {
        // twitch sends a keepalive every 10 seconds when *no other events are received*
        self.keep_alive();
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
                        return Ok(MessageResult::Reconnect { url });
                    }
                    EventsubWebsocketData::Notification { metadata, payload } => {
                        let new = ctx
                            .caches()
                            .set(
                                "eventsub-seen",
                                Duration::from_secs(600),
                                &metadata.message_id,
                                "1",
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
                    EventsubWebsocketData::Keepalive { .. } => {
                        // noop, update the canary below
                    }
                    _ => unreachable!("EventsubWebsocketData got a new variant added"),
                }
            }
            tungstenite::Message::Close(frame) => {
                return Ok(MessageResult::Closed(frame));
            }
            _ => {}
        }
        Ok(MessageResult::Ok)
    }
}

type WebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn websocket_connect(url: &Url) -> Result<WebSocket> {
    tracing::info!(%url, "websocket connect");
    let (stream, _) = tokio_tungstenite::connect_async_with_config(
        url.clone(),
        Some(
            tungstenite::protocol::WebSocketConfig::default()
                .max_message_size(Some(64 << 20)) // 64 MiB
                .max_frame_size(Some(16 << 20)), // 16 MiB
        ),
        false,
    )
    .await?;
    Ok(stream)
}

enum MessageResult {
    Ok,
    Reconnect { url: Url },
    Closed(Option<CloseFrame>),
}

#[cfg(test)]
mod tests {
    use std::env;

    use tracing_subscriber::{
        EnvFilter, Layer as _,
        fmt::{Layer, time::LocalTime},
        layer::SubscriberExt,
        util::SubscriberInitExt,
    };
    use twitch_api::helix::points::CreateCustomRewardBody;

    use crate::{
        config::Config,
        services::{
            Injector,
            caches::{CacheService, CacheServiceInMemory},
        },
    };

    use super::*;

    fn setup_logging() {
        let fmt_layer = Layer::new()
            .with_timer(LocalTime::rfc_3339())
            .with_filter(EnvFilter::new(
                env::var(tracing_subscriber::EnvFilter::DEFAULT_ENV)
                    .as_deref()
                    .unwrap_or("limatoukka=trace"),
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
        let eventsub = EventSub::new(TwitchApi::new(&config).await?);

        let ctx = AppContext::new(
            Injector::new().with::<dyn CacheService>(Arc::new(CacheServiceInMemory::default())),
        );

        let mut rx = eventsub.subscribe();
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                tracing::info!("got event: {event:?}");
            }
        });

        eventsub.run(ctx).await
    }

    #[tokio::test]
    #[ignore]
    async fn bootstrap_rewards() -> Result<()> {
        do_bootstrap_rewards().await
    }

    async fn do_bootstrap_rewards() -> Result<()> {
        let twitch = TwitchApi::new(&Config::load()?).await?;

        let buy1 = twitch
            .create_reward(
                CreateCustomRewardBody::builder()
                    .title("Buy 1 charge")
                    .prompt(Some(
                        "Sell fish for charges which *will* be used by various interactions".into(),
                    ))
                    .cost(1000)
                    .background_color(Some("#16C4AA".into()))
                    .build(),
            )
            .await?;

        let buy5 = twitch
            .create_reward(
                CreateCustomRewardBody::builder()
                    .title("Buy 5 charges")
                    .prompt(Some(
                        "Sell fish for charges which *will* be used by various interactions".into(),
                    ))
                    .cost(5000)
                    .background_color(Some("#16C4AA".into()))
                    .build(),
            )
            .await?;

        let buy10 = twitch
            .create_reward(
                CreateCustomRewardBody::builder()
                    .title("Buy 10 charges")
                    .prompt(Some(
                        "Sell fish for charges which *will* be used by various interactions".into(),
                    ))
                    .cost(10000)
                    .background_color(Some("#16C4AA".into()))
                    .build(),
            )
            .await?;

        let buy50 = twitch
            .create_reward(
                CreateCustomRewardBody::builder()
                    .title("Buy 50 charges")
                    .prompt(Some(
                        "Sell fish for charges which *will* be used by various interactions".into(),
                    ))
                    .cost(50000)
                    .background_color(Some("#16C4AA".into()))
                    .build(),
            )
            .await?;

        let buy100 = twitch
            .create_reward(
                CreateCustomRewardBody::builder()
                    .title("Buy 100 charges")
                    .prompt(Some(
                        "Sell fish for charges which *will* be used by various interactions".into(),
                    ))
                    .cost(100000)
                    .background_color(Some("#16C4AA".into()))
                    .build(),
            )
            .await?;

        dbg!(buy1, buy5, buy10, buy50, buy100);

        Ok(())
    }
}
