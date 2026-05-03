use anyhow::Result;
use async_trait::async_trait;
use futures::TryStreamExt;
use twitch_api::{
    helix::{
        channels::{
            ModifyChannelInformation, ModifyChannelInformationBody, ModifyChannelInformationRequest,
        },
        chat::SendAShoutoutRequest,
        moderation::GetBannedUsersRequest,
        points::{
            CustomRewardRedemptionStatus, UpdateRedemptionStatusBody, UpdateRedemptionStatusRequest,
        },
    },
    types::UserId,
};

use crate::{injector_getter, integration::twitch_api::TwitchApi, services::Service};

pub enum UserIdResponse {
    Found(UserId),
    NotFound,
    Banned,
}

#[async_trait]
pub trait TwitchService: Service {
    async fn is_live(&self) -> Result<bool>;

    async fn get_user_id(&self, login: &str) -> Result<UserIdResponse>;

    async fn set_stream_title(&self, title: &str) -> Result<()>;

    async fn shout_out(&self, user_id: &str) -> Result<()>;

    async fn fulfill_redemption(&self, reward_id: &str, id: &str) -> Result<()>;
}

injector_getter!(TwitchService::twitch);

pub struct TwitchServiceImpl(TwitchApi);

impl TwitchServiceImpl {
    pub fn new(twitch: TwitchApi) -> Self {
        Self(twitch)
    }
}

#[async_trait]
impl TwitchService for TwitchServiceImpl {
    async fn is_live(&self) -> Result<bool> {
        let streams = self
            .0
            .call(async |t| {
                t.helix
                    .get_streams_from_ids(&(&[t.caster_id]).into(), &t.token)
                    .try_collect::<Vec<_>>()
                    .await
            })
            .await?;
        Ok(!streams.is_empty())
    }

    async fn get_user_id(&self, login: &str) -> Result<UserIdResponse> {
        let Some(user) = self
            .0
            .call(async |t| t.helix.get_user_from_login(login, &t.token).await)
            .await?
        else {
            return Ok(UserIdResponse::NotFound);
        };

        let ban_resp = self
            .0
            .caster_call(async |t| {
                t.helix
                    .req_get(
                        GetBannedUsersRequest::broadcaster_id(t.caster_id).users(vec![&user.id]),
                        &t.token,
                    )
                    .await
            })
            .await?;

        Ok(if ban_resp.data.iter().any(|b| b.expires_at.is_none()) {
            UserIdResponse::Banned
        } else {
            UserIdResponse::Found(user.id)
        })
    }

    async fn set_stream_title(&self, title: &str) -> Result<()> {
        self.0
            .caster_call(async |t| {
                let request = ModifyChannelInformationRequest::broadcaster_id(t.caster_id);
                let mut body = ModifyChannelInformationBody::new();
                body.title(title);

                let response: ModifyChannelInformation =
                    t.helix.req_patch(request, body, &t.token).await?.data;

                Ok(response)
            })
            .await?;
        Ok(())
    }

    async fn shout_out(&self, user_id: &str) -> Result<()> {
        self.0
            .call(async |t| {
                let request =
                    SendAShoutoutRequest::new(t.caster_id, user_id, t.token.user_id.clone());
                t.helix
                    .req_post(request, Default::default(), &t.token)
                    .await?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn fulfill_redemption(&self, reward_id: &str, id: &str) -> Result<()> {
        self.0
            .caster_call(async |t| {
                let request = UpdateRedemptionStatusRequest::new(t.caster_id, reward_id, id);
                let body =
                    UpdateRedemptionStatusBody::status(CustomRewardRedemptionStatus::Fulfilled);
                t.helix.req_patch(request, body, &t.token).await?;
                Ok(())
            })
            .await?;
        Ok(())
    }
}
