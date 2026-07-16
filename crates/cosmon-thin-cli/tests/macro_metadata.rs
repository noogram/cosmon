// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the `#[verb]` macro.
//!
//! Lives in `cosmon-thin-cli/tests` (not the macro crate) because proc-macro
//! crates cannot host integration tests that consume their own output. We
//! exercise the macro from a downstream crate the way real callers will.

use cosmon_thin_cli::{registry, IsoVerb, Principal};
use cosmon_thin_macro::verb;

/// Sample annotated function — exercises the macro end-to-end.
///
/// The body is intentionally trivial. The interesting artefacts are the
/// `SampleObserveVerb` marker, the `SampleObserveVerbBody` /
/// `SampleObserveVerbResponse` placeholder structs, and the registry entry
/// the macro emits.
#[verb(method = "GET", path = "/v1/sample/observe", principal = "operator")]
pub fn sample_observe() {}

#[verb(method = "POST", path = "/v1/sample/nucleate", principal = "tenant")]
pub fn sample_nucleate() {}

#[test]
fn iso_verb_metadata_matches_annotation() {
    assert_eq!(SampleObserveVerb::METHOD, "GET");
    assert_eq!(SampleObserveVerb::PATH, "/v1/sample/observe");
    assert_eq!(SampleObserveVerb::PRINCIPAL, Principal::Operator);
    assert_eq!(SampleObserveVerb::VERB_NAME, "sample_observe");

    assert_eq!(SampleNucleateVerb::METHOD, "POST");
    assert_eq!(SampleNucleateVerb::PATH, "/v1/sample/nucleate");
    assert_eq!(SampleNucleateVerb::PRINCIPAL, Principal::Tenant);
}

#[test]
fn registry_contains_annotated_verbs() {
    let names: Vec<&str> = registry::all().iter().map(|d| d.name).collect();
    assert!(
        names.contains(&"sample_observe"),
        "registry missing sample_observe; saw {names:?}"
    );
    assert!(
        names.contains(&"sample_nucleate"),
        "registry missing sample_nucleate; saw {names:?}"
    );
}

#[test]
fn registry_descriptors_typed_principal_decodes() {
    let d = registry::all()
        .iter()
        .find(|d| d.name == "sample_observe")
        .expect("sample_observe descriptor present");
    assert_eq!(d.method, "GET");
    assert_eq!(d.path, "/v1/sample/observe");
    assert_eq!(d.principal(), Some(Principal::Operator));
}

#[test]
fn original_function_remains_callable() {
    // The macro must preserve the original function body unchanged.
    sample_observe();
    sample_nucleate();
}
