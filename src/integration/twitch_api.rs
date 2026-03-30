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
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::{
    net::TcpListener,
    sync::{Notify, RwLock, oneshot},
    task::JoinSet,
};
use tracing::instrument;
use twitch_api::{
    HelixClient,
    helix::{
        ClientRequestError, HelixRequestDeleteError, HelixRequestGetError, HelixRequestPatchError,
        HelixRequestPostError, HelixRequestPutError, Scope,
        points::{
            CreateCustomRewardBody, CreateCustomRewardRequest, CustomReward,
            UpdateCustomRewardBody, UpdateCustomRewardRequest,
        },
    },
    twitch_oauth2::{
        AccessToken, RefreshToken, TwitchToken as _, UserToken, UserTokenBuilder, url::Url,
    },
    types::RewardId,
};
use twitch_irc::login::{CredentialsPair, LoginCredentials};

use crate::config::Config;

type TwitchClient = twitch_api::TwitchClient<'static, reqwest::Client>;

struct Inner {
    client: TwitchClient,
    bot_id: String,
    bot_login: String,
    caster_id: String,
    caster_login: String,
    bot_token: TwitchToken,
    caster_token: TwitchToken,
}

#[derive(Clone)]
pub struct TwitchApi {
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
        //  cant event blame twitch-api for being bad, it's Rust auto-capture
        //  being too broad and the use<> thing being real new (and still
        //  annoying) although they could've implemented TwitchToken like for
        //  Arc, who needs a Box impl lmao
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

impl Debug for TwitchApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Twitch")
    }
}

#[async_trait]
impl LoginCredentials for TwitchApi {
    type Error = anyhow::Error;

    async fn get_credentials(&self) -> Result<CredentialsPair, Self::Error> {
        let token = self.inner.bot_token.token.read().await.clone();

        // idk about the time
        let token = if token.expires_in() < Duration::from_secs(1800) {
            let mut token = self.inner.bot_token.token.write().await;
            tracing::info!("(irc) token close to expiration, refreshing");

            // workaround the bug with token losing the refresh token if refreshing fails
            let rt = token.refresh_token.clone();

            token
                .refresh_token(self.inner.client.helix.get_client())
                .await
                .map_err(|e| {
                    tracing::error!(error=?e, "(irc) failed to refresh token");
                    // restore that token
                    token.refresh_token = rt;
                    e
                })?;

            token.clone()
        } else {
            token
        };

        Ok(CredentialsPair {
            login: token.login.take(),
            token: Some(token.access_token.take()),
        })
    }
}

pub struct TwitchRefs<'a> {
    pub helix: &'a HelixClient<'static, reqwest::Client>,
    pub caster_id: &'a str,
    pub token: UserToken,
}

impl TwitchApi {
    pub fn bot_id(&self) -> &str {
        &self.inner.bot_id
    }

    pub fn bot_login(&self) -> &str {
        &self.inner.bot_login
    }

    pub fn caster_id(&self) -> &str {
        &self.inner.caster_id
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
                bot_login: bot_token.login.clone().take(),
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
    #[instrument(name = "twitch-api-bot-call", skip_all)]
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

    #[instrument(name = "twitch-api-caster-call", skip_all)]
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

    pub async fn create_reward(&self, body: CreateCustomRewardBody<'_>) -> Result<bool> {
        let res = self
            .caster_call(async |t| {
                let request = CreateCustomRewardRequest::broadcaster_id(t.caster_id);
                t.helix.req_post(request, body.clone(), &t.token).await
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

    pub async fn update_reward(
        &self,
        id: RewardId,
        body: UpdateCustomRewardBody<'_>,
    ) -> Result<()> {
        self.caster_call(async |t| {
            let request = UpdateCustomRewardRequest::new(t.caster_id, id.clone());
            t.helix.req_patch(request, body.clone(), &t.token).await
        })
        .await?;
        Ok(())
    }

    async fn get_rewards(&self) -> Result<Vec<CustomReward>> {
        self.caster_call(async |t| {
            t.helix
                .get_all_custom_rewards(t.caster_id, false, &t.token)
                .await
        })
        .await
    }

    pub async fn unpause_rewards(&self) -> Result<()> {
        let rewards = self.get_rewards().await?;

        let mut join_set = JoinSet::new();

        for reward in rewards {
            // mega cringe todo make this not cringe lmao
            if reward.title == "CURSE THE RUN" || reward.title == "BLESS THE RUN" {
                continue;
            }
            if !reward.is_paused {
                tracing::info!(reward.title, "reward already unpaused, skipping");
                continue;
            }
            tracing::info!(reward.title, "unpausing reward");
            let request = UpdateCustomRewardBody::builder()
                .is_paused(Some(false))
                .build();
            let handle = self.clone();
            join_set.spawn(async move { handle.update_reward(reward.id, request).await });
        }

        join_set.join_all().await;

        Ok(())
    }

    pub async fn pause_rewards(&self) -> Result<()> {
        let rewards = self.get_rewards().await?;

        let mut join_set = JoinSet::new();

        for reward in rewards {
            if reward.is_paused {
                tracing::info!(reward.title, "reward already paused, skipping");
                continue;
            }
            tracing::info!(reward.title, "pausing reward");
            let request = UpdateCustomRewardBody::builder()
                .is_paused(Some(true))
                .build();
            let handle = self.clone();
            join_set.spawn(async move { handle.update_reward(reward.id, request).await });
        }

        join_set.join_all().await;

        Ok(())
    }
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
