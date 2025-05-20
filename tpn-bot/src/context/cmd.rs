use std::{
    fmt::{self, Display},
    ops::Deref,
};

use crate::commands::CommandRegistration;

use super::eval::EvalContext;

#[derive(Debug, Clone)]
pub struct CommandToken {
    pub name: neca_cmd::CommandToken,
    pub group: usize,
    pub idx: usize,
}

#[derive(Debug, Clone)]
pub struct CommandDescriptor {
    pub registration: &'static CommandRegistration,
    pub token: CommandToken,
}

impl Display for CommandDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.token
            .name
            .tpe
            .write_command(f, self.registration.name)?;
        if f.alternate() {
            write!(f, "({},{})", self.token.group, self.token.idx)?;
        }
        Ok(())
    }
}

impl Display for CommandToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        if f.alternate() {
            write!(f, "({},{})", self.group, self.idx)?;
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct CommandContext {
    parent: EvalContext,
    pub command: CommandDescriptor,
}

impl Deref for CommandContext {
    type Target = EvalContext;

    fn deref(&self) -> &Self::Target {
        &self.parent
    }
}

impl CommandContext {
    pub fn new(eval_ctx: EvalContext, command: CommandDescriptor) -> Self {
        Self {
            parent: eval_ctx,
            command,
        }
    }
}
