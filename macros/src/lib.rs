use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{ToTokens, quote, quote_spanned};
use syn::{
    Expr, FnArg, Ident, Lit, Pat, parse::Parse, punctuated::Punctuated, spanned::Spanned as _,
};

fn type_span(arg: &FnArg) -> proc_macro2::Span {
    match arg {
        FnArg::Receiver(receiver) => receiver.span(),
        FnArg::Typed(pat_type) => pat_type.ty.span(),
    }
}

struct MacroArg {
    name: syn::Ident,
    value: Option<syn::Expr>,
}

impl Parse for MacroArg {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let name = input.parse()?;
        if input.peek(syn::Token![,]) || input.is_empty() {
            return Ok(Self { name, value: None });
        }
        _ = input.parse::<syn::Token![=]>()?;
        Ok(Self {
            name,
            value: Some(input.parse()?),
        })
    }
}

#[derive(Default)]
struct CommandMacroAttrs {
    permission: Option<MacroArg>,
    global_gate: Option<MacroArg>,
    sender_gate: Option<MacroArg>,
    shortcode: Option<MacroArg>,
    cost: Option<MacroArg>,
    free_for: Option<MacroArg>,
    free_if: Option<MacroArg>,
    extras: Vec<MacroArg>,
}

impl Parse for CommandMacroAttrs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut args = Self::default();
        for arg in Punctuated::<MacroArg, syn::Token![,]>::parse_terminated(input)? {
            let name = arg.name.span();
            let prev = match arg.name.to_string().as_str() {
                "permission" => args.permission.replace(arg),
                "global_gate" => args.global_gate.replace(arg),
                "sender_gate" => args.sender_gate.replace(arg),
                "shortcode" => args.shortcode.replace(arg),
                "cost" => args.cost.replace(arg),
                "free_for" => args.free_for.replace(arg),
                "free_if" => args.free_if.replace(arg),
                _ => {
                    let prev = args.extras.iter().position(|a| a.name == arg.name);
                    args.extras.push(arg);
                    prev.map(|i| args.extras.swap_remove(i))
                }
            };
            if let Some(prev) = prev {
                return Err(syn::Error::new(
                    name.join(prev.name.span()).unwrap_or(name),
                    "Duplicate argument",
                ));
            }
        }
        Ok(args)
    }
}

#[proc_macro_attribute]
pub fn command(attrs: TokenStream, input: TokenStream) -> TokenStream {
    let mut input = syn::parse_macro_input!(input as syn::ItemFn);
    let attrs = syn::parse_macro_input!(attrs as CommandMacroAttrs);

    input.vis = syn::Visibility::Inherited;

    let ident = &input.sig.ident;
    let name = ident.to_string().replace('_', "-");
    let name = name.strip_prefix("r#").unwrap_or(&name);
    let doc = input
        .attrs
        .iter()
        .filter_map(|attr| {
            if attr.path().is_ident("doc") {
                match &attr.meta.require_name_value().ok()?.value {
                    Expr::Lit(syn::ExprLit {
                        lit: Lit::Str(doc), ..
                    }) => Some(doc.value()),
                    _ => None,
                }
            } else {
                None
            }
        })
        .fold(String::new(), |acc, doc| acc + doc.trim() + "\n");

    let mut inputs = input.sig.inputs.iter();
    let (arg_defs, arg_gets, args, doc_args) = match inputs.next() {
        None => (quote!(), quote!(), quote!(), quote!()),
        Some(first) => {
            // even making the errors pretty lol
            let ctx = quote_spanned!(type_span(first) => ctx);

            let mut arg_defs = Vec::new();
            let mut arg_gets = Vec::new();
            let mut args = Vec::new();
            let mut doc_args = Vec::new();

            for (i, arg) in inputs.enumerate() {
                let (pat, ty) = match arg {
                    FnArg::Receiver(_) => panic!("receiver?"),
                    FnArg::Typed(pt) => (&*pt.pat, &pt.ty),
                };

                let name = match pat {
                    Pat::Ident(ident) => ident.ident.to_string(),
                    _ => {
                        return quote_spanned!(pat.span() => compile_error!("a pattern, lmao?"))
                            .into();
                    }
                };
                let name = name.strip_prefix("r#").unwrap_or(&name);

                let ident = Ident::new(&format!("arg_{i}"), Span::call_site());

                arg_defs.push(quote_spanned! { type_span(arg) =>
                    let #ident = <#ty as crate::commands::args::ArgExtractor>::extract(&ctx, &mut args).await?;
                });
                arg_gets.push(quote_spanned! { type_span(arg) =>
                    let #ident = #ident.get(&ctx).await
                        .map_err(|e| crate::commands::args::ExtractorError::BadArgument(#i, e))?;
                });

                args.push(quote_spanned!(type_span(arg) => #ident));

                doc_args.push(quote!(crate::commands::CommandArgDesc {
                    name: #name,
                    optional: || <#ty as crate::commands::args::ArgExtractor>::optional_desc(),
                    desc: || <#ty as crate::commands::args::ArgExtractor>::type_desc(),
                }));
            }

            (
                quote!(#(#arg_defs)*),
                quote!(#(#arg_gets)*),
                quote!(#ctx, #(#args),*),
                quote!(#(#doc_args),*),
            )
        }
    };

    // meh all this is a mouthful but ehhhh
    let permission = match attrs.permission {
        Some(permission) => {
            let name = permission.name;
            if let Some(value) = permission.value {
                quote!(#name: crate::services::messaging::PermissionLevel::#value)
            } else {
                quote_spanned!(name.span() => #name: compile_error!("missing permission value"))
            }
        }
        None => quote!(permission: crate::services::messaging::PermissionLevel::Viewer),
    };
    let global_gate = match attrs.global_gate {
        None => quote!(global_gate: None),
        Some(global_gate) => {
            let name = global_gate.name;
            if let Some(value) = global_gate.value {
                let d = parse_duration(&value);
                quote!(#name: Some(#d))
            } else {
                quote_spanned!(name.span() => #name: compile_error!("missing global gate value"))
            }
        }
    };
    let sender_gate = match attrs.sender_gate {
        None => quote!(sender_gate: None),
        Some(sender_gate) => {
            let name = sender_gate.name;
            if let Some(value) = sender_gate.value {
                let d = parse_duration(&value);
                quote!(#name: Some(#d))
            } else {
                quote_spanned!(name.span() => #name: compile_error!("missing sender gate value"))
            }
        }
    };
    let shortcode = match attrs.shortcode {
        Some(shortcode) => {
            let name = shortcode.name;
            if let Some(value) = shortcode.value {
                let value = value.to_token_stream().to_string();
                quote!(#name: Some(#value))
            } else {
                quote_spanned!(name.span() => #name: compile_error!("missing shortcode value"))
            }
        }
        None => quote!(shortcode: None),
    };
    let cost = match attrs.cost {
        Some(cost) => {
            let name = cost.name;
            if let Some(value) = cost.value {
                let charges = parse_charges(&value);
                quote!(#name: Some(#charges))
            } else {
                quote_spanned!(name.span() => #name: compile_error!("missing cost value"))
            }
        }
        None => quote!(cost: None),
    };
    let free_for = match attrs.free_for {
        Some(free_for) => {
            let name = free_for.name;
            if let Some(value) = free_for.value {
                quote!(#name: crate::services::messaging::PermissionLevel::#value)
            } else {
                quote_spanned!(name.span() => #name: compile_error!("missing free_for value"))
            }
        }
        None => quote!(free_for: crate::services::messaging::PermissionLevel::Caster),
    };
    // an `async fn(&CommandContext) -> anyhow::Result<bool>`
    let free_if = match attrs.free_if {
        Some(free_if) => {
            let name = free_if.name;
            if let Some(value) = free_if.value {
                quote!(#name: Some(|ctx| ::std::boxed::Box::pin((#value)(ctx))))
            } else {
                quote_spanned!(name.span() => #name: compile_error!("missing free_if value"))
            }
        }
        None => quote!(free_if: None),
    };

    let tags = attrs.extras.into_iter().map(|attr| {
        if let Some(value) = attr.value {
            let name = attr.name;
            quote_spanned!(value.span() => #name: compile_error!("`{name}` attribute does not take a value"))
        } else {
            let name = attr.name;
            quote!(crate::commands::CommandTag::#name)
        }
    });

    quote! {
        #input

        ::inventory::submit!(crate::commands::NativeCommand {
            name: #name,
            doc: #doc,
            args: &[#doc_args],
            module_path: module_path!(),
            line_number: line!(),
            action: |ctx, mut args| Box::pin(async move {
                #arg_defs
                // extraneous arguments are ignored, the same as with macros;
                // this also keeps `mut args` used for commands without arguments
                let _ = &mut args;
                #arg_gets
                #ident(#args).await
            }),
            tags: &[#(#tags),*],
            #permission,
            #global_gate,
            #sender_gate,
            #shortcode,
            #cost,
            #free_for,
            #free_if,
        });
    }
    .into()
}

fn parse_duration(input: &syn::Expr) -> proc_macro2::TokenStream {
    match humantime::parse_duration(&input.to_token_stream().to_string()) {
        Ok(d) => {
            let s = d.as_secs();
            let n = d.subsec_nanos();
            quote!(::std::time::Duration::new(#s, #n))
        }
        Err(e) => {
            let e = e.to_string();
            quote_spanned!(input.span() => compile_error!(#e))
        }
    }
}

// todo should somehow reuse Charges impl here
fn parse_charges(input: &syn::Expr) -> proc_macro2::TokenStream {
    let input = input.to_token_stream().to_string().replace(' ', "");
    let mut parts = input.splitn(2, '.');
    let whole = parts.next().unwrap();
    let (neg, whole) = match whole.strip_prefix("-") {
        Some(whole) => (true, whole),
        None => (false, whole),
    };
    let fraction = parts.next().unwrap_or("0");
    let Ok(whole) = whole.parse::<i64>() else {
        return quote_spanned!(input.span() => compile_error!("cost must be a number"));
    };
    let fraction: i64 = match (fraction.len(), fraction.parse()) {
        (1, Ok(n)) => n * 100,
        (2, Ok(n)) => n * 10,
        (3, Ok(n)) => n,
        (_, Err(_)) => {
            return quote_spanned!(input.span() => compile_error!("cost must be a number"));
        }
        _ => {
            return quote_spanned!(input.span() => compile_error!("cost can have at most 3 decimal places"));
        }
    };
    let total = if neg { -1 } else { 1 } * (whole * 1000 + fraction);
    quote!(#total)
}
