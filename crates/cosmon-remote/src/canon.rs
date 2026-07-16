// SPDX-License-Identifier: AGPL-3.0-only

//! Canon-projected route surface — the ONE place the delivered tenant
//! binary knows the §8p routes.
//!
//! The table below is folded at build time from
//! `crates/cosmon-rpp-adapter/data/surface_events.txt` (the enriched
//! surface canon). Client methods and the clap
//! `about` strings refer to routes through the generated consts —
//! never through a string literal. The bijection gate
//! (`tests/surface_bijection.rs`) compares [`ROUTES_USED`] against the
//! `#[verb]` link-time registry, which the adapter-side
//! `routes_and_verbs_are_bijective` test in turn pins to the canon:
//! the binary a tenant installs is covered end-to-end.

pub use cosmon_surface_canon::Exposure;

/// One §8p route, reified from a canon line at build time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonRoute {
    /// Upper-case HTTP method literal (`"GET"`, `"POST"`, …).
    pub method: &'static str,
    /// Path template in the canon's brace form (`/v1/molecules/{id}`).
    pub path: &'static str,
    /// Minimal scope expression: `-` (auth-level, no scope check) or
    /// `+`-joined `cosmon:` scopes with AND semantics.
    pub scope: &'static str,
    /// §8p exposure classification.
    pub exposure: Exposure,
    /// One-line human description from the canon.
    pub blurb: &'static str,
}

impl CanonRoute {
    /// Substitute the `{…}` placeholders in order with `args`.
    ///
    /// Arity is part of the canon contract: every call site passes a
    /// fixed-size slice, and `tests/surface_bijection.rs` exercises the
    /// substitution for every consumed route.
    ///
    /// # Panics
    ///
    /// Panics when `args` does not match the placeholder count — a
    /// programmer error, unreachable through the CLI surface.
    #[must_use]
    pub fn path_with(&self, args: &[&str]) -> String {
        let mut out = String::with_capacity(self.path.len() + 16);
        let mut rest = self.path;
        let mut values = args.iter();
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let end = rest[start..]
                .find('}')
                .map(|e| e + start)
                .expect("canon paths have balanced placeholder braces");
            let value = values
                .next()
                .unwrap_or_else(|| panic!("missing argument for placeholder in {}", self.path));
            out.push_str(value);
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        assert!(
            values.next().is_none(),
            "too many arguments for {}",
            self.path
        );
        out
    }

    /// The scope set to request when minting a token for this route —
    /// the canon's `+`-joined expression split apart; empty for `-`
    /// (auth-level routes).
    #[must_use]
    pub fn scopes(&self) -> Vec<String> {
        if self.scope == "-" {
            return Vec::new();
        }
        self.scope.split('+').map(str::to_owned).collect()
    }

    /// Backticked `METHOD /path` label — the form the clap `about`
    /// strings embed, byte-identical to the pre-fusion hand prose.
    #[must_use]
    pub fn label(&self) -> String {
        format!("`{} {}`", self.method, self.path)
    }

    /// ` [coûteux]` / ` [irréversible]` marker (leading space) or `""`,
    /// derived from the route's scope column via the ONE map in
    /// [`cosmon_surface_canon::effect_annotation`]: the scope catalogue
    /// already encodes
    /// « this burns real money / cannot be undone » structurally
    /// (ADR-080 §6.5 — distinct scope per costly/irreversible effect),
    /// so the help marker is a derivation, never hand prose. Every
    /// route-backed clap `about` appends this; a future costly route
    /// renders its marker on the next build with no prose edit.
    #[must_use]
    pub fn effect_suffix(&self) -> String {
        cosmon_surface_canon::effect_annotation(self.scope)
            .map_or_else(String::new, |marker| format!(" {marker}"))
    }
}

include!(concat!(env!("OUT_DIR"), "/canon_surface_generated.rs"));

/// Every canon route the delivered binary dials, one entry per client
/// method. This is the projection surface the bijection gate checks:
/// the `tenant-verb` subset must equal the `#[verb]` registry, and no
/// entry may be `operator-only`.
///
/// NOT consumed (and why): `GET /v1/molecules/{id}/logs` (SSE pane
/// tail — no CLI verb yet) and `POST /v1/avatar/perceive` (canal (d),
/// adapter-only, OFF by default). `converse` joined as a top-level
/// verb to expose the conversational channel.
///
/// Known accepted gap: nothing
/// here proves each entry is *wired into the clap dispatch* — the same
/// verbs↔dispatch channel cs-thin had. The route TUPLES cannot drift
/// (consts vanish when canon lines do); the wiring is covered by the
/// wiremock contract tests per family.
pub static ROUTES_USED: &[&CanonRoute] = &[
    // Molecule lifecycle (tenant verbs).
    GET_V1_MOLECULES,
    GET_V1_MOLECULES_ID,
    POST_V1_MOLECULES,
    POST_V1_MOLECULES_ID_TAGS,
    POST_V1_MOLECULES_ID_COLLAPSE,
    POST_V1_MOLECULES_ID_FREEZE,
    POST_V1_MOLECULES_ID_STUCK,
    POST_V1_MOLECULES_ID_TACKLE,
    POST_V1_MOLECULES_ID_RUN,
    // D-AVATAR canal (b) — top-level `converse` verb (task-20260610-0b57).
    POST_V1_AVATAR_CONVERSE,
    // D-AVATAR instance lifecycle (tenant verbs).
    GET_V1_AVATAR_INSTANCE_ID_STATUS,
    POST_V1_AVATAR_INSTANCE_ID_INCARNATE,
    POST_V1_AVATAR_INSTANCE_ID_GRANT,
    GET_V1_AVATAR_INSTANCE_ID_AUDIT,
    GET_V1_AVATAR_INSTANCE_ID_MOULD_INFO,
    // Deliverable + artifacts.
    GET_V1_MOLECULES_ID_RESULT,
    GET_V1_MOLECULES_ID_ARTIFACTS,
    GET_V1_MOLECULES_ID_ARTIFACTS_TOKEN,
    PUT_V1_MOLECULES_ID_ARTIFACTS_TOKEN,
    // Auth-claude PKCE flow + whoami.
    POST_V1_AUTH_CLAUDE_START,
    POST_V1_AUTH_CLAUDE_EMAIL,
    POST_V1_AUTH_CLAUDE_CONFIRM,
    GET_V1_AUTH_CLAUDE_SESSION_ID,
    DELETE_V1_AUTH_CLAUDE_SESSION_ID,
    GET_V1_AUTH_ME,
    // Streams, discovery, observability.
    GET_V1_EVENTS,
    GET_V1_QUOTA,
    GET_V1_NOYAUX,
    GET_V1_WORKERS,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_with_substitutes_in_order() {
        assert_eq!(
            GET_V1_MOLECULES_ID_ARTIFACTS_TOKEN.path_with(&["m-1", "tok"]),
            "/v1/molecules/m-1/artifacts/tok"
        );
        assert_eq!(GET_V1_MOLECULES.path_with(&[]), "/v1/molecules");
    }

    #[test]
    fn scopes_split_the_and_expression() {
        assert_eq!(
            POST_V1_MOLECULES_ID_TACKLE.scopes(),
            vec![
                "cosmon:molecule:write".to_owned(),
                "cosmon:worker:spawn".to_owned()
            ]
        );
        assert!(POST_V1_AUTH_CLAUDE_START.scopes().is_empty());
    }

    #[test]
    fn label_is_the_backticked_pre_fusion_form() {
        assert_eq!(
            POST_V1_MOLECULES_ID_TACKLE.label(),
            "`POST /v1/molecules/{id}/tackle`"
        );
    }

    /// The cost marker is a derivation of the scope column.
    /// Exactly the routes whose scope demands `worker:spawn` render
    /// ` [coûteux]`; every other frozen route renders nothing today.
    #[test]
    fn effect_suffix_fires_exactly_where_the_scope_says_so() {
        assert_eq!(POST_V1_MOLECULES_ID_TACKLE.effect_suffix(), " [coûteux]");
        for route in ROUTES_USED {
            if route.scopes().iter().any(|s| s == "cosmon:worker:spawn") {
                assert_eq!(route.effect_suffix(), " [coûteux]");
            } else {
                assert_eq!(route.effect_suffix(), "", "{}", route.path);
            }
        }
    }
}
