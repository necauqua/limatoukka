use proc_macro::TokenStream;
use quote::{quote, quote_spanned};
use syn::{Expr, FnArg, Lit, Pat, spanned::Spanned as _};

fn type_span(arg: &FnArg) -> proc_macro2::Span {
    match arg {
        FnArg::Receiver(receiver) => receiver.span(),
        FnArg::Typed(pat_type) => pat_type.ty.span(),
    }
}

#[proc_macro_attribute]
pub fn command(_attr: TokenStream, input: TokenStream) -> TokenStream {
    let mut input = syn::parse_macro_input!(input as syn::ItemFn);

    input.vis = syn::Visibility::Inherited;

    let ident = &input.sig.ident;
    let name = ident.to_string().replace('_', "-");
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
        });
    }
    .into()
}
