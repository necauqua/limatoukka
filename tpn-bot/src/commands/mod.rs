use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::LazyLock,
    time::Duration,
};

use anyhow::Result;
use args::{Args, ExtractorResult};

pub mod args;
pub mod runner;

pub use tpn_bot_macros::command;

use crate::{context::cmd::CommandContext, services::messaging::PermissionLevel};

pub type CommandFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
pub type PrepareFuture = Pin<Box<dyn Future<Output = ExtractorResult<CommandFuture>> + Send>>;

pub type CommandPtr = fn(CommandContext, Args) -> PrepareFuture;

#[derive(Debug)]
pub struct CommandArgDesc {
    pub name: &'static str,
    pub optional: fn() -> Option<Cow<'static, str>>,
    pub desc: fn() -> Cow<'static, str>,
}

#[derive(Debug)]
pub struct CommandRegistration {
    pub name: &'static str,
    pub doc: &'static str,
    pub args: &'static [CommandArgDesc],
    pub module_path: &'static str,
    pub line_number: u32,
    pub handler: CommandPtr,
    /// Which minimum permission level is needed to use this command
    pub permission: PermissionLevel,
    /// A global timeout before the command can be used again
    pub global_gate: Option<Duration>,
    /// A timeout before the command can be used again by the same user
    pub sender_gate: Option<Duration>,
    /// Whether the command should not be shown in documentation
    pub hidden: bool,
    /// An ultra-short version of the command
    pub shortcode: Option<&'static str>,
    /// Whether the command should not be shown on the wall
    pub no_wall: bool,
}

inventory::collect!(CommandRegistration);

pub fn find(name: &str) -> Option<&'static CommandRegistration> {
    static MAP: LazyLock<HashMap<&str, &'static CommandRegistration>> = LazyLock::new(|| {
        let mut shortcodes = HashSet::new();
        inventory::iter::<CommandRegistration>
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

pub static MACRO: LazyLock<&'static CommandRegistration> = LazyLock::new(|| find("macro").unwrap());
