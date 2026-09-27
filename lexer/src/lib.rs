use maud::{Markup, PreEscaped, html};
use neca_cmd::{Statement, param::Param};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn tokenize(input: String) -> Result<String, JsValue> {
    Ok(tokenize_impl(parse(&input)).0)
}

fn parse(input: &str) -> Statement {
    let mut stmt = Statement::parse(input);
    for seq in &mut stmt.parallel {
        for cmd in seq {
            if let Some((name, n)) = cmd.token.split_inline_number() {
                let (name, n) = (name.into(), n.to_owned());
                cmd.params.push_front(Param::simple(n));
                cmd.token.name = name;
            };
        }
    }
    stmt
}

fn tokenize_impl(input: Statement) -> Markup {
    if input.is_noop() {
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
                                (cmd.token)
                            }
                            div.args {
                                @for (k, param) in cmd.params.into_iter().enumerate() {
                                    div.arg {
                                        span { (k + 1)": " }
                                        @let rendered = render_param(&param);
                                        @let parsed_param = parse(param.text());
                                        @if parsed_param.is_noop() {
                                            span { "\"" span.lit { (rendered) } "\"" }
                                        } @else {
                                            details {
                                                summary { "\"" span.lit { (rendered) } "\"" }
                                                (tokenize_impl(parsed_param))
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

fn render_param(param: &Param) -> Markup {
    // meh
    PreEscaped(param.expand(|name| Some(html! {
        span.sub {
            @if name.parse::<u32>().ok().is_some_and(|n| n != 0) {
                abbr title=(format!("This will be replaced verbatim with macro parameter #{name}")) { "%" (name) }
            } @else {
                abbr title=(format!("This will be replaced verbatim with the contents of variable `{name}`")) { "%" (name) }
            }
        }
    }.0)).into())
}
