use std::{borrow::Cow, sync::Arc, time::Duration};

use anyhow::Result;
use async_trait::async_trait;
use futures::TryStreamExt;

use crate::{
    injector_getter,
    integration::twitch_api::TwitchApi,
    services::{Service, caches::CacheService},
};

#[async_trait]
pub trait NamesService: Service {
    async fn warm_up(&self, user_ids: &[&str]) -> Result<()>;

    async fn lookup<'u>(&self, user_id: &'u str) -> Result<Cow<'u, str>>;
}

injector_getter!(NamesService::names { NamesServiceNoop });

pub struct NamesServiceNoop;

#[async_trait]
impl NamesService for NamesServiceNoop {
    async fn warm_up(&self, _user_ids: &[&str]) -> Result<()> {
        Ok(())
    }

    async fn lookup<'u>(&self, user_id: &'u str) -> Result<Cow<'u, str>> {
        Ok(Cow::Borrowed(user_id))
    }
}

pub struct NamesServiceImpl {
    cache: Arc<dyn CacheService>,
    twitch: TwitchApi,
}

impl NamesServiceImpl {
    pub fn new(cache: Arc<dyn CacheService>, twitch: TwitchApi) -> Self {
        Self { cache, twitch }
    }
}

const CACHE: &str = "names";
const TTL: Duration = Duration::from_hours(24);

#[async_trait]
impl NamesService for NamesServiceImpl {
    async fn warm_up(&self, user_ids: &[&str]) -> Result<()> {
        let mut cold = Vec::new();
        for &user_id in user_ids {
            if self.cache.get(CACHE, user_id).await?.is_none() {
                cold.push(user_id);
            }
        }

        let cold = &cold[..];
        let response = self
            .twitch
            .call(async |t| {
                t.helix
                    .get_users_from_ids(&cold.into(), &t.token)
                    .try_collect::<Vec<_>>()
                    .await
            })
            .await?;

        for user in response {
            self.cache
                .set(CACHE, TTL, user.id.as_str(), user.display_name.as_str())
                .await?;
        }

        Ok(())
    }

    async fn lookup<'u>(&self, user_id: &'u str) -> Result<Cow<'u, str>> {
        if let Some(cached) = self.cache.get(CACHE, user_id).await? {
            return Ok(Cow::Owned(cached));
        }

        let user = self
            .twitch
            .call(async |t| t.helix.get_user_from_id(user_id, &t.token).await)
            .await?;

        Ok(match user {
            Some(user) => {
                self.cache
                    .set(CACHE, TTL, user_id, user.display_name.as_str())
                    .await?;
                Cow::Owned(user.display_name.take())
            }
            None => Cow::Borrowed(user_id),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{config::Config, services::caches::CacheServiceInMemory};

    use super::*;

    #[tokio::test]
    #[ignore = "manual test"]
    async fn get_name() -> Result<()> {
        let twitch = TwitchApi::new(&Config::load()?).await?;
        let caches = Arc::new(CacheServiceInMemory::default());

        let service: Arc<dyn NamesService> = Arc::new(NamesServiceImpl::new(caches, twitch));

        let test = &[
            "57872632",
            "839321138",
            "1450860847",
            "142813827",
            "97501875",
        ];

        service.warm_up(test).await?;
        println!("warmed up");
        println!("hit = {:?}", service.lookup("97501875").await?);
        println!("miss = {:?}", service.lookup("42001248").await?);

        Ok(())
    }
}
