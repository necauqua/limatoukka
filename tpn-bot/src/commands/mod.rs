use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::LazyLock,
    time::Duration,
};

use args::{Args, ExtractorResult};

pub mod args;
pub mod runner;

use runner::CommandError;
pub use tpn_bot_macros::command;

use crate::{context::cmd::CommandContext, services::messaging::PermissionLevel};

pub type CommandResult = std::result::Result<(), CommandError>;
pub type CommandFuture = Pin<Box<dyn Future<Output = CommandResult> + Send>>;
pub type PrepareFuture = Pin<Box<dyn Future<Output = ExtractorResult<CommandFuture>> + Send>>;

pub type CommandPtr = fn(CommandContext, Args) -> PrepareFuture;

#[derive(Debug)]
pub struct CommandArgDesc {
    pub name: &'static str,
    pub optional: fn() -> Option<Cow<'static, str>>,
    pub desc: fn() -> Cow<'static, str>,
}

#[derive(Debug)]
pub struct CommandMetadata {
    pub name: &'static str,
    pub doc: &'static str,
    pub args: &'static [CommandArgDesc],
    pub module_path: &'static str,
    pub line_number: u32,
    pub handler: CommandPtr,
    pub tags: &'static [CommandTag],
    /// Which minimum permission level is needed to use this command
    pub permission: PermissionLevel,
    /// A global timeout before the command can be used again
    pub global_gate: Option<Duration>,
    /// A timeout before the command can be used again by the same user
    pub sender_gate: Option<Duration>,
    /// An ultra-short version of the command
    pub shortcode: Option<&'static str>,
}

impl CommandMetadata {
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
    /// Whether the command is for controlling OBS
    OBSControl,
}

inventory::collect!(CommandMetadata);

pub fn find(name: &str) -> Option<&'static CommandMetadata> {
    static MAP: LazyLock<HashMap<&str, &'static CommandMetadata>> = LazyLock::new(|| {
        let mut shortcodes = HashSet::new();
        inventory::iter::<CommandMetadata>
            .into_iter()
            .flat_map(|reg| match reg.shortcode {
                Some(shortcode) => {
                    if !shortcodes.insert(shortcode) {
                        panic!("Duplicate shortcode: {shortcode}");
                    }
                    vec![(reg.name, reg), (shortcode, reg)]
                }
                None => vec![(reg.name, reg)],
            })
            .collect()
    });

    MAP.get(name).copied()
}

pub static MACRO: LazyLock<&'static CommandMetadata> = LazyLock::new(|| find("macro").unwrap());
