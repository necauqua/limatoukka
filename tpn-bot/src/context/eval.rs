use std::{ops::Deref, sync::Arc};

use super::msg::MessageContext;

#[derive(Clone)]
pub struct EvalContext {
    parent: MessageContext,
    pub owner: Arc<str>,
    pub depth: u32,
    pub macro_depth: u32,
    pub in_global_macro: bool,
}

impl Deref for EvalContext {
    type Target = MessageContext;

    fn deref(&self) -> &Self::Target {
        &self.parent
    }
}

impl EvalContext {
    pub fn new(parent: MessageContext) -> Self {
        Self {
            owner: (&*parent.message().sender.id).into(),
            parent,
            depth: 0,
            macro_depth: 0,
            in_global_macro: false,
        }
    }

    pub fn nesting_str(&self) -> String {
        let mut s = String::with_capacity(self.depth as _);
        for _ in 0..self.depth {
            s.push('|');
        }
        s
    }

    pub fn nest(&self) -> Self {
        Self {
            parent: self.parent.clone(),
            owner: self.owner.clone(),
            depth: self.depth + 1,
            macro_depth: self.macro_depth,
            in_global_macro: self.in_global_macro,
        }
    }

    pub fn nest_macro(&self, owner: &str, is_global: bool) -> Self {
        Self {
            parent: self.parent.clone(),
            owner: owner.into(),
            depth: self.depth + 1,
            macro_depth: self.macro_depth + 1,
            in_global_macro: self.in_global_macro || is_global,
        }
    }
}
