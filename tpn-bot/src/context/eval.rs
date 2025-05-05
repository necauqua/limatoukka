use std::{collections::VecDeque, ops::Deref, sync::Arc};

use super::msg::MessageContext;

pub struct MacroContext {
    pub owner: String,
    pub args: VecDeque<String>,
}

#[derive(Clone)]
pub struct EvalContext {
    pub macro_ctx: Arc<MacroContext>,
    pub in_global_macro: bool,
    pub macro_depth: u32,
    depth: u32,
    parent: MessageContext,
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
            macro_ctx: Arc::new(MacroContext {
                owner: (&*parent.message().sender.id).into(),
                args: Default::default(),
            }),
            depth: 0,
            macro_depth: 0,
            in_global_macro: false,
            parent,
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
            macro_ctx: self.macro_ctx.clone(),
            depth: self.depth + 1,
            macro_depth: self.macro_depth,
            in_global_macro: self.in_global_macro,
        }
    }

    pub fn nest_macro(&self, owner: &str, is_global: bool, args: VecDeque<String>) -> Self {
        Self {
            parent: self.parent.clone(),
            macro_ctx: Arc::new(MacroContext {
                owner: owner.into(),
                args,
            }),
            depth: self.depth + 1,
            macro_depth: self.macro_depth + 1,
            in_global_macro: self.in_global_macro || is_global,
        }
    }
}
