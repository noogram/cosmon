// SPDX-License-Identifier: AGPL-3.0-only

//! Parser for the enriched §8p surface event log — the single canonical
//! declaration of the cosmon RPP API surface.
//!
//! The canon lives in `crates/cosmon-rpp-adapter/data/surface_events.txt`,
//! an append-only event log where each non-comment line declares one HTTP
//! route mounted on the RPP. This crate is the **one** parser of that
//! format, consumed as a *build-dependency* by every crate that folds the
//! canon into compile-time tables:
//!
//! - `cosmon-rpp-adapter/build.rs` → `SURFACE_EVENTS` / `SURFACE_ROUTES`
//!   (the §8p frozen surface and its enriched metadata);
//! - `cosmon-thin-cli/build.rs` → `SURFACE_SCOPES` (the per-verb scope
//!   table rendered by `cs-thin help`, replacing the hand-mapped
//!   `help::scope_for`).
//!
//! It is also a *normal* dependency of `cosmon-rpp-adapter` so that the
//! [`Exposure`] enum referenced by the generated code is a single type,
//! not a per-crate mirror.
//!
//! # Line format (7 `|`-separated fields)
//!
//! ```text
//! METHOD PATH | molecule_id | YYYY-MM-DD | principal | scope | exposure | blurb
//! ```
//!
//! - `principal` ∈ {`tenant`, `operator`, `worker`} — the wire-form
//!   principal class of the caller (mirrors the `#[verb]` annotation).
//! - `scope` — minimal `OAuth2` scope required by the route. Either `-`
//!   (authentication-level route, no scope check) or one or more
//!   `cosmon:`-prefixed scopes joined by `+` (AND semantics, e.g.
//!   `cosmon:molecule:write+cosmon:worker:spawn` for tackle).
//! - `exposure` ∈ {`tenant-verb`, `adapter-only`, `operator-only`} — the
//!   §8p classification previously hand-coded twice in
//!   `api_surface_freeze.rs` (`is_adapter_only()` + `forbidden`).
//! - `blurb` — one-line human description, drained from the clap
//!   doc-comments. Non-empty.
//!
//! The parser **refuses ambiguous lines**: wrong field count, unknown
//! principal/exposure, malformed scope, or empty blurb all yield an
//! `Err` naming the file line — a build that consumes a half-classified
//! canon must not succeed.

#![forbid(unsafe_code)]

use std::fmt;

/// §8p exposure classification of one route. Replaces the two
/// hand-maintained copies of the §8p frontier that used to live in
/// `api_surface_freeze.rs`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Exposure {
    /// The route has a matching `#[verb]` annotation and is carried by
    /// the thin tenant CLI — it participates in the bijection test.
    TenantVerb,
    /// The route is served by the adapter only (filesystem-mediated,
    /// SSE, discovery, auth flow…) — intentionally **no** `cs` verb.
    AdapterOnly,
    /// Reserved classification: a route that would expose an
    /// operator-only verb (ADR-080 §5.1). The §8p freeze test asserts
    /// that **no** event on the frozen surface carries this value.
    OperatorOnly,
}

impl Exposure {
    /// The canonical kebab-case token used in the data file.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::TenantVerb => "tenant-verb",
            Self::AdapterOnly => "adapter-only",
            Self::OperatorOnly => "operator-only",
        }
    }

    /// The Rust variant path emitted by the build-script folds.
    #[must_use]
    pub const fn as_variant(self) -> &'static str {
        match self {
            Self::TenantVerb => "Exposure::TenantVerb",
            Self::AdapterOnly => "Exposure::AdapterOnly",
            Self::OperatorOnly => "Exposure::OperatorOnly",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "tenant-verb" => Some(Self::TenantVerb),
            "adapter-only" => Some(Self::AdapterOnly),
            "operator-only" => Some(Self::OperatorOnly),
            _ => None,
        }
    }
}

impl fmt::Display for Exposure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_token())
    }
}

/// One parsed `surface_added` event — the owned, build-time shape.
/// The compile-time reification (`&'static str` fields) is emitted by
/// the consuming build scripts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonEvent {
    /// `"METHOD PATH"`, e.g. `"GET /v1/molecules/{id}"`.
    pub method_path: String,
    /// Molecule (or ADR slug) that mounted the route.
    pub molecule_id: String,
    /// `YYYY-MM-DD` landing date (free-form, for humans).
    pub timestamp: String,
    /// Wire-form principal class: `tenant`, `operator`, or `worker`.
    pub principal: String,
    /// Minimal scope expression (`-` or `+`-joined `cosmon:` scopes).
    pub scope: String,
    /// §8p exposure classification.
    pub exposure: Exposure,
    /// One-line human description of the route.
    pub blurb: String,
}

const KNOWN_PRINCIPALS: &[&str] = &["tenant", "operator", "worker"];
const KNOWN_METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH"];

/// Parse the full canon file. `origin` is used in error messages
/// (typically the file path as the consumer knows it).
///
/// # Errors
///
/// Returns the first offending line with its 1-based line number and a
/// description of why it is ambiguous. The consuming build scripts
/// `panic!` on `Err` — an ambiguous canon must not build.
pub fn parse_canon(raw: &str, origin: &str) -> Result<Vec<CanonEvent>, String> {
    let mut events = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        let lineno = idx + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        events.push(parse_line(trimmed).map_err(|why| format!("{origin}:{lineno}: {why}"))?);
    }
    Ok(events)
}

fn parse_line(line: &str) -> Result<CanonEvent, String> {
    let parts: Vec<&str> = line.split('|').map(str::trim).collect();
    if parts.len() != 7 {
        return Err(format!(
            "malformed event line (need 7 `|`-separated fields \
             `METHOD PATH | molecule_id | date | principal | scope | exposure | blurb`, \
             got {}): {line:?}",
            parts.len(),
        ));
    }
    let [method_path, molecule_id, timestamp, principal, scope, exposure, blurb] = [
        parts[0], parts[1], parts[2], parts[3], parts[4], parts[5], parts[6],
    ];

    let Some((method, path)) = method_path.split_once(' ') else {
        return Err(format!(
            "first field must be `METHOD PATH`, got {method_path:?}"
        ));
    };
    if !KNOWN_METHODS.contains(&method) {
        return Err(format!(
            "unknown HTTP method {method:?} (expected one of {KNOWN_METHODS:?})"
        ));
    }
    if !path.starts_with('/') {
        return Err(format!("path must start with `/`, got {path:?}"));
    }
    if molecule_id.is_empty() {
        return Err("molecule_id is required (use task ID or ADR slug)".to_owned());
    }
    if timestamp.is_empty() {
        return Err("date is required (YYYY-MM-DD)".to_owned());
    }
    if !KNOWN_PRINCIPALS.contains(&principal) {
        return Err(format!(
            "ambiguous principal {principal:?} (expected one of {KNOWN_PRINCIPALS:?})"
        ));
    }
    validate_scope(scope)?;
    let Some(exposure) = Exposure::from_token(exposure) else {
        return Err(format!(
            "ambiguous exposure {exposure:?} (expected `tenant-verb`, \
             `adapter-only`, or `operator-only`)"
        ));
    };
    if blurb.is_empty() {
        return Err("blurb is required (one-line human description)".to_owned());
    }

    Ok(CanonEvent {
        method_path: method_path.to_owned(),
        molecule_id: molecule_id.to_owned(),
        timestamp: timestamp.to_owned(),
        principal: principal.to_owned(),
        scope: scope.to_owned(),
        exposure,
        blurb: blurb.to_owned(),
    })
}

/// `-` (no scope check) or `+`-joined `cosmon:`-prefixed scopes.
fn validate_scope(scope: &str) -> Result<(), String> {
    if scope == "-" {
        return Ok(());
    }
    if scope.is_empty() {
        return Err("scope is required (`-` for auth-level routes, else `cosmon:…`)".to_owned());
    }
    for part in scope.split('+') {
        if !part.starts_with("cosmon:") || part.len() <= "cosmon:".len() {
            return Err(format!(
                "ambiguous scope part {part:?} in {scope:?} \
                 (each `+`-joined part must be a `cosmon:`-prefixed scope, or the whole field `-`)"
            ));
        }
    }
    Ok(())
}

/// Effect annotation derived from a canon scope expression: the scope
/// catalogue is already the map of real-world side effects. ADR-080 §6.5
/// demands a *distinct scope*
/// for any operation that burns real money or is structurally
/// irreversible, so the marker a help page / man page / generated
/// reference prints can be DERIVED from the scope column instead of
/// hand-written prose (which can lie). This function is the ONE map;
/// `cosmon-remote`'s clap `about` strings and `xtask gen-api-ref`
/// both call it. A future irreversible scope gains its row here and
/// every surface picks it up on the next build.
#[must_use]
pub fn effect_annotation(scope: &str) -> Option<&'static str> {
    if scope == "-" {
        return None;
    }
    for part in scope.split('+') {
        match part {
            // $$ Anthropic — spawning a worker burns real credit.
            "cosmon:worker:spawn" => return Some("[coûteux]"),
            // Reserved scope for destructive worker ops (cancel/kill);
            // declared in the catalogue, not yet wired to any route.
            "cosmon:worker:terminate" => return Some("[irréversible]"),
            _ => {}
        }
    }
    None
}

/// Normalise a path template so `{id}`-style (axum) and `:id`-style
/// (Express, used by the `#[verb]` annotations) placeholders compare
/// equal. The canonical output form is the colon prefix.
#[must_use]
pub fn normalise_path(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            if let Some(name) = seg.strip_prefix(':') {
                format!(":{name}")
            } else if seg.starts_with('{') && seg.ends_with('}') && seg.len() > 2 {
                format!(":{}", &seg[1..seg.len() - 1])
            } else {
                seg.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "GET /v1/molecules/{id} | task-x | 2026-06-10 | tenant | \
                        cosmon:molecule:read | tenant-verb | Read one molecule";

    #[test]
    fn parses_a_well_formed_line() {
        let events = parse_canon(GOOD, "test").unwrap();
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.method_path, "GET /v1/molecules/{id}");
        assert_eq!(ev.principal, "tenant");
        assert_eq!(ev.scope, "cosmon:molecule:read");
        assert_eq!(ev.exposure, Exposure::TenantVerb);
        assert_eq!(ev.blurb, "Read one molecule");
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let raw = format!("# comment\n\n{GOOD}\n");
        assert_eq!(parse_canon(&raw, "test").unwrap().len(), 1);
    }

    #[test]
    fn refuses_wrong_field_count() {
        let err = parse_canon("GET /x | m | d | note", "test").unwrap_err();
        assert!(err.contains("need 7"), "{err}");
        assert!(err.contains("test:1"), "error must name the line: {err}");
    }

    #[test]
    fn refuses_ambiguous_exposure() {
        let line = GOOD.replace("tenant-verb", "maybe-public");
        let err = parse_canon(&line, "test").unwrap_err();
        assert!(err.contains("ambiguous exposure"), "{err}");
    }

    #[test]
    fn refuses_ambiguous_principal() {
        let line = GOOD.replace("| tenant |", "| client |");
        let err = parse_canon(&line, "test").unwrap_err();
        assert!(err.contains("ambiguous principal"), "{err}");
    }

    #[test]
    fn refuses_malformed_scope() {
        for bad in ["molecule:read", "cosmon:", ""] {
            let line = GOOD.replace("cosmon:molecule:read", bad);
            let err = parse_canon(&line, "test").unwrap_err();
            assert!(err.contains("scope"), "scope {bad:?}: {err}");
        }
    }

    #[test]
    fn accepts_no_scope_marker_and_compound_scopes() {
        for ok in ["-", "cosmon:molecule:write+cosmon:worker:spawn"] {
            let line = GOOD.replace("cosmon:molecule:read", ok);
            let ev = &parse_canon(&line, "test").unwrap()[0];
            assert_eq!(ev.scope, ok);
        }
    }

    #[test]
    fn refuses_empty_blurb() {
        let line = GOOD.replace("Read one molecule", "");
        let err = parse_canon(&line, "test").unwrap_err();
        assert!(err.contains("blurb"), "{err}");
    }

    #[test]
    fn refuses_unknown_method() {
        let line = GOOD.replace("GET ", "FETCH ");
        let err = parse_canon(&line, "test").unwrap_err();
        assert!(err.contains("unknown HTTP method"), "{err}");
    }

    #[test]
    fn normalisation_round_trips() {
        assert_eq!(normalise_path("/v1/molecules/:id"), "/v1/molecules/:id");
        assert_eq!(normalise_path("/v1/molecules/{id}"), "/v1/molecules/:id");
        assert_eq!(
            normalise_path("/v1/molecules/{id}/tags"),
            "/v1/molecules/:id/tags"
        );
        assert_eq!(normalise_path("/v1/molecules"), "/v1/molecules");
    }

    #[test]
    fn effect_annotation_derives_from_scope_not_prose() {
        assert_eq!(
            effect_annotation("cosmon:molecule:write+cosmon:worker:spawn"),
            Some("[coûteux]")
        );
        assert_eq!(
            effect_annotation("cosmon:worker:terminate"),
            Some("[irréversible]")
        );
        assert_eq!(effect_annotation("cosmon:molecule:write"), None);
        assert_eq!(effect_annotation("-"), None);
    }

    #[test]
    fn exposure_tokens_round_trip() {
        for e in [
            Exposure::TenantVerb,
            Exposure::AdapterOnly,
            Exposure::OperatorOnly,
        ] {
            assert_eq!(Exposure::from_token(e.as_token()), Some(e));
        }
    }
}
