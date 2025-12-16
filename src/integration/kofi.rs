use anyhow::Context;
use serde::Deserialize;
use twitch_api::twitch_oauth2::url::form_urlencoded;

#[derive(Debug, Deserialize)]
pub enum KofiType {
    Donation,
    Subscription,
    #[serde(rename = "Shop Order")]
    ShopOrder,
}

#[derive(Debug, Deserialize)]
pub struct KofiPayload {
    #[serde(rename = "type")]
    pub event_type: KofiType,
    pub is_public: bool,
    pub from_name: String,
    pub message: Option<String>,
    pub amount: String,
    pub currency: String,
    pub is_subscription_payment: bool,
    pub is_first_subscription_payment: bool,
    pub tier_name: Option<String>,
}

impl KofiPayload {
    pub fn parse_payload(payload: &str) -> anyhow::Result<Self> {
        let (_, data) = form_urlencoded::parse(payload.as_bytes())
            .find(|(key, _)| key == "data")
            .context("missing 'data' field in ko-fi urlencoded message")?;

        Ok(serde_json::from_str::<KofiPayload>(&data)?)
    }
}
