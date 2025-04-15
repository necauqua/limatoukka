use std::{
    collections::VecDeque,
    fmt::{self, Write},
};

#[derive(Debug, Clone, Copy)]
pub enum CommandType {
    Uwu,
    UwuMeh,
    Crusade,
    Normie,
    Neither,
}

impl CommandType {
    pub fn write_command(&self, f: &mut fmt::Formatter<'_>, command: &str) -> fmt::Result {
        match self {
            Self::Uwu => {
                f.write_str(command)?;
                f.write_char('~')
            }
            Self::UwuMeh => {
                f.write_char('~')?;
                f.write_str(command)
            }
            Self::Crusade => {
                f.write_char('+')?;
                f.write_str(command)
            }
            Self::Normie => {
                f.write_char('!')?;
                f.write_str(command)
            }
            Self::Neither => f.write_str(command),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommandExpr {
    pub name: String,
    pub args: VecDeque<String>,
    pub tpe: CommandType,
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
        let (word, tpe) = (word.strip_suffix('~').map(|w| (w, CommandType::Uwu)))
            .or(word.strip_prefix('~').map(|w| (w, CommandType::UwuMeh)))
            .or(word.strip_prefix('+').map(|w| (w, CommandType::Crusade)))
            .or(word.strip_prefix('!').map(|w| (w, CommandType::Normie)))
            .unwrap_or((word, CommandType::Neither));

        let mut parts = split_balanced(word, &[':']).into_iter();
        let mut name = parts.next().unwrap();

        if name.is_empty() || name.starts_with('-') || name.ends_with('-') {
            return None;
        }

        let mut args: VecDeque<_> = parts.map(|s| unwrap_string_literals(&s)).collect();

        if (matches!(tpe, CommandType::Neither) && args.is_empty())
            || args.iter().any(|arg| arg.is_empty())
        {
            return None;
        }

        if let Some((_, prefix, number)) = lazy_regex::regex_captures!(r"^(.*?)(\d+s?)$", &name) {
            args.push_front(number.to_owned());
            name = prefix.to_owned();
        }

        Some(Self { name, args, tpe })
    }
}

#[derive(Debug, Clone)]
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

impl CommandMessage {
    pub fn parse(content: &str) -> Self {
        Self {
            parallel: split_balanced(content.trim(), &['|', '/']) // allow / for mobile
                .into_iter()
                .map(|group| {
                    split_balanced(&group, &[' '])
                        .iter()
                        .filter_map(|s| CommandExpr::parse(s))
                        .collect()
                })
                .filter(|g: &Vec<_>| !g.is_empty())
                .collect::<Vec<_>>(),
        }
    }
}

fn unwrap_string_literals(input: &str) -> String {
    let stripped = match input.strip_prefix('"') {
        Some(tail) => tail.strip_suffix('"'),
        None => input
            .strip_prefix('{')
            .and_then(|tail| tail.strip_suffix('}'))
            .map(|s| s.trim()),
    };
    let Some(input) = stripped else {
        return input.to_owned();
    };

    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some('n') => result.push('\n'),
                // Some('r') => result.push('\r'), // SSE does not support \r
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
fn split_balanced(input: &str, seps: &[char]) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut brace_depth = 0;
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if (in_string || brace_depth != 0) && ch == '\\' {
            if let Some(ch) = chars.next() {
                current.push('\\');
                current.push(ch);
            }
            continue;
        }
        match ch {
            '"' if brace_depth == 0 => in_string = !in_string,
            '{' if !in_string => brace_depth += 1,
            '}' if !in_string => brace_depth -= 1,
            _ => {}
        }
        if !in_string && brace_depth == 0 && seps.contains(&ch) {
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

        insta::assert_snapshot!(parsed, @r#"left~ right:".3"~ mouse:123:321~ | test~ test:2~ nope~"#); // no test2 hah
    }

    #[test]
    fn cringing() {
        let message = "Hello, this is +left and +right:.3 and +mouse:123:321 | and then +test +test2 and +wut~";
        let parsed = CommandMessage::parse(message);

        insta::assert_snapshot!(parsed, @r#"left~ right:".3"~ mouse:123:321~ | test~ test:2~ +wut~"#);
    }

    #[test]
    fn normieing() {
        let message = "Hello, this is !left and !right:.3 and !mouse:123:321 | and then !test !test2 and !wut~";
        let parsed = CommandMessage::parse(message);

        insta::assert_snapshot!(parsed, @r#"left~ right:".3"~ mouse:123:321~ | test~ test:2~ !wut~"#);
    }

    #[test]
    fn neithering() {
        let message =
            "Hello, this is left and right:.3 and mouse:123:321 | and then test test2 and wut";
        let parsed = CommandMessage::parse(message);

        insta::assert_snapshot!(parsed, @r#"right:".3"~ mouse:123:321~"#);
    }

    #[test]
    fn strings() {
        let message = r#"print:"hello space"~ +hah:"and | pipe" | ~nope:123 | and-also-escapes:" \"incredible\", lol"~ "#;

        let parsed = CommandMessage::parse(message);

        insta::assert_snapshot!(parsed, @r#"print:"hello space"~ hah:"and | pipe"~ | nope:123~ | and-also-escapes:" \"incredible\", lol"~"#);
    }

    #[test]
    fn braces() {
        let message = r#"print:{ hello space }~ +hah:{ and | pipe } | ~nope:123 | and-also-escapes:{  { incredible }, lol }~ "#;

        let parsed = CommandMessage::parse(message);

        insta::assert_snapshot!(parsed, @r#"print:"hello space"~ hah:"and | pipe"~ | nope:123~ | and-also-escapes:"{ incredible }, lol"~"#);
    }

    #[test]
    fn number_arg() {
        let message = r#"test123~ wait123s~ nope432h~"#;

        let parsed = CommandMessage::parse(message);

        insta::assert_snapshot!(parsed, @"test:123~ wait:123s~ nope432h~");
    }
}
