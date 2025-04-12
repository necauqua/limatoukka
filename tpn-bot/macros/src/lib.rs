use proc_macro::TokenStream;
use quote::{ToTokens, quote, quote_spanned};
use syn::{Expr, FnArg, Lit, Pat, parse::Parse, punctuated::Punctuated, spanned::Spanned as _};

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
    let (args, doc_args) = match inputs.next() {
        None => (quote!(), quote!()),
        Some(first) => {
            // even making the errors pretty lol
            let ctx = quote_spanned!(type_span(first) => ctx);
            let (args, doc_args): (Vec<_>, Vec<_>) = inputs
                .map(|arg| {
                    let (pat, ty) = match arg {
                        FnArg::Receiver(_) => panic!("receiver?"),
                        FnArg::Typed(pt) => (&*pt.pat, &pt.ty),
                    };
                    let name = match pat {
                        Pat::Ident(ident) => ident.ident.to_string(),
                        _ => panic!("pattern?"),
                    };
                    let arg = quote_spanned!(type_span(arg) => args.extract()?);
                    let doc_arg = quote!(crate::commands::CommandArgDesc {
                        name: #name,
                        optional: <#ty as crate::commands::args::ArgExtractor>::OPTIONAL,
                        desc: || <#ty as crate::commands::args::ArgExtractor>::type_desc(),
                    });
                    (arg, doc_arg)
                })
                .unzip();

            (quote!(#ctx, #(#args),*), quote!(#(#doc_args),*))
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

    quote! {
        #input

        ::inventory::submit!(crate::commands::CommandRegistration {
            name: #name,
            doc: #doc,
            args: &[#doc_args],
            module_path: module_path!(),
            line_number: line!(),
            handler: |ctx, mut args| {
                let fut = #ident(#args);
                if let Some((idx, arg)) = args.pop() {
                    return Err(crate::commands::args::ExtractorError::UnexpectedArgument(idx, arg));
                }
                Ok(Box::pin(fut))
            },
            #permission,
            #global_gate,
            #sender_gate,
            #hidden,
            #shortcode,
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
