use std::{collections::VecDeque, num::NonZero, ops::Deref, sync::Arc};

use anyhow::Result;
use dashmap::DashMap;
use thiserror::Error;

use crate::{
    commands::{args::Chatter, runner::CommandError},
    context::app::InterruptKind,
    services::storage_old::Storage,
};

use super::msg::MessageContext;

pub struct EvalContextShared {
    pub owner: Chatter,
    pub macro_args: VecDeque<Option<String>>,
}

#[derive(Clone)]
pub struct EvalContext {
    pub shared: Arc<EvalContextShared>,
    pub vars: Arc<DashMap<String, String>>,
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
        let owner = Chatter {
            id: parent.message().sender.id.clone(),
            login: parent.message().sender.login.clone(),
        };

        // FIXME cringetastic hack for tests
        let vars = if let Some(storage) = parent.service_opt::<Storage>() {
            storage.read_vars(&owner).await?
        } else {
            Default::default()
        };

        Ok(Self {
            vars: Arc::new(vars),
            shared: Arc::new(EvalContextShared {
                owner,
                macro_args: Default::default(),
            }),
            in_global_macro: false,
            macro_depth: 0,
            repeat_i: None,
            depth: 0,
            parent,
        })
    }

    pub fn is_owner(&self, chatter: &Chatter) -> bool {
        self.shared.owner.id == chatter.id
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
        owner: Chatter,
        is_global: bool,
        args: VecDeque<Option<String>>,
    ) -> Result<Self> {
        let vars = if self.shared.owner.id == owner.id {
            self.vars.clone()
        } else {
            Arc::new(self.storage().read_vars(&owner).await?)
        };
        Ok(Self {
            shared: Arc::new(EvalContextShared {
                owner,
                macro_args: args,
            }),
            vars,
            in_global_macro: self.in_global_macro || is_global,
            macro_depth: self.macro_depth + (!is_global) as u32,
            repeat_i: self.repeat_i,
            depth: self.depth + 1,
            parent: self.parent.clone(),
        })
    }

    pub fn interruptible<F, R>(
        &self,
        f: F,
    ) -> impl Future<Output = Result<Option<R>, Interrupted>> + use<F, R>
    where
        F: Future<Output = R>,
    {
        let interrupted = self.wait_for_interrupt();
        async move {
            tokio::select! {
                kind = interrupted => match kind {
                    InterruptKind::Break => Ok(None),
                    InterruptKind::Interrupt => Err(Interrupted),
                },
                r = f => Ok(Some(r))
            }
        }
    }
}

#[derive(Debug, Error)]
#[error("interrupted")]
pub struct Interrupted;

impl From<Interrupted> for CommandError {
    fn from(_: Interrupted) -> Self {
        CommandError::Interrupt
    }
}
