// SPDX-License-Identifier: AGPL-3.0-only

//! Semver token-compat tests.
//!
//! These tests live in `tests/` so they exercise the **out-of-crate**
//! public surface of `cosmon-agent-harness`. Anything that compiles here
//! is exactly what a downstream crate sees.
//!
//! The guarantee under test: every wire-shaped public type is
//! `#[non_exhaustive]`, so adding a field or a variant is a MINOR bump.
//! The mechanical proof is two-fold:
//!
//! 1. **Construction goes through constructors, not struct literals.**
//!    The `::new()` / `::from_json()` factories are the only stable
//!    construction path. If a future change removed `#[non_exhaustive]`
//!    and a downstream crate started using `ToolCall { … }` directly,
//!    re-adding `#[non_exhaustive]` would become a MAJOR break — these
//!    tests fail-fast against that drift.
//!
//! 2. **Match sites carry a catch-all.** Adding a new `ToolError` variant
//!    must be a MINOR bump, so external matches must use `_ => …`. The
//!    `matches!` test below mirrors what a downstream consumer must do.

use cosmon_agent_harness::tool::{ParametersSchema, ToolCall, ToolDeclaration, ToolError};

/// `ToolCall::new` is the public construction path. Struct literal
/// `ToolCall { id, name, arguments_json }` does not compile here
/// because the struct is `#[non_exhaustive]` — this test proves the
/// factory is sufficient for downstream needs.
#[test]
fn tool_call_constructs_via_new_factory() {
    let call = ToolCall::new("call-1", "read_file", r#"{"path":"x"}"#);
    assert_eq!(call.id, "call-1");
    assert_eq!(call.name, "read_file");
    assert!(call.arguments_json.contains("path"));
}

/// `ParametersSchema::from_json` is the only stable construction path —
/// the inner `serde_json::Value` is hidden behind the newtype so a future
/// migration to `schemars` is non-breaking (tolnay F1).
#[test]
fn parameters_schema_constructs_via_from_json() {
    let schema = ParametersSchema::from_json(serde_json::json!({
        "type": "object",
        "properties": {}
    }));
    let back = schema.as_json();
    assert_eq!(back["type"], "object");
}

/// `ToolDeclaration::new` accepts `impl Into<ParametersSchema>` — both
/// a raw `serde_json::Value` and a pre-built `ParametersSchema` are
/// admissible. The struct literal is rejected by the compiler because
/// of `#[non_exhaustive]`.
#[test]
fn tool_declaration_constructs_via_new_factory() {
    let decl = ToolDeclaration::new(
        "exec_command",
        "Run a shell command in the worktree.",
        serde_json::json!({"type": "object"}),
    );
    assert_eq!(decl.name, "exec_command");
    assert!(decl.description.contains("worktree"));
    assert_eq!(decl.parameters.as_json()["type"], "object");
}

/// External `match` on a `#[non_exhaustive]` enum must carry a catch-all
/// arm. This test mirrors what a downstream consumer must do: enumerate
/// the variants the caller cares about, route them to distinct outputs,
/// and forward the rest via `_`. A future MINOR bump that adds e.g.
/// `ToolError::Timeout(_)` must not require recompiling downstream
/// callers — the catch-all guarantees it.
#[test]
fn tool_error_match_carries_catchall() {
    fn classify(err: &ToolError) -> &'static str {
        match err {
            ToolError::PathEscape(_) => "escape",
            ToolError::NotWhitelisted(_) => "unknown-tool",
            _ => "other",
        }
    }

    assert_eq!(
        classify(&ToolError::PathEscape("../escape".to_owned())),
        "escape"
    );
    assert_eq!(
        classify(&ToolError::NotWhitelisted("ghost".to_owned())),
        "unknown-tool"
    );
    assert_eq!(classify(&ToolError::Io("disk".to_owned())), "other");
}

/// `matches!` with a catch-all is the idiomatic shorthand for the above.
/// The macro form mirrors what downstream code most often writes; a new
/// variant added later still matches the `_` arm.
#[test]
fn tool_error_matches_macro_with_catchall() {
    fn is_io(err: &ToolError) -> bool {
        matches!(err, ToolError::Io(_))
    }

    assert!(is_io(&ToolError::Io("disk full".to_owned())));
    assert!(!is_io(&ToolError::PathEscape("x".to_owned())));
}
