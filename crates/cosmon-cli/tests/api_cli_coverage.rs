// SPDX-License-Identifier: AGPL-3.0-only

//! API ↔ CLI coverage drift test (§8p enforcement).
//!
//! This test is the CI gate for invariant
//! [`§8p` — *API surface ⊊ CLI surface*](../../../docs/architectural-invariants.md#8p-api-surface-cli-surface-proposed--adr-080).
//! It mechanically enforces four rules on every CI run:
//!
//! 1. Every user-facing `cs` verb has a row in
//!    `docs/guides/api-cli-coverage.md`. A new verb landing without a
//!    row fails the test.
//! 2. Every row that claims a *shipped* exposure (`V0`, `V1`, `V2`,
//!    `PARTIAL` — anything without a `TBD` qualifier) names an API
//!    path, and every path it names is live on the adapter. A registry
//!    that promises a route the adapter does not serve fails the test.
//! 3. Every live route that belongs to the `cs` alphabet is named by
//!    such a row. A route added without updating the registry fails.
//! 4. No row whose *Exposed* column starts with `NO` names a live
//!    route. That is the §8p breach the registry exists to catch.
//!
//! # Where the route set comes from
//!
//! [`live_routes`] folds `crates/cosmon-rpp-adapter/data/surface_events.txt`
//! — the append-only canon that `cosmon-rpp-adapter/build.rs` folds into
//! `frozen_api_surface()` and the router itself is built from. Reading
//! the canon rather than the adapter crate keeps this test a cheap
//! `cosmon-cli` integration test (the parser, `cosmon-surface-canon`, is
//! a dev-dependency with no runtime deps) while comparing against the
//! *same* bytes the server mounts.
//!
//! It did not always. Until 2026-09-22 the route set was a hand-written
//! array of three entries standing for a surface of forty-two, and the
//! reverse check fired only for rows marked `V0` — so the gate was green
//! over eight rows that named a path nobody served or denied a route that
//! had been live for months. An instrument that compares 3/42 of a
//! surface reads present and is not.
//!
//! # What the gate deliberately does not compare
//!
//! * `Exposure::AdapterOnly` routes (artifact I/O, the Claude PKCE flow,
//!   SSE, discovery, the operator admin plane) have **no** `cs` verb by
//!   construction, so the registry — a `cs`-verb registry — carries no
//!   row for them. Requiring one would mean inventing verbs.
//! * [`TENANT_VERB_ROUTES_OUTSIDE_THE_CS_ALPHABET`] names the six
//!   avatar-canal routes that are tenant verbs of the *thin* client
//!   (`cosmon-remote`) and deliberately have no `cs` counterpart —
//!   « avatar est un mot de doctrine, jamais un nom d'API ». They are
//!   listed one by one, with the reason, rather than skipped by a
//!   wildcard: a blind spot somebody had to write down is one a reviewer
//!   can see.
//!
//! See [ADR-080 §4 (§8p)](../../../docs/adr/080-remote-pilot-port-https-oidc.md)
//! for the governing decision and
//! [docs/guides/api-cli-coverage.md](../../../docs/guides/api-cli-coverage.md)
//! for the registry itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_surface_canon::{fold_live, normalise_path, parse_canon, Exposure};

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
    /// Every `METHOD /path` token found in the *API path* column,
    /// normalised to the `:placeholder` form so a registry cell written
    /// `GET /v1/molecules/{id}` and a canon line written
    /// `GET /v1/molecules/:id` compare equal.
    ///
    /// A cell may name more than one route, or none: `cs wait`'s cell
    /// names the status route it polls while exposing nothing itself.
    /// Parsing the cell rather than trusting a hand-kept verb mapping is
    /// what lets the gate catch a row pointing at a path the adapter
    /// never served (`POST /v1/molecules/:id/transitions`, on the
    /// `tackle` and `collapse` rows for four months).
    api_routes: Vec<String>,
}

impl RegistryRow {
    /// Whether the row claims a *shipped* exposure — a version marker
    /// with no `TBD` qualifier on it.
    ///
    /// `V0` was once the only shipped version, and the round-trip check
    /// read `exposed == "V0"` accordingly. It stayed that way through
    /// the V1 cut, so every `V1`, `V2` and `PARTIAL` row was exempt from
    /// both directions of the gate — which is most of the live surface.
    /// A `TBD` qualifier is what marks a row as a *plan*; everything
    /// else is a claim about today, and the adapter must back it.
    fn claims_shipped(&self) -> bool {
        let exposed = self.exposed.trim();
        if exposed.contains("TBD") {
            return false;
        }
        matches!(exposed, "V0" | "V1" | "V2" | "PARTIAL")
    }

    /// Whether the row *denies* exposure: its verdict begins with `NO`
    /// (`NO`, `NO (NEVER)`, `NO (V2 TBD)`, …). Such a row naming a live
    /// route is the §8p breach the registry exists to catch.
    fn denies_exposure(&self) -> bool {
        self.exposed.trim().starts_with("NO")
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

        let api_routes = extract_routes(&api_path);
        rows.push(RegistryRow {
            verb,
            exposed,
            api_path,
            api_routes,
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

/// Extract every `METHOD /path` token from an *API path* cell,
/// normalised to the `:placeholder` form.
///
/// The cell is prose, not a field: it may carry a qualifier
/// (`(TBD) GET /v1/molecules/:id/peek`), a parenthesised body hint
/// (`POST /v1/molecules/:id/tags`), a dash for "none", or a sentence
/// naming the route a client-side verb polls. Scanning it for
/// method-then-path pairs is tolerant of all four and needs no
/// reformatting of a 350-line guide.
fn extract_routes(cell: &str) -> Vec<String> {
    const METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH"];
    let tokens: Vec<&str> = cell.split_whitespace().collect();
    let mut out = Vec::new();
    for pair in tokens.windows(2) {
        let (method, path) = (pair[0], pair[1]);
        if !METHODS.contains(&method) {
            continue;
        }
        // Trim trailing punctuation the prose leaves attached to a path
        // (`GET /v1/molecules/:id,` at the end of a clause).
        let path = path.trim_end_matches([',', '.', ';', ')', '`']);
        if !path.starts_with('/') {
            continue;
        }
        out.push(format!("{method} {}", normalise_path(path)));
    }
    out.sort();
    out.dedup();
    out
}

/// One route live on the adapter today, folded out of the §8p canon.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveRoute {
    /// `"METHOD /path"` with `:placeholder` segments — the join key
    /// against [`RegistryRow::api_routes`].
    key: String,
    /// §8p classification carried by the canon line that mounted it.
    exposure: Exposure,
}

/// Path to the append-only §8p surface canon.
fn canon_path() -> PathBuf {
    workspace_root().join("crates/cosmon-rpp-adapter/data/surface_events.txt")
}

/// Fold the §8p canon into the set of routes the adapter serves today.
///
/// This is the *same* file `cosmon-rpp-adapter/build.rs` folds into
/// `SURFACE_ROUTES` / `frozen_api_surface()`, and the router is built
/// from that fold — so the set compared here is the set mounted, not a
/// second copy of it. [`fold_live`] subtracts `withdrawn` events, so a
/// route taken back (issue #51 retired `POST /v1/molecules/:id/land`)
/// is absent here exactly as it is absent from the server.
fn live_routes() -> Vec<LiveRoute> {
    let path = canon_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let logged = parse_canon(&raw, &path.display().to_string())
        .unwrap_or_else(|err| panic!("parse {}: {err}", path.display()));
    let live = fold_live(&logged).unwrap_or_else(|err| panic!("fold {}: {err}", path.display()));
    live.iter()
        .map(|ev| {
            let (method, route) = ev
                .method_path
                .split_once(' ')
                .unwrap_or_else(|| panic!("canon entry {:?} is not `METHOD PATH`", ev.method_path));
            LiveRoute {
                key: format!("{method} {}", normalise_path(route)),
                exposure: ev.exposure,
            }
        })
        .collect()
}

/// Tenant-verb routes that deliberately have **no** `cs` counterpart,
/// and therefore no row in a registry whose alphabet is `cs` verbs.
///
/// These six are the D-AVATAR canals (ADR-0020) — `converse` plus the
/// five instance-lifecycle routes. They are tenant
/// verbs of the *thin* client — `cosmon-remote` carries them, and
/// `cosmon-thin-cli::verbs` declares their `#[verb]` stubs — but `cs`
/// has no `avatar` subcommand and will not grow one: « avatar est un
/// mot de doctrine, jamais un nom d'API » (tenant guide §12.2), and the
/// top-level client verb is `converse`, not `avatar converse`.
///
/// They are enumerated route by route rather than skipped by a
/// `/v1/avatar/` prefix rule. A wildcard would silently absorb the next
/// avatar route somebody mounts; a list makes adding one a line in this
/// file that a reviewer reads.
const TENANT_VERB_ROUTES_OUTSIDE_THE_CS_ALPHABET: &[&str] = &[
    "POST /v1/avatar/converse",
    "GET /v1/avatar/:instance_id/status",
    "POST /v1/avatar/:instance_id/incarnate",
    "POST /v1/avatar/:instance_id/grant",
    "GET /v1/avatar/:instance_id/audit",
    "GET /v1/avatar/:instance_id/mould-info",
];

/// Verbs the registry MUST mark as `**NO (NEVER)**` (or equivalent
/// hard-NEVER form) per ADR-080 §5.1. The audit gate refuses to admit
/// any axum route for these verbs.
const NEVER_VERBS: &[&str] = &[
    // `done` left the NEVER class 2026-09-07 (issue #51, ADR-080 §5.4):
    // closing a molecule is the last step of its normal lifecycle, not an
    // administration surface, and `POST /v1/molecules/:id/done` exposes it
    // under its own name with the full `cs done` parameter set. What stays
    // restricted is WHICH molecules a requester may close — the
    // multi-tenant question, deliberately unanswered.
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
fn exemption_list_names_only_live_tenant_verb_routes() {
    // The exemption list is a blind spot written down. It must stay one:
    // an entry for a route that was withdrawn, reclassified adapter-only,
    // or never existed would silently widen the hole it documents.
    let live: BTreeMap<String, Exposure> = live_routes()
        .into_iter()
        .map(|r| (r.key, r.exposure))
        .collect();
    let mut stale = Vec::new();
    for exempt in TENANT_VERB_ROUTES_OUTSIDE_THE_CS_ALPHABET {
        match live.get(*exempt) {
            Some(Exposure::TenantVerb) => {}
            Some(other) => stale.push(format!("{exempt} is `{other}`, not a tenant verb")),
            None => stale.push(format!("{exempt} is not a live route")),
        }
    }
    assert!(
        stale.is_empty(),
        "TENANT_VERB_ROUTES_OUTSIDE_THE_CS_ALPHABET has rotted — remove \
         the entries that no longer describe a live tenant-verb route:\n  {}",
        stale.join("\n  ")
    );
}

#[test]
fn every_shipped_row_names_a_live_route() {
    let registry = parse_registry();
    let live: Vec<String> = live_routes().into_iter().map(|r| r.key).collect();

    let mut violations = Vec::new();
    for row in &registry {
        if !row.claims_shipped() {
            continue;
        }
        if row.api_routes.is_empty() {
            violations.push(format!(
                "cs {} is marked `{}` but its API path column names no \
                 route (found {:?})",
                row.verb, row.exposed, row.api_path
            ));
            continue;
        }
        for route in &row.api_routes {
            if !live.contains(route) {
                violations.push(format!(
                    "cs {} is marked `{}` and points at `{route}`, which \
                     the adapter does not serve — correct the path or \
                     demote the row to `TBD`",
                    row.verb, row.exposed
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "docs/guides/api-cli-coverage.md promises routes that are not on \
         the §8p surface canon ({}):\n  {}",
        canon_path().display(),
        violations.join("\n  ")
    );
}

#[test]
fn every_cs_alphabet_route_is_declared_shipped() {
    let registry = parse_registry();
    let mut missing = Vec::new();

    for route in live_routes() {
        // Adapter-only routes have no `cs` verb by construction — see
        // the module docs. The exemption list covers the tenant verbs
        // that are `cosmon-remote`'s alone.
        if route.exposure != Exposure::TenantVerb
            || TENANT_VERB_ROUTES_OUTSIDE_THE_CS_ALPHABET.contains(&route.key.as_str())
        {
            continue;
        }
        let naming: Vec<&RegistryRow> = registry
            .iter()
            .filter(|row| row.api_routes.iter().any(|r| *r == route.key))
            .collect();
        if naming.iter().any(|row| row.claims_shipped()) {
            continue;
        }
        let seen = if naming.is_empty() {
            "no row names it".to_string()
        } else {
            naming
                .iter()
                .map(|row| format!("`cs {}` says `{}`", row.verb, row.exposed))
                .collect::<Vec<_>>()
                .join(", ")
        };
        missing.push(format!("{} — {seen}", route.key));
    }

    assert!(
        missing.is_empty(),
        "these routes are live on the adapter but docs/guides/\
         api-cli-coverage.md does not declare them shipped (promote the \
         row, or withdraw the route in the surface canon):\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn no_refused_row_names_a_live_route() {
    let registry = parse_registry();
    let live: Vec<String> = live_routes().into_iter().map(|r| r.key).collect();

    let mut breaches = Vec::new();
    for row in &registry {
        if !row.denies_exposure() {
            continue;
        }
        for route in &row.api_routes {
            if live.contains(route) {
                breaches.push(format!(
                    "cs {} is marked `{}` yet `{route}` is live",
                    row.verb, row.exposed
                ));
            }
        }
    }

    assert!(
        breaches.is_empty(),
        "§8p breach — the registry refuses a verb the adapter serves. \
         File a bead, do not patch the registry to make the breach \
         pass:\n  {}",
        breaches.join("\n  ")
    );
}
