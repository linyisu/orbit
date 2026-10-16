//! Compile typed tool methods into the existing object-safe execution contract.
use proc_macro::TokenStream;
use quote::quote;
use syn::{FnArg, ImplItemFn, LitStr, Pat, parse_macro_input, spanned::Spanned};

/// Generates `Tool::definition` and `Tool::execute` from a documented typed async method.
/// Apply inside `impl Tool`; keep capabilities and context-dependent dispatch explicit.
#[proc_macro_attribute]
pub fn tool(attribute: TokenStream, item: TokenStream) -> TokenStream {
    let mut name = None;
    let parser = syn::meta::parser(|meta| {
        if !meta.path.is_ident("name") || name.is_some() {
            return Err(meta.error("expected one name = \"tool_name\""));
        }
        name = Some(meta.value()?.parse::<LitStr>()?);
        Ok(())
    });
    parse_macro_input!(attribute with parser);
    let method = parse_macro_input!(item as ImplItemFn);
    expand(name, method)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand(name: Option<LitStr>, method: ImplItemFn) -> syn::Result<proc_macro2::TokenStream> {
    let signature = &method.sig;
    let error = |message: &str| syn::Error::new(signature.span(), message);
    let name = name.ok_or_else(|| error("missing tool name"))?;
    if name.value().is_empty()
        || signature.ident != "execute"
        || signature.asyncness.is_none()
        || signature.unsafety.is_some()
        || signature.constness.is_some()
        || signature.abi.is_some()
        || !signature.generics.params.is_empty()
        || signature
            .generics
            .where_clause
            .is_some()
        || signature.inputs.len() != 3
    {
        return Err(error(
            "expected async fn execute(&self, args: Arguments, cancellation: CancellationToken) -> Result<ToolOutput, ToolError>",
        ));
    }
    let FnArg::Receiver(receiver) = &signature.inputs[0] else {
        return Err(error("first argument must be &self"));
    };
    if receiver.reference.is_none()
        || receiver.mutability.is_some()
        || receiver.colon_token.is_some()
    {
        return Err(error("first argument must be &self"));
    }
    let FnArg::Typed(arguments) = &signature.inputs[1] else {
        return Err(error("expected named argument struct"));
    };
    let FnArg::Typed(cancellation) = &signature.inputs[2] else {
        return Err(error("expected cancellation token"));
    };
    if !matches!(&*arguments.pat, Pat::Ident(_)) || !matches!(&*cancellation.pat, Pat::Ident(_)) {
        return Err(error("tool parameters must be named bindings"));
    }
    let description = method
        .attrs
        .iter()
        .filter_map(|attribute| {
            if !attribute.path().is_ident("doc") {
                return None;
            }
            let syn::Meta::NameValue(value) = &attribute.meta else {
                return None;
            };
            let syn::Expr::Lit(value) = &value.value else {
                return None;
            };
            let syn::Lit::Str(value) = &value.lit else {
                return None;
            };
            Some(value.value().trim().to_owned())
        })
        .collect::<Vec<_>>()
        .join("\n");
    if description.trim().is_empty() {
        return Err(error(
            "document the tool method with /// before applying #[tool]",
        ));
    }
    if method
        .attrs
        .iter()
        .any(|attribute| !attribute.path().is_ident("doc"))
    {
        return Err(error(
            "place conditional attributes on the enclosing Tool implementation",
        ));
    }
    let argument_type = &arguments.ty;
    let binding = &arguments.pat;
    let body = &method.block;
    let syn::ReturnType::Type(_, output) = &signature.output else {
        return Err(error(
            "declare Result<ToolOutput, ToolError> as the return type",
        ));
    };
    Ok(quote! {
        fn definition(&self) -> ::orbit_agent::__private::ToolDefinition {
            ::orbit_agent::tool::typed_definition::<#argument_type>(#name, #description)
        }

        fn execute<'a>(
            &'a self,
            call: ::orbit_agent::__private::ToolCall,
            #cancellation,
        ) -> ::orbit_agent::tool::ToolFuture<'a> {
            Box::pin(async move {
                let #binding: #argument_type = ::orbit_agent::__private::from_str(
                    &call.function.arguments,
                ).map_err(|error| ::orbit_agent::tool::ToolError::InvalidArguments {
                    tool: #name.into(),
                    message: error.to_string(),
                })?;
                let result: #output = async move #body.await;
                result
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_signatures_and_missing_contracts_are_rejected() {
        for source in [
            "fn execute(&self, args: Args, token: Token) {}",
            "async fn execute(&mut self, args: Args, token: Token) {}",
            "async fn execute<T>(&self, args: T, token: Token) {}",
            "async fn execute(&self, args: Args, token: Token) {}",
        ] {
            let method = syn::parse_str(source).unwrap();
            assert!(expand(Some(syn::parse_quote!("generated")), method).is_err());
        }
    }
}
