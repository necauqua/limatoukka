use std::{collections::VecDeque, num::NonZero, ops::Deref, sync::Arc};

use anyhow::Result;
use dashmap::DashMap;
use thiserror::Error;

use crate::{
    commands::{args::Chatter, runner::CommandError},
    context::app::InterruptKind,
};

use super::msg::MessageContext;

struct Inner {
    owner: Chatter,
    macro_args: VecDeque<Option<String>>,
}

#[derive(Clone)]
pub struct EvalContext {
    parent: MessageContext,
    inner: Arc<Inner>,
    // separate Arc from inner because it's shared with nested eval contexts (see nest_macro)
    locals: Arc<DashMap<String, String>>,
    pub in_global_macro: bool,
    pub macro_depth: u32,
    pub repeat_i: Option<NonZero<u32>>,
    pub depth: u32,
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
        Ok(Self {
            parent,
            inner: Arc::new(Inner {
                owner,
                macro_args: Default::default(),
            }),
            locals: Default::default(),
            in_global_macro: false,
            macro_depth: 0,
            repeat_i: None,
            depth: 0,
        })
    }

    pub fn owner(&self) -> &Chatter {
        &self.inner.owner
    }

    pub fn macro_arg(&self, i: usize) -> Option<&str> {
        self.inner.macro_args.get(i).and_then(|s| s.as_deref())
    }

    pub fn set_local(&self, name: String, value: String) {
        self.locals.insert(name, value);
    }

    pub fn remove_local(&self, name: &str) {
        self.locals.remove(name);
    }

    pub fn clear_locals(&self) {
        self.locals.clear();
    }

    pub fn local_var(&self, name: &str) -> Option<String> {
        self.locals.get(name).map(|v| v.value().clone())
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
        macro_args: VecDeque<Option<String>>,
    ) -> Result<Self> {
        let locals = match self.inner.owner == owner {
            true => self.locals.clone(),
            false => Default::default(),
        };
        Ok(Self {
            parent: self.parent.clone(),
            inner: Arc::new(Inner { owner, macro_args }),
            locals,
            in_global_macro: self.in_global_macro || is_global,
            macro_depth: self.macro_depth + (!is_global) as u32,
            repeat_i: self.repeat_i,
            depth: self.depth + 1,
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
