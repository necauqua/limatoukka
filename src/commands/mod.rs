use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    pin::Pin,
    result::Result,
    sync::Arc,
    time::Duration,
};

use args::Args;

pub mod args;
pub mod runner;

use runner::CommandError;
pub use tpn_bot_macros::command;

use crate::{context::cmd::CommandContext, services::messaging::PermissionLevel};

pub type CommandResult = Result<(), CommandError>;
pub type CommandFuture = Pin<Box<dyn Future<Output = CommandResult> + Send>>;

#[derive(Debug)]
pub struct CommandArgDesc {
    pub name: &'static str,
    pub optional: fn() -> Option<Cow<'static, str>>,
    pub desc: fn() -> Cow<'static, str>,
}

#[derive(Debug, Clone)]
pub struct NativeCommand {
    pub name: &'static str,
    pub doc: &'static str,
    pub args: &'static [CommandArgDesc],
    pub module_path: &'static str,
    pub line_number: u32,
    pub action: fn(CommandContext, Args) -> CommandFuture,
    pub tags: &'static [CommandTag],
    /// Which minimum permission level is needed to use this command
    pub permission: PermissionLevel,
    /// A global timeout before the command can be used again
    pub global_gate: Option<Duration>,
    /// A timeout before the command can be used again by the same user
    pub sender_gate: Option<Duration>,
    /// An ultra-short version of the command
    pub shortcode: Option<&'static str>,
    /// Cost in charges to use the command
    pub cost: Option<i64>,
    /// Minimum permission level that allows to use the command for free
    pub free_for: PermissionLevel,
}

impl NativeCommand {
    pub fn is(&self, tag: CommandTag) -> bool {
        self.tags.contains(&tag)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandTag {
    /// Whether the command should not be shown in documentation
    Hidden,
    /// Whether the command should not be shown on the wall
    NoWall,
    /// Whether the command is for controlling the game
    NoitaControl,
    /// Whether the command is for reading the game state
    NoitaData,
    /// Whether the command cost and permission should be ignored if the command is run behind a global macro
    GlobalMacroExempt,
}

inventory::collect!(NativeCommand);

pub fn discover_declared_commands() -> HashMap<String, Arc<NativeCommand>> {
    let mut shortcodes = HashSet::new();

    inventory::iter::<NativeCommand>
        .into_iter()
        .flat_map(|cmd| match cmd.shortcode {
            Some(shortcode) => {
                assert!(
                    shortcodes.insert(shortcode),
                    "Duplicate shortcode: {shortcode}"
                );
                let cmd = Arc::new(cmd.clone());
                vec![
                    (cmd.name.to_owned(), cmd.clone()),
                    (shortcode.to_owned(), cmd),
                ]
            }
            None => vec![(cmd.name.to_owned(), Arc::new(cmd.clone()))],
        })
        .collect()
}
