use maud::{Markup, PreEscaped, html};
use neca_cmd::{CommandMessage, sub::Arg};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn tokenize(input: String) -> Result<String, JsValue> {
    Ok(tokenize_impl(parse(&input)).0)
}

fn parse(input: &str) -> CommandMessage {
    let mut msg = CommandMessage::parse(input);
    for seq in &mut msg.parallel {
        for cmd in seq {
            if let Some((name, n)) = cmd.name.split_inline_number() {
                cmd.args.push_front(Arg::simple(n.to_owned()));
                cmd.name.name = name.into();
            };
        }
    }
    msg
}

fn tokenize_impl(input: CommandMessage) -> Markup {
    if input.is_empty() {
        return html!(span.none { "none" });
    }
    html! {
        div.parallel {
            @for (i, seq) in input.parallel.into_iter().enumerate() {
                div.seq {
                    @for (j, cmd) in seq.into_iter().enumerate() {
                        div.cmd {
                            span.pos {
                                "("(i) "," (j) ") "
                            }
                            span.name {
                                (cmd.name)
                            }
                            div.args {
                                @for (k, arg) in cmd.args.into_iter().enumerate() {
                                    div.arg {
                                        span { (k + 1)": " }
                                        @let rendered = render_arg(&arg);
                                        @let parsed_arg = parse(arg.text());
                                        @if parsed_arg.is_empty() {
                                            span { "\"" span.lit { (rendered) } "\"" }
                                        } @else {
                                            details {
                                                summary { "\"" span.lit { (rendered) } "\"" }
                                                (tokenize_impl(parsed_arg))
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn render_arg(arg: &Arg) -> Markup {
    // meh
    PreEscaped(arg.expand(&mut |name| Some(html! {
        span.sub {
            @if name.parse::<u32>().ok().is_some_and(|n| n != 0) {
                abbr title=(format!("This will be replaced verbatim with macro parameter #{name}")) { "%" (name) }
            } @else {
                abbr title=(format!("This will be replaced verbatim with the contents of variable `{name}`")) { "%" (name) }
            }
        }
    }.0)))
}
