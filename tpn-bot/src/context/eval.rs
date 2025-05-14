use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    num::NonZero,
    ops::Deref,
    sync::Arc,
};

use anyhow::Result;
use rustis::commands::{HashCommands, SetCondition, SetExpiration, StringCommands};
use tokio::sync::RwLock;

use crate::fail;

use super::msg::MessageContext;

pub struct EvalContextShared {
    pub owner: String,
    pub macro_args: VecDeque<String>,
    pub vars: RwLock<HashMap<String, String>>,
}

#[derive(Clone)]
pub struct EvalContext {
    pub shared: Arc<EvalContextShared>,
    pub in_global_macro: bool,
    pub macro_depth: u32,
    pub repeat_i: Option<NonZero<u32>>,
    pub depth: u32,
    parent: MessageContext,
}

impl Deref for EvalContext {
    type Target = MessageContext;

    fn deref(&self) -> &Self::Target {
        &self.parent
    }
}

impl EvalContext {
    pub async fn new(parent: MessageContext) -> Result<Self> {
        let owner = parent.message().sender.id.clone();

        let vars: HashMap<String, String> =
            parent.storage().hgetall(format!("vars:{owner}")).await?;

        Ok(Self {
            shared: Arc::new(EvalContextShared {
                owner,
                macro_args: Default::default(),
                vars: RwLock::new(vars),
            }),
            in_global_macro: false,
            macro_depth: 0,
            repeat_i: None,
            depth: 0,
            parent,
        })
    }

    pub fn nesting_str(&self) -> String {
        let mut s = String::with_capacity(self.depth as _);
        for _ in 0..self.depth {
            s.push('|');
        }
        s
    }

    pub fn nest(&self) -> Self {
        let mut clone = self.clone();
        clone.depth += 1;
        clone
    }

    pub fn nest_repeat(&self, i: NonZero<u32>) -> Self {
        let mut clone = self.nest();
        clone.repeat_i = Some(i);
        clone
    }

    pub async fn nest_macro(
        &self,
        owner: &str,
        is_global: bool,
        args: VecDeque<String>,
    ) -> Result<Self> {
        let vars = if self.shared.owner == owner {
            self.shared.vars.read().await.clone()
        } else {
            self.storage().hgetall(format!("vars:{owner}")).await?
        };
        Ok(Self {
            shared: Arc::new(EvalContextShared {
                owner: owner.into(),
                macro_args: args,
                vars: RwLock::new(vars),
            }),
            in_global_macro: self.in_global_macro || is_global,
            macro_depth: self.macro_depth + 1,
            repeat_i: self.repeat_i,
            depth: self.depth + 1,
            parent: self.parent.clone(),
        })
    }

    pub async fn chatter_id(&self, login: Option<&str>) -> Result<Cow<'_, str>> {
        let Some(login) = login else {
            return Ok(Cow::Borrowed(&self.shared.owner));
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
