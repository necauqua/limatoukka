use std::{ops::Deref, sync::Arc};

use neca_cmd::Command;

use crate::commands::{NativeCommand, runner::Location};

use super::eval::EvalContext;

#[derive(Clone)]
pub struct CommandContext {
    parent: EvalContext,
    pub meta: Arc<NativeCommand>,
    pub command: Command,
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
        meta: Arc<NativeCommand>,
        command: Command,
        pos: Location,
    ) -> Self {
        Self {
            parent: eval_ctx,
            meta,
            command,
            pos,
        }
    }
}
