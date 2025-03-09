use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;
use tpn_bot::commands::CommandRegistration;

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
}

#[derive(Serialize)]
struct CategoryOut {
    name: String,
    commands: Vec<CommandOut>,
}

#[derive(Default, Serialize)]
struct DocOut {
    categories: Vec<CategoryOut>,
}

fn main() -> Result<()> {
    let mut categories = HashMap::new();

    for cmd in inventory::iter::<CommandRegistration> {
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
                            .map(|arg| CommandArgOut {
                                name: arg.name.to_owned(),
                                doc: (arg.desc)().into_owned(),
                                optional: arg.optional,
                            })
                            .collect::<Vec<_>>(),
                    )
                    .filter(|args| !args.is_empty()),
                })
                .collect(),
        });
    }

    println!("{}", serde_yml::to_string(&result)?);

    Ok(())
}
