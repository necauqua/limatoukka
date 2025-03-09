use std::{borrow::Cow, collections::HashMap, pin::Pin, sync::LazyLock};

use anyhow::Result;
use args::{Args, ExtractorResult};
use context::CommandContext;

pub mod args;
pub mod context;
pub mod parsing;
pub mod runner;

pub use tpn_bot_macros::command;

pub type CommandFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
pub type CommandPtr = fn(CommandContext, Args) -> ExtractorResult<CommandFuture>;

pub struct CommandArgDesc {
    pub name: &'static str,
    pub optional: bool,
    pub desc: fn() -> Cow<'static, str>,
}

pub struct CommandRegistration {
    pub name: &'static str,
    pub doc: &'static str,
    pub args: &'static [CommandArgDesc],
    pub module_path: &'static str,
    pub line_number: u32,
    pub handler: CommandPtr,
}

inventory::collect!(CommandRegistration);

pub fn find_command(name: &str) -> Option<CommandPtr> {
    static MAP: LazyLock<HashMap<&str, CommandPtr>> = LazyLock::new(|| {
        inventory::iter::<CommandRegistration>
            .into_iter()
            .map(|reg| (reg.name, reg.handler))
            .collect()
    });

    MAP.get(name).copied()
}
