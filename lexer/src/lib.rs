use maud::{Markup, html};
use neca_cmd::{CommandMessage, CommandType};
use std::fmt::Display;
use wasm_bindgen::prelude::*;

struct Name(String, CommandType);

impl Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.1.write_command(f, &self.0)
    }
}

#[wasm_bindgen]
pub fn tokenize(input: String) -> Result<String, JsValue> {
    Ok(tokenize_impl(parse(&input)).0)
}

fn parse(input: &str) -> CommandMessage {
    let mut msg = CommandMessage::parse(input);
    for seq in &mut msg.parallel {
        for cmd in seq {
            lazy_regex::regex_if!(r#"^(?<name>.+?)(?<n>\d+s?)$"#, &cmd.name, {
                cmd.args.push_front(n.to_owned());
                cmd.name = name.into();
            });
        }
    }
    msg
}

fn tokenize_impl(input: CommandMessage) -> Markup {
    if input.is_empty() {
        return html!(span.none { "none" });
    }
    html! {
        @for (i, seq) in input.parallel.into_iter().enumerate() {
            div.seq {
                @for (j, cmd) in seq.into_iter().enumerate() {
                    div.cmd {
                        span.pos {
                            "("(i) "," (j) ") "
                        }
                        span.name {
                            (Name(cmd.name, cmd.tpe))
                        }
                        div.args {
                            @for (k, arg) in cmd.args.into_iter().enumerate() {
                                div.arg {
                                    span { (k + 1)": " }
                                    @let parsed_arg = parse(&arg);
                                    @if parsed_arg.is_empty() {
                                        span { "\"" span.lit { (arg) } "\"" }
                                    } @else {
                                        details {
                                            summary { "\"" span.lit { (arg) } "\"" }
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

// // Called by our JS entry point to run the example
// #[wasm_bindgen(start)]
// fn run() -> Result<(), JsValue> {
//     // Use `web_sys`'s global `window` function to get a handle on the global
//     // window object.
//     let window = web_sys::window().expect("no global `window` exists");
//     let document = window.document().expect("should have a document on window");
//     let body = document.body().expect("document should have a body");

//     // Manufacture the element we're gonna append
//     let val = document.create_element("p")?;
//     val.set_text_content(Some("Hello from Rust!"));

//     body.append_child(&val)?;

//     Ok(())
// }
