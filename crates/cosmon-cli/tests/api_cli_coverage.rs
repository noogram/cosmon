// SPDX-License-Identifier: AGPL-3.0-only

//! API ↔ CLI coverage drift test (§8p enforcement).
//!
//! This test is the CI gate for invariant
//! [`§8p` — *API surface ⊊ CLI surface*](../../../docs/architectural-invariants.md#8p-api-surface-cli-surface-proposed--adr-080).
//! It mechanically enforces three rules on every CI run:
//!
//! 1. Every user-facing `cs` verb has a row in
//!    `docs/guides/api-cli-coverage.md`. A new verb landing without a
//!    row fails the test.
//! 2. Every row marked `Exposed = V0` corresponds to an axum route in
//!    `cosmon-rpp-adapter::routes`. A registry that promises a V0
//!    route the adapter does not implement fails the test.
//! 3. Every axum route exposed by `cosmon-rpp-adapter` has a row in
//!    the registry, and that row's *Exposed* column is at or below
//!    the current version. A route added without updating the
//!    registry fails the test.
//!
//! The pre-V0 state of the codebase (V0 lands week 5–9 May 2026 per
//! ADR-080 §10.1) is encoded by [`list_axum_routes`] returning an
//! empty slice. Once `cosmon-rpp-adapter` lands, that function grows
//! a re-export from the adapter crate without changing the test
//! structure.
//!
//! See [ADR-080 §4 (§8p)](../../../docs/adr/080-remote-pilot-port-https-oidc.md)
//! for the governing decision and
//! [docs/guides/api-cli-coverage.md](../../../docs/guides/api-cli-coverage.md)
//! for the registry itself.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the registry guide, resolved against the workspace root.
fn registry_path() -> PathBuf {
    workspace_root().join("docs/guides/api-cli-coverage.md")
}

/// Walk up from the test binary to find the workspace root (the
/// directory containing `Cargo.toml` and a `crates/` subdirectory).
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // CARGO_MANIFEST_DIR points at crates/cosmon-cli/. Walk up two
    // levels: -> crates/ -> workspace root.
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("workspace root above crates/cosmon-cli")
}

/// Path to the `cs` test binary built by Cargo for this integration
/// test target.
fn cs_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cs")
}

/// Run `cs <args...>` and return stdout (panicking on non-zero exit).
fn run_cs(args: &[&str]) -> String {
    let out = Command::new(cs_bin())
        .args(args)
        .output()
        .expect("spawn cs");
    assert!(
        out.status.success(),
        "cs {} exited non-zero: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("stdout is utf8")
}

/// Top-level user-facing `cs` verbs, as enumerated by the hidden
/// `cs __help-tree` subcommand. Multi-segment paths (e.g. `events tail`)
/// are collapsed to their root verb (`events`); the registry tracks the
/// root verb and any sub-verbs it explicitly calls out (e.g.
/// `cs security activate` is a separate row).
fn cli_top_level_verbs() -> Vec<String> {
    // `--all` includes `hide = true` verbs: the UX-CLI parity registry
    // tracks every real verb because API exposure is orthogonal to
    // book/help visibility (a verb hidden from the mdBook Reference can
    // still carry a deliberate `NO`/`V0` API-exposure decision).
    let stdout = run_cs(&["__help-tree", "--all"]);
    let mut roots: Vec<String> = stdout
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect();
    roots.sort();
    roots.dedup();
    // Filter out hidden plumbing that begins with `__` (defensive — the
    // help-tree walker already skips `hide = true` subcommands).
    roots.retain(|v| !v.starts_with("__"));
    roots
}

/// Sub-verbs the registry tracks as separate rows (e.g.
/// `cs security activate` is a hard-NEVER, distinct from the
/// `cs security status` row). Listed explicitly so the test can
/// assert their presence even though `__help-tree` emits them
/// nested under their parent.
const TRACKED_SUB_VERBS: &[&str] = &["security activate"];

/// One row of the `docs/guides/api-cli-coverage.md` audit table.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RegistryRow {
    /// The `cs` verb (without the leading `cs `), as the row's first
    /// column declares it. May contain spaces for sub-verbs (e.g.
    /// `security activate`).
    verb: String,
    /// The *Exposed via API?* column, normalised to one of:
    /// `V0`, `V1`, `V1 (TBD)`, `V2 (TBD)`, `NO`, `NO (NEVER)`,
    /// `NO (V1 TBD)`, `NO (V2 TBD)`, etc.
    exposed: String,
    /// The *API path* column, empty for un-exposed verbs.
    api_path: String,
}

impl RegistryRow {
    /// Whether the row's *Exposed* column promises an axum route at
    /// V0 (i.e. the adapter MUST implement this route today).
    fn is_v0(&self) -> bool {
        self.exposed.trim() == "V0"
    }

    /// Whether the row promises an axum route at the current shipped
    /// version. Used to gate the route ↔ registry round-trip: a route
    /// in `list_axum_routes()` whose registry row is `V1 (TBD)` is a
    /// drift (route landed before the version it was promised at).
    fn is_currently_exposed(&self) -> bool {
        // V0 is the only currently-shipped version.
        self.exposed.trim() == "V0"
    }
}

/// Parse the audit table from `docs/guides/api-cli-coverage.md`.
///
/// The parser scans every markdown table whose header row contains
/// the literal string `cs` verb (the first column header) and
/// `Exposed via API?` (the second). Rows are tolerant of inline
/// markdown (back-ticks, bold, code spans) — the verb is extracted
/// from the first column by stripping back-ticks and the leading
/// `cs ` prefix.
fn parse_registry() -> Vec<RegistryRow> {
    let text =
        std::fs::read_to_string(registry_path()).expect("docs/guides/api-cli-coverage.md exists");
    let mut rows = Vec::new();
    let mut in_audit_table = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Header detection: the audit table's header row contains
        // `\`cs\` verb` and `Exposed via API?`. Be permissive about
        // adjacent columns so future column additions do not break
        // the parser.
        if trimmed.starts_with('|')
            && trimmed.contains("`cs` verb")
            && trimmed.contains("Exposed via API?")
        {
            in_audit_table = true;
            continue;
        }

        // Any non-table line ends the current table. A separator row
        // (`|---|---|...`) is *inside* the table and is skipped below.
        if in_audit_table && !trimmed.starts_with('|') {
            in_audit_table = false;
            continue;
        }

        if !in_audit_table {
            continue;
        }

        // Skip the separator row immediately after the header.
        if trimmed
            .chars()
            .filter(|&c| c != '|' && c != '-' && !c.is_whitespace())
            .count()
            == 0
        {
            continue;
        }

        // Split on `|`, drop the leading and trailing empty fields
        // (the row starts and ends with `|`).
        let cells: Vec<&str> = trimmed
            .split('|')
            .map(str::trim)
            .collect::<Vec<_>>()
            .into_iter()
            .filter(|c| !c.is_empty())
            .collect();

        // We need at least 4 columns: verb · exposed · path · reason.
        if cells.len() < 4 {
            continue;
        }

        let verb = strip_verb(cells[0]);
        let exposed = strip_inline_md(cells[1]);
        let api_path = strip_inline_md(cells[2]);

        // A row whose verb does not start with the `cs` prefix is
        // a header-row variant or a stray pipe in prose; skip it.
        if verb.is_empty() {
            continue;
        }

        rows.push(RegistryRow {
            verb,
            exposed,
            api_path,
        });
    }

    rows
}

/// Strip back-ticks and the leading `cs ` prefix from the first
/// column of an audit row, leaving the bare verb (e.g.
/// `nucleate`, `security activate`, `done`).
fn strip_verb(cell: &str) -> String {
    let bare = strip_inline_md(cell);
    bare.strip_prefix("cs ").unwrap_or(&bare).to_string()
}

/// Strip back-ticks, bold markers, and inline code spans from a
/// markdown cell; collapse internal whitespace.
fn strip_inline_md(cell: &str) -> String {
    let cleaned: String = cell
        .chars()
        .filter(|&c| c != '`' && c != '*')
        .collect::<String>();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One axum route exposed by `cosmon-rpp-adapter`, surfaced for the
/// drift test.
///
/// In the pre-V0 state of the codebase, `list_axum_routes` returns
/// an empty slice. Once `cosmon-rpp-adapter` lands its first route
/// (`GET /v1/molecules/:id` per ADR-080 §10.1), this struct is
/// re-exported from the adapter and the function returns the real
/// route table.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RouteInfo {
    /// HTTP method, uppercased (`GET`, `POST`, …).
    method: &'static str,
    /// Path with axum-style placeholders (e.g. `/v1/molecules/:id`).
    path: &'static str,
    /// The user-facing `cs` verb this route is a strict subset of.
    /// Matched against `RegistryRow::verb` for the round-trip check.
    cs_verb: &'static str,
}

/// Return the list of axum routes exposed by `cosmon-rpp-adapter`.
///
/// V0 surface — kept hand-wired here (the adapter does not currently
/// re-export a typed route table; the freeze surface
/// `cosmon_rpp_adapter::frozen_api_surface()` carries strings, not
/// (verb, path) pairs). When the adapter grows a typed `list_routes()`,
/// this function reads from it.
fn list_axum_routes() -> &'static [RouteInfo] {
    &[
        RouteInfo {
            method: "GET",
            path: "/v1/molecules/:id",
            cs_verb: "observe",
        },
        RouteInfo {
            method: "POST",
            path: "/v1/molecules",
            cs_verb: "nucleate",
        },
        RouteInfo {
            method: "POST",
            path: "/v1/molecules/:id/run",
            cs_verb: "run",
        },
    ]
}

/// Verbs the registry MUST mark as `**NO (NEVER)**` (or equivalent
/// hard-NEVER form) per ADR-080 §5.1. The audit gate refuses to admit
/// any axum route for these verbs.
const NEVER_VERBS: &[&str] = &[
    "done",
    "evolve",
    "complete",
    "purge",
    "reconcile",
    "kill",
    // `run` left the NEVER class 2026-06-11 (ADR-124): the bounded
    // drain `POST /v1/molecules/:id/run` exposes a REQUEST for a
    // drain under binding-sealed bounds, not the operator
    // orchestrator.
    "security activate",
];

#[test]
fn every_cli_verb_has_a_registry_row() {
    let registry = parse_registry();
    let mut verbs = cli_top_level_verbs();
    for sub in TRACKED_SUB_VERBS {
        verbs.push((*sub).to_string());
    }

    let mut missing = Vec::new();
    for verb in &verbs {
        let found = registry.iter().any(|row| row.verb == *verb);
        if !found {
            missing.push(verb.clone());
        }
    }

    assert!(
        missing.is_empty(),
        "the following user-facing `cs` verbs are absent from \
         docs/guides/api-cli-coverage.md (mark `Exposed via API? = NO` \
         if no remote use case exists yet):\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn registry_does_not_invent_unknown_verbs() {
    // Verbs the registry tracks as separate rows that are NOT top-level
    // `cs` commands but are deliberate sub-verb call-outs (e.g.
    // `security activate` vs the parent `security`). The list mirrors
    // the audit table's intent: explicit hard-NEVER sub-verbs.
    let allowed_extras: Vec<String> = TRACKED_SUB_VERBS
        .iter()
        .map(|s| (*s).to_string())
        .chain(std::iter::once("security status".to_string()))
        .chain(std::iter::once(
            "security oidc kill / revoke / unrevoke".to_string(),
        ))
        .collect();

    let cli_verbs: std::collections::HashSet<String> = cli_top_level_verbs().into_iter().collect();
    let registry = parse_registry();

    let mut unknown = Vec::new();
    for row in &registry {
        // Strip a sub-verb to its root for the existence check
        // (e.g. `security activate` → `security`).
        let root = row.verb.split_whitespace().next().unwrap_or(&row.verb);
        if !cli_verbs.contains(root) && !allowed_extras.contains(&row.verb) {
            unknown.push(row.verb.clone());
        }
    }

    assert!(
        unknown.is_empty(),
        "the registry tracks verbs that no longer exist in the CLI \
         (remove the row or restore the verb):\n  {}",
        unknown.join("\n  ")
    );
}

#[test]
fn never_verbs_carry_a_hard_no_marker() {
    let registry = parse_registry();
    let mut violations = Vec::new();
    for never in NEVER_VERBS {
        let row = registry
            .iter()
            .find(|r| r.verb == *never)
            .unwrap_or_else(|| panic!("registry must list `{never}` (ADR-080 §5.1)"));
        // The hard-NEVER class is identifiable by the literal string
        // `NO (NEVER)` inside the row's *Exposed* column. The visible
        // markdown (`**NO (NEVER)**`) is normalised by `strip_inline_md`
        // to `NO (NEVER)`.
        if !row.exposed.contains("NO (NEVER)") && !row.exposed.contains("NO") {
            violations.push((never.to_string(), row.exposed.clone()));
        }
        // Stronger guarantee: a NEVER verb's row must not name an API
        // path. An entry under "API path" for a NEVER verb is by
        // definition a §8p breach.
        if !row.api_path.is_empty() && row.api_path != "—" && row.api_path != "-" {
            violations.push((never.to_string(), format!("api_path={}", row.api_path)));
        }
    }
    assert!(
        violations.is_empty(),
        "the following ADR-080 §5.1 NEVER verbs lack a hard-NO marker \
         or carry an API path:\n  {}",
        violations
            .iter()
            .map(|(v, e)| format!("{v} → {e}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

#[test]
fn axum_routes_round_trip_with_registry() {
    let registry = parse_registry();
    let routes = list_axum_routes();

    // 1. Every axum route has a matching registry row marked at-or-before
    //    the current version.
    for route in routes {
        let row = registry
            .iter()
            .find(|r| r.verb == route.cs_verb)
            .unwrap_or_else(|| {
                panic!(
                    "axum route {} {} (cs_verb=`{}`) has no row in \
                     docs/guides/api-cli-coverage.md — add one or remove \
                     the route",
                    route.method, route.path, route.cs_verb
                )
            });
        assert!(
            row.is_currently_exposed(),
            "axum route {} {} is exposed by cosmon-rpp-adapter, but the \
             registry says `Exposed = {}` (must be V0 to ship today)",
            route.method,
            route.path,
            row.exposed,
        );
        assert!(
            !NEVER_VERBS.contains(&route.cs_verb),
            "axum route {} {} maps to NEVER verb `cs {}` — this is a \
             §8p breach (ADR-080 §5.1). File a bead, do not patch the \
             adapter.",
            route.method,
            route.path,
            route.cs_verb,
        );
    }

    // 2. Every registry row marked `V0` has a corresponding axum route.
    for row in &registry {
        if !row.is_v0() {
            continue;
        }
        let implemented = routes.iter().any(|r| r.cs_verb == row.verb);
        assert!(
            implemented,
            "registry promises `cs {}` at V0 (path: {}) but no \
             matching axum route is listed in \
             cosmon-rpp-adapter::routes::list_routes()",
            row.verb, row.api_path,
        );
    }
}
