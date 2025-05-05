use std::{
    borrow::Cow,
    ops::Deref,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use anyhow::{Ok, Result};
use rustis::{
    client::BatchPreparedCommand,
    commands::{GenericCommands, ListCommands, SetCondition, SetExpiration, StringCommands},
};

use crate::{fail, services::messaging::Message};

use super::app::AppContext;

struct MessageState {
    message: Message,
    repeats: AtomicU32,
}

#[derive(Clone)]
pub struct MessageContext {
    parent: AppContext,
    state: Arc<MessageState>,
}

// deref hack lol
impl Deref for MessageContext {
    type Target = AppContext;

    fn deref(&self) -> &Self::Target {
        &self.parent
    }
}

impl MessageContext {
    pub fn new(state: AppContext, message: Message) -> Self {
        Self {
            parent: state,
            state: Arc::new(MessageState {
                message,
                repeats: AtomicU32::new(0),
            }),
        }
    }

    pub fn message(&self) -> &Message {
        &self.state.message
    }

    pub fn inc_repeats(&self) -> u32 {
        self.state.repeats.fetch_add(1, Ordering::Relaxed)
    }

    pub fn reset_repeats(&self) {
        self.state.repeats.store(0, Ordering::Relaxed);
    }

    /// Returns true once (atomically) in the given period - per key and per sender.
    pub async fn sender_gate(&self, key: &str, period: Duration) -> Result<bool> {
        self.gate(&format!("{key}:{}", self.message().sender.id), period)
            .await
    }

    pub async fn reply(&self, message: String) -> Result<()> {
        tracing::debug!(reply = message, "replying");
        self.messaging().reply(self.message(), message).await?;
        Ok(())
    }

    pub async fn reply_buffered(&self, message: String) -> Result<()> {
        let state_key = format!("reply_buffered:{}", self.message().sender.id);

        if self.storage().rpush(&state_key, &message).await? != 1 {
            tracing::debug!(message, "adding to existing reply buffer");
            return Ok(());
        }

        tracing::debug!(message, "new reply buffer");
        let ctx = self.clone();
        self.schedule(Duration::from_millis(100), move |_| async move {
            let mut tx = ctx.storage().create_transaction();
            tx.lrange::<_, _, Vec<String>>(&state_key, 0, -1).queue();
            tx.del(&state_key).forget();

            let messages: Vec<String> = tx.execute().await?;
            ctx.reply(messages.join("; ")).await
        });

        Ok(())
    }

    pub async fn chatter_id(&self, login: Option<&str>) -> Result<Cow<'_, str>> {
        let Some(login) = login else {
            return Ok(Cow::Borrowed(&self.message().sender.id));
        };

        let key = format!("chatter:{login}");
        let cached: Option<String> = self.storage().get(&key).await?;
        if let Some(cached) = cached {
            return Ok(cached.into());
        }

        let full = self
            .twitch()
            .call(|t| async move { t.helix.get_user_from_login(login, &t.token).await })
            .await?;
        let Some(user) = full else {
            fail!("this user does not exist");
        };

        self.storage()
            .set_with_options(
                key,
                user.id.as_str(),
                SetCondition::None,
                SetExpiration::Ex(3600),
                false,
            )
            .await?;

        Ok(user.id.take().into())
    }
}
