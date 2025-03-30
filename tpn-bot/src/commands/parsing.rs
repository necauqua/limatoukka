use std::{collections::VecDeque, fmt};

#[derive(Debug, Clone, Copy)]
pub enum CommandType {
    Uwu,
    Crusade,
}

#[derive(Debug)]
pub struct CommandExpr {
    pub name: String,
    pub args: VecDeque<String>,
    pub rest: Option<String>,
    pub tpe: CommandType,
}

impl CommandExpr {
    pub fn as_command(&self) -> String {
        match self.tpe {
            CommandType::Uwu => format!("{}~", self.name),
            CommandType::Crusade => format!("+{}", self.name),
        }
    }
}

impl fmt::Display for CommandExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        for arg in &self.args {
            f.write_str(":")?;
            if arg.chars().all(|ch| ch.is_alphanumeric()) {
                f.write_str(arg)?;
            } else {
                write!(f, "{arg:?}")?;
            }
        }
        f.write_str("~")?;
        Ok(())
    }
}

impl CommandExpr {
    pub fn parse(word: &str) -> Option<Self> {
        let (word, tpe) = word
            .strip_suffix('~')
            .map(|w| (w, CommandType::Uwu))
            .or(word.strip_prefix('+').map(|w| (w, CommandType::Crusade)))?;

        let mut parts = split_balanced(word, ':').into_iter();
        let name = parts.next().unwrap();
        if !is_good_command_name(&name) {
            return None;
        }
        Some(Self {
            name,
            args: parts.map(|s| unwrap_string_literals(&s)).collect(),
            tpe,
            rest: None,
        })
    }
}

#[derive(Debug)]
pub struct CommandMessage {
    pub parallel: Vec<Vec<CommandExpr>>,
}

impl fmt::Display for CommandMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, group) in self.parallel.iter().enumerate() {
            if i > 0 {
                f.write_str(" | ")?;
            }
            for (j, command) in group.iter().enumerate() {
                if j > 0 {
                    f.write_str(" ")?;
                }
                write!(f, "{command}")?;
            }
        }
        Ok(())
    }
}

pub fn is_good_command_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name.chars().all(|ch| ch.is_ascii_lowercase() || ch == '-')
}

impl CommandMessage {
    pub fn parse(content: &str) -> Self {
        let content = content.trim();

        let parallel = split_balanced(&content, '|');
        let single = parallel.len() == 1;

        Self {
            parallel: parallel
                .into_iter()
                .map(|group| {
                    let words = split_balanced(&group, ' ');

                    // meh
                    if single {
                        let mut words_iter = words.iter();
                        let word = words_iter.next().unwrap();
                        if let Some(mut token) = CommandExpr::parse(&word) {
                            if token.args.is_empty() && words_iter.next().is_some() {
                                // ideally args will be a enum of args|rest, but that requires rewriting a lot of things, eh
                                token.rest = Some(content[word.len() + 1..].trim().to_owned());
                            }
                        }
                    }

                    words
                        .iter()
                        .filter_map(|word| {
                            word.strip_suffix('~')
                                .map(|w| (w, CommandType::Uwu))
                                .or(word.strip_prefix('+').map(|w| (w, CommandType::Crusade)))
                        })
                        .filter_map(|(word, tpe)| {
                            let mut parts = split_balanced(word, ':').into_iter();
                            let name = parts.next().unwrap();
                            if !is_good_command_name(&name) {
                                return None;
                            }
                            let cmd = CommandExpr {
                                name,
                                args: parts.map(|s| unwrap_string_literals(&s)).collect(),
                                tpe,
                                rest: None,
                            };
                            if cmd.args.iter().any(|arg| arg.is_empty()) {
                                return None;
                            }
                            Some(cmd)
                        })
                        .collect()
                })
                .filter(|g: &Vec<_>| !g.is_empty())
                .collect::<Vec<_>>(),
        }
    }
}

fn unwrap_string_literals(input: &str) -> String {
    let Some(input) = input
        .strip_prefix('"')
        .and_then(|input| input.strip_suffix('"'))
    else {
        return input.to_owned();
    };

    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some('n') => result.push('\n'),
                Some('r') => result.push('\r'),
                Some('t') => result.push('\t'),
                Some(ch) => result.push(ch),
                _ => (),
            },
            ch => result.push(ch),
        }
    }
    result
}

// split that considers "string literals"
fn split_balanced(input: &str, sep: char) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if in_string && ch == '\\' {
            if let Some(ch) = chars.next() {
                current.push('\\');
                current.push(ch);
            }
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
        }
        if !in_string && ch == sep {
            result.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    result.push(current);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        let message = "Hello, this is left~ and right:.3~ and mouse:123:321~ | and then test~ test2~ and ~nope";
        let parsed = CommandMessage::parse(message);

        insta::assert_snapshot!(parsed, @r#"left~ right:".3"~ mouse:123:321~ | test~"#); // no test2 hah
    }

    #[test]
    fn cringing() {
        let message = "Hello, this is +left and +right:.3 and +mouse:123:321 | and then +test +test2 and +wut~";
        let parsed = CommandMessage::parse(message);

        insta::assert_snapshot!(parsed, @r#"left~ right:".3"~ mouse:123:321~ | test~"#);
    }

    #[test]
    fn strings() {
        let message = r#"print:"hello space"~ +hah:"and | pipe" | ~nope:123 | and-also-escapes:" \"incredible\", lol"~ "#;

        let parsed = CommandMessage::parse(message);

        insta::assert_snapshot!(parsed, @r#"print:"hello space"~ hah:"and | pipe"~ | and-also-escapes:" \"incredible\", lol"~"#);
    }
}
