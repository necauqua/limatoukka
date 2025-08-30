use anyhow::Result;
use async_trait::async_trait;
use twitch_api::helix::{
    channels::{
        ModifyChannelInformation, ModifyChannelInformationBody, ModifyChannelInformationRequest,
    },
    chat::SendAShoutoutRequest,
};

use crate::twitch::Twitch;

#[async_trait]
pub trait TwitchService: Send + Sync {
    async fn get_user_id(&self, login: &str) -> Result<Option<String>>;
    async fn set_stream_title(&self, title: &str) -> Result<()>;
    async fn shout_out(&self, user_id: &str) -> Result<()>;
}

pub struct TwitchServiceImpl {
    twitch: Twitch,
}

impl TwitchServiceImpl {
    pub fn new(twitch: Twitch) -> Self {
        Self { twitch }
    }
}

#[async_trait]
impl TwitchService for TwitchServiceImpl {
    async fn get_user_id(&self, login: &str) -> Result<Option<String>> {
        let user = self
            .twitch
            .call(async |t| t.helix.get_user_from_login(login, &t.token).await)
            .await?;
        Ok(user.map(|u| u.id.take()))
    }

    async fn set_stream_title(&self, title: &str) -> Result<()> {
        self.twitch
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
        self.twitch
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
}
