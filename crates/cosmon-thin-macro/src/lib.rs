// SPDX-License-Identifier: Apache-2.0

//! `cosmon-thin-macro` — proc-macro layer for [`cosmon-thin-cli`].
//!
//! Exposes a single attribute, [`macro@verb`], which sits on a function and
//! emits two artefacts:
//!
//! 1. A request/response struct pair derived from the function signature
//!    (today: opaque placeholders; T-CST-V0 will wire real fields).
//! 2. An entry in a compile-time global registry (via the [`linkme`] crate)
//!    so that [`cosmon_thin_cli::registry::all`] can enumerate every verb
//!    the binary knows about, with its HTTP method, path, and principal.
//!
//! The macro deliberately performs **no** HTTP work, **no** JSON
//! manipulation, and **no** I/O. It is a metadata generator. Runtime
//! plumbing lives in [`cosmon-thin-cli`].
//!
//! # Why a separate crate?
//!
//! Rust requires `proc-macro = true` crates to be self-contained — they
//! cannot expose ordinary types or traits. Splitting `cosmon-thin-macro`
//! from `cosmon-thin-cli` is a Cargo constraint, not a design choice.
//!
//! # Why `compile_error!` over `panic!`?
//!
//! A malformed `#[verb(...)]` annotation is a *human* mistake, surfaced
//! during `cargo check`. `compile_error!` produces a structured rustc
//! diagnostic that names the verb in question; `panic!` collapses to
//! "proc-macro panicked" with no actionable context. The contract of the
//! foundation crate is: every error is a typed, named diagnostic.

#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input,
    punctuated::Punctuated,
    ItemFn, LitStr, Meta, Token,
};

/// Parsed `#[verb(method = "POST", path = "/v1/...", principal = "tenant")]` arguments.
struct VerbArgs {
    method: LitStr,
    path: LitStr,
    principal: LitStr,
}

impl Parse for VerbArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let metas = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
        let mut method: Option<LitStr> = None;
        let mut path: Option<LitStr> = None;
        let mut principal: Option<LitStr> = None;

        for meta in metas {
            let nv = match meta {
                Meta::NameValue(nv) => nv,
                other => {
                    return Err(syn::Error::new_spanned(
                        other,
                        "expected `key = \"value\"` in #[verb(...)]",
                    ));
                }
            };
            let ident = nv
                .path
                .get_ident()
                .ok_or_else(|| syn::Error::new_spanned(&nv.path, "expected identifier key"))?
                .to_string();

            let lit = match &nv.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) => s.clone(),
                other => {
                    return Err(syn::Error::new_spanned(
                        other,
                        "expected string literal value",
                    ));
                }
            };

            match ident.as_str() {
                "method" => method = Some(lit),
                "path" => path = Some(lit),
                "principal" => principal = Some(lit),
                other => {
                    return Err(syn::Error::new_spanned(
                        &nv.path,
                        format!(
                            "unknown #[verb] argument `{other}` (expected method, path, principal)"
                        ),
                    ));
                }
            }
        }

        let method = method.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "#[verb] missing required `method = \"...\"`",
            )
        })?;
        let path = path.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "#[verb] missing required `path = \"...\"`",
            )
        })?;
        let principal = principal.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "#[verb] missing required `principal = \"...\"`",
            )
        })?;

        Ok(VerbArgs {
            method,
            path,
            principal,
        })
    }
}

fn validate_method(verb_name: &str, method: &LitStr) -> Result<(), TokenStream2> {
    let v = method.value();
    let upper = v.to_ascii_uppercase();
    let allowed = ["GET", "POST", "PUT", "PATCH", "DELETE"];
    if !allowed.contains(&upper.as_str()) {
        let msg = format!(
            "#[verb] on `{verb_name}`: invalid method `{v}` (expected one of GET, POST, PUT, PATCH, DELETE)"
        );
        return Err(quote! { ::core::compile_error!(#msg); });
    }
    Ok(())
}

fn validate_principal(verb_name: &str, principal: &LitStr) -> Result<(), TokenStream2> {
    let v = principal.value();
    if !matches!(v.as_str(), "tenant" | "operator" | "worker") {
        let msg = format!(
            "#[verb] on `{verb_name}`: invalid principal `{v}` (expected tenant, operator, or worker)"
        );
        return Err(quote! { ::core::compile_error!(#msg); });
    }
    Ok(())
}

fn validate_path(verb_name: &str, path: &LitStr) -> Result<(), TokenStream2> {
    let v = path.value();
    if !v.starts_with('/') {
        let msg = format!("#[verb] on `{verb_name}`: path `{v}` must start with `/`");
        return Err(quote! { ::core::compile_error!(#msg); });
    }
    Ok(())
}

/// Annotate a function with HTTP verb metadata for `cs-thin`.
///
/// # Arguments
///
/// - `method` — HTTP method literal: `"GET"`, `"POST"`, `"PUT"`, `"PATCH"`, `"DELETE"`.
/// - `path` — URL path template, must start with `/`.
/// - `principal` — authorisation principal: `"tenant"`, `"operator"`, or `"worker"`.
///
/// # Generated artefacts
///
/// For a function `pub fn foo_bar(...) -> ...` annotated with `#[verb]`, the
/// macro emits:
///
/// - `pub struct FooBarVerb` — zero-sized marker that names the verb.
/// - `pub struct FooBarVerbBody` / `pub struct FooBarVerbResponse` —
///   placeholder request/response bodies (T-CST-V0 leaves them empty;
///   future tasks may swap them for caller-defined types via additional
///   macro arguments).
/// - `impl ::cosmon_thin_cli::IsoVerb for FooBarVerb` — wires the metadata.
/// - A `linkme` distributed-slice entry registering `VerbDescriptor` at link time.
///
/// The marker name `FooBarVerb` (instead of `FooBarRequest`) avoids
/// colliding with caller-defined `Request` types — for example,
/// `cosmon_state::ops::nucleate` already exports its own `NucleateRequest`,
/// and the macro must be annotatable on that function without a name fight.
///
/// The original function body is preserved unchanged.
///
/// # Errors
///
/// Malformed annotations produce `compile_error!` diagnostics that name the
/// verb. Missing arguments, unknown arguments, non-string values, and invalid
/// method/principal/path values are all caught at compile time.
#[proc_macro_attribute]
pub fn verb(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as VerbArgs);
    let func = parse_macro_input!(item as ItemFn);

    let fn_name = &func.sig.ident;
    let verb_name_str = fn_name.to_string();

    if let Err(err) = validate_method(&verb_name_str, &args.method) {
        return err.into();
    }
    if let Err(err) = validate_principal(&verb_name_str, &args.principal) {
        return err.into();
    }
    if let Err(err) = validate_path(&verb_name_str, &args.path) {
        return err.into();
    }

    let pascal = pascal_case(&verb_name_str);
    let verb_ty = format_ident!("{}Verb", pascal);
    let req_ty = format_ident!("{}VerbBody", pascal);
    let resp_ty = format_ident!("{}VerbResponse", pascal);
    let registry_ident = format_ident!("__COSMON_THIN_VERB_{}", verb_name_str.to_uppercase());

    let method_lit = &args.method;
    let path_lit = &args.path;
    let principal_lit = &args.principal;

    // `validate_principal` above already rejected anything outside
    // {tenant, operator, worker}, so the match below is exhaustive in practice.
    let principal_variant = match args.principal.value().as_str() {
        "operator" => quote!(::cosmon_thin_cli::Principal::Operator),
        "worker" => quote!(::cosmon_thin_cli::Principal::Worker),
        // Default arm covers "tenant" and the unreachable case identically.
        _ => quote!(::cosmon_thin_cli::Principal::Tenant),
    };

    let expanded = quote! {
        #func

        #[doc = concat!("Zero-sized marker for the `", stringify!(#fn_name), "` verb.")]
        #[derive(::core::fmt::Debug, ::core::clone::Clone, ::core::marker::Copy)]
        pub struct #verb_ty;

        #[doc = concat!("Placeholder request body for the `", stringify!(#fn_name), "` verb.")]
        #[derive(::core::fmt::Debug, ::core::clone::Clone, ::serde::Serialize, ::serde::Deserialize)]
        pub struct #req_ty {}

        #[doc = concat!("Placeholder response body for the `", stringify!(#fn_name), "` verb.")]
        #[derive(::core::fmt::Debug, ::core::clone::Clone, ::serde::Serialize, ::serde::Deserialize)]
        pub struct #resp_ty {}

        impl ::cosmon_thin_cli::IsoVerb for #verb_ty {
            const METHOD: &'static str = #method_lit;
            const PATH: &'static str = #path_lit;
            const PRINCIPAL: ::cosmon_thin_cli::Principal = #principal_variant;
            const VERB_NAME: &'static str = stringify!(#fn_name);
            type Request = #req_ty;
            type Response = #resp_ty;
        }

        #[::linkme::distributed_slice(::cosmon_thin_cli::registry::VERBS)]
        #[allow(non_upper_case_globals)]
        static #registry_ident: ::cosmon_thin_cli::registry::VerbDescriptor =
            ::cosmon_thin_cli::registry::VerbDescriptor {
                name: stringify!(#fn_name),
                method: #method_lit,
                path: #path_lit,
                principal_str: #principal_lit,
            };
    };

    expanded.into()
}

fn pascal_case(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper_next = true;
    for ch in snake.chars() {
        if ch == '_' {
            upper_next = true;
            continue;
        }
        if upper_next {
            for u in ch.to_uppercase() {
                out.push(u);
            }
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::pascal_case;

    #[test]
    fn pascal_simple() {
        assert_eq!(pascal_case("observe"), "Observe");
    }

    #[test]
    fn pascal_snake() {
        assert_eq!(pascal_case("nucleate_molecule"), "NucleateMolecule");
    }

    #[test]
    fn pascal_double_underscore() {
        assert_eq!(pascal_case("a__b"), "AB");
    }
}
