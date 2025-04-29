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
use helix::{
    ClientRequestError, HelixRequestDeleteError, HelixRequestGetError, HelixRequestPatchError,
    HelixRequestPostError, HelixRequestPutError, users::User,
};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::{
    net::TcpListener,
    sync::{Notify, RwLock, oneshot},
};
use twitch_api::{twitch_oauth2::*, *};
use twitch_irc::login::{CredentialsPair, LoginCredentials};
use url::Url;

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

async fn full_auth(
    config: &TwitchApp,
    client: &TwitchClient,
    entry: &keyring::Entry,
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

    println!("\nFull auth needed, go to: {url}\n");

    let (state, code) = s.await??;
    let token = builder.get_user_token(client, &state, &code).await?;
    entry.set_password(&serde_json::to_string(&TokenPair::from(&token))?)?;
    Ok(token)
}

async fn read_token(config: &TwitchApp, client: &TwitchClient, variant: &str) -> Result<UserToken> {
    let entry = keyring::Entry::new("limatoukka-the-twitch-bot", variant)?;
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
        Err(keyring::Error::NoEntry) => full_auth(config, client, &entry).await?,
        Err(e) => bail!(e),
    };
    Ok(token)
}

type TwitchClient = twitch_api::TwitchClient<'static, reqwest::Client>;

struct Inner {
    client: TwitchClient,
    token: RwLock<UserToken>,
    target: User,
    bot: String,
}

#[derive(Clone)]
pub struct Twitch {
    inner: Arc<Inner>,
}

impl Debug for Twitch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Twitch")
            .field("bot", &self.bot())
            .field("target", &self.target().login)
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

pub struct TwitchRefs<'a> {
    pub helix: &'a HelixClient<'static, reqwest::Client>,
    pub target: &'a User,
    pub token: UserToken,
}

impl Twitch {
    pub fn bot(&self) -> &str {
        &self.inner.bot
    }

    pub fn target(&self) -> &User {
        &self.inner.target
    }

    pub async fn new(config: &TwitchApp) -> Result<Self> {
        let client = TwitchClient::new();

        let token = read_token(config, &client, "bot").await?;

        let target = client
            .helix
            .get_user_from_login(&config.target_channel, &token)
            .await?
            .with_context(|| format!("twitch user {} not found", config.target_channel))?;

        Ok(Self {
            inner: Arc::new(Inner {
                client,
                bot: token.login.as_str().into(),
                token: token.into(),
                target,
            }),
        })
    }

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
