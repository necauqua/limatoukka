use std::ops::Deref;

use neca_cmd::Token;

use crate::commands::{CommandMetadata, runner::Location};

use super::eval::EvalContext;

#[derive(Clone)]
pub struct CommandContext {
    parent: EvalContext,
    pub meta: &'static CommandMetadata,
    pub token: Token,
    pub pos: Location,
}

impl Deref for CommandContext {
    type Target = EvalContext;

    fn deref(&self) -> &Self::Target {
        &self.parent
    }
}

impl CommandContext {
    pub fn new(
        eval_ctx: EvalContext,
        meta: &'static CommandMetadata,
        token: Token,
        pos: Location,
    ) -> Self {
        Self {
            parent: eval_ctx,
            meta,
            token,
            pos,
        }
    }
}
