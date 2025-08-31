use std::collections::HashMap;

use anyhow::Result;
use humantime_serde::re::humantime;
use serde::Serialize;
use strum::{EnumMessage, IntoEnumIterator};
use tpn_bot::{
    commands::{CommandTag, NativeCommand},
    services::{charges::Charges, messaging::PermissionLevel},
};

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

#[derive(Serialize)]
struct CommandArgOut {
    name: String,
    doc: String,
    optional: bool,
}

#[derive(Serialize)]
struct CommandOut {
    name: String,
    doc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<Vec<CommandArgOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    permission: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    global_macro_exempt: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    global_gate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sender_gate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shortcode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    free_for: Option<String>,
}

#[derive(Serialize)]
struct CategoryOut {
    name: String,
    commands: Vec<CommandOut>,
}

#[derive(Serialize)]
struct PermissionOut {
    name: &'static str,
    description: &'static str,
    level: u8,
}

#[derive(Default, Serialize)]
struct DocOut {
    categories: Vec<CategoryOut>,
    permissions: Vec<PermissionOut>,
}

fn main() -> Result<()> {
    let mut categories = HashMap::new();

    for cmd in inventory::iter::<NativeCommand> {
        if cmd.is(CommandTag::Hidden) || cmd.is(CommandTag::NoitaControl) {
            continue;
        }
        let category = cmd.module_path.rsplit_once("::").unwrap().1.to_owned();
        categories
            .entry(category)
            .or_insert_with(Vec::new)
            .push(cmd);
    }

    let mut categories = categories.into_iter().collect::<Vec<_>>();
    categories.sort_by(|a, b| a.0.cmp(&b.0));

    let mut result = DocOut::default();

    for (category, mut commands) in categories {
        commands.sort_by_key(|cmd| cmd.line_number);

        result.categories.push(CategoryOut {
            name: capitalise(&category),
            commands: commands
                .into_iter()
                .map(|cmd| CommandOut {
                    name: cmd.name.to_owned(),
                    doc: cmd.doc.to_owned(),
                    args: Some(
                        cmd.args
                            .iter()
                            .map(|arg| {
                                let mut doc = (arg.desc)().into_owned();
                                let opt = (arg.optional)();
                                if let Some(opt) = opt.as_deref() {
                                    doc.push_str(&format!(", {opt}"));
                                }
                                CommandArgOut {
                                    name: arg.name.replace("_", "-"),
                                    doc,
                                    optional: opt.is_some(),
                                }
                            })
                            .collect::<Vec<_>>(),
                    )
                    .filter(|args| !args.is_empty()),
                    permission: match cmd.permission {
                        PermissionLevel::Viewer => None,
                        _ => Some(<&'static str>::from(cmd.permission).into()),
                    },
                    global_macro_exempt: if cmd.is(CommandTag::GlobalMacroExempt) {
                        Some(true)
                    } else {
                        None
                    },
                    global_gate: cmd
                        .global_gate
                        .map(|d| format!("{}", humantime::format_duration(d))),
                    sender_gate: cmd
                        .sender_gate
                        .map(|d| format!("{}", humantime::format_duration(d))),
                    shortcode: cmd.shortcode.map(|s| s.into()),
                    cost: cmd.cost.map(|c| Charges::from(c).to_string()),
                    free_for: match cmd.free_for {
                        PermissionLevel::Caster => None,
                        _ => Some(<&'static str>::from(cmd.free_for).into()),
                    },
                })
                .collect(),
        });
    }

    for perm in PermissionLevel::iter() {
        result.permissions.push(PermissionOut {
            name: perm.into(),
            description: perm.get_documentation().unwrap_or_default(),
            level: perm as _,
        });
    }

    println!("{}", serde_yml::to_string(&result)?);

    Ok(())
}
