use std::{
    collections::{HashMap, VecDeque},
    num::NonZero,
    ops::Deref,
    sync::Arc,
};

use anyhow::Result;
use rustis::commands::HashCommands;
use tokio::sync::RwLock;

use crate::{commands::runner::CommandInterrupt, context::app::InterruptKind};

use super::msg::MessageContext;

pub struct EvalContextShared {
    pub owner: String,
    pub macro_args: VecDeque<String>,
}

#[derive(Clone)]
pub struct EvalContext {
    pub shared: Arc<EvalContextShared>,
    pub vars: Arc<RwLock<HashMap<String, String>>>,
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
            }),
            vars: Arc::new(RwLock::new(vars)),
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
            self.vars.clone()
        } else {
            Arc::new(RwLock::new(
                self.storage().hgetall(format!("vars:{owner}")).await?,
            ))
        };
        Ok(Self {
            shared: Arc::new(EvalContextShared {
                owner: owner.into(),
                macro_args: args,
            }),
            vars,
            in_global_macro: self.in_global_macro || is_global,
            macro_depth: self.macro_depth + 1,
            repeat_i: self.repeat_i,
            depth: self.depth + 1,
            parent: self.parent.clone(),
        })
    }

    pub fn arg_expander(&self) -> impl FnMut(&str) -> Option<String> {
        |name| {
            if name == "i" {
                if let Some(i) = self.repeat_i {
                    return Some(i.to_string());
                }
            }
            if let Some(arg) = name
                .parse::<u32>()
                .ok()
                .filter(|n| *n != 0)
                .and_then(|n| self.shared.macro_args.get((n - 1) as _))
            {
                return Some(arg.clone());
            }
            // meh
            tokio::task::block_in_place(|| self.vars.blocking_read().get(name).cloned())
        }
    }

    pub fn interruptible<F>(
        &self,
        f: F,
    ) -> impl Future<Output = Result<(), CommandInterrupt>> + use<F>
    where
        F: Future<Output = ()>,
    {
        let interrupted = self.wait_for_interrupt();
        async move {
            tokio::select! {
                kind = interrupted => match kind {
                    InterruptKind::Break => Ok(()),
                    InterruptKind::Interrupt => Err(CommandInterrupt),
                },
                _ = f => Ok(())
            }
        }
    }
}
