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
    hidden: Option<MacroArg>,
    shortcode: Option<MacroArg>,
    no_wall: Option<MacroArg>,
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
                "hidden" => args.hidden.replace(arg),
                "shortcode" => args.shortcode.replace(arg),
                "no_wall" => args.no_wall.replace(arg),
                _ => return Err(syn::Error::new(arg.name.span(), "Unknown argument")),
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
            match permission.value {
                None => {
                    quote_spanned!(name.span() => #name: compile_error!("missing permission value"))
                }
                Some(value) => quote!(#name: crate::services::messaging::PermissionLevel::#value),
            }
        }
        None => quote!(permission: crate::services::messaging::PermissionLevel::Viewer),
    };
    let global_gate = match attrs.global_gate {
        None => quote!(global_gate: None),
        Some(global_gate) => {
            let name = global_gate.name;
            match global_gate.value {
                None => {
                    quote_spanned!(name.span() => #name: compile_error!("missing global gate value"))
                }
                Some(value) => {
                    let d = parse_duration(&value);
                    quote!(#name: Some(#d))
                }
            }
        }
    };
    let sender_gate = match attrs.sender_gate {
        None => quote!(sender_gate: None),
        Some(sender_gate) => {
            let name = sender_gate.name;
            match sender_gate.value {
                None => {
                    quote_spanned!(name.span() => #name: compile_error!("missing sender gate value"))
                }
                Some(value) => {
                    let d = parse_duration(&value);
                    quote!(#name: Some(#d))
                }
            }
        }
    };
    let hidden = match attrs.hidden {
        Some(hidden) => {
            let name = hidden.name;
            if let Some(value) = hidden.value {
                quote_spanned!(value.span() => #name: compile_error!("`hidden` attribute does not take a value"))
            } else {
                quote!(#name: true)
            }
        }
        None => quote!(hidden: false),
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
    let no_wall = match attrs.no_wall {
        Some(no_wall) => {
            let name = no_wall.name;
            if let Some(value) = no_wall.value {
                quote_spanned!(value.span() => #name: compile_error!("`no_wall` attribute does not take a value"))
            } else {
                quote!(#name: true)
            }
        }
        None => quote!(no_wall: false),
    };

    quote! {
        #input

        ::inventory::submit!(crate::commands::CommandRegistration {
            name: #name,
            doc: #doc,
            args: &[#doc_args],
            module_path: module_path!(),
            line_number: line!(),
            handler: |ctx, mut args| Box::pin(async move {
                #arg_defs
                let idx = args.current_idx();
                if let Some(arg) = args.pop() {
                    return Err(crate::commands::args::ExtractorError::UnexpectedArgument(idx, arg));
                }
                Ok(Box::pin(async {
                    #arg_gets
                    #ident(#args).await
                }) as crate::commands::CommandFuture)
            }),
            #permission,
            #global_gate,
            #sender_gate,
            #hidden,
            #shortcode,
            #no_wall,
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
