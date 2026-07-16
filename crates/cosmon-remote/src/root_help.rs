// SPDX-License-Identifier: AGPL-3.0-only

//! Long-form help text attached to the `cosmon-remote` clap tree.
//!
//! Same layout discipline as `cosmon-cli/src/root_help.rs`: the
//! narrative blocks live in one module, referenced from
//! `#[command(after_long_help = ...)]` attributes, so `--help` AND the
//! generated man page (`__man-page` → `clap_mangen`) render them from
//! ONE source. This drains the hand-written `render_root_help` blocks
//! of the former `cs-thin help` (TYPICAL WORKFLOW, AUTHENTICATION,
//! EXIT CODES) into the clap tree: written-once content
//! is *attached to the command*, never printed beside it.
//!
//! Boundary: nothing here describes what a *formula*
//! does. The help documents the call; formula semantics are tenant
//! data, outside the frozen surface, discovered server-side.

/// Golden-path epilogue — the four gestures of
/// the first hour, then the diagnostic verbs in their own paragraph so
/// they never read as steps of the journey. Written ONCE here; the
/// short help (`-h`) shows it alone, the long help (`--help`) and the
/// man page open with it (one source, two readers).
///
/// The command examples render under the **invoked** name (`bin`): the
/// installer poses `cosmon` as an alias and the epilogue must echo what
/// the operator actually typed, never a hand-pinned long name (P4 — one
/// name, sourced from `invoked_name()`; the man page passes the
/// canonical `cosmon-remote`).
fn first_steps(bin: &str) -> String {
    format!(
        "First steps (golden path):
  1. {bin} doctor                          verify the install (green/red checks)
  2. {bin} auth login --email <you>        connect the Claude worker (once)
  3. {bin} molecule nucleate task-work --topic \"…\" --kind task
     {bin} molecule tackle <id>            start the work (costs credit)
  4. {bin} molecule result <id>            fetch the deliverable

Diagnostics (when something breaks, not before):
  doctor · healthz · auth me · quota · workers list · noyaux list"
    )
}

/// Root `after_help` — the short (`-h`) epilogue: the golden path
/// alone, nothing else, under the invoked `bin` name.
#[must_use]
pub fn after_help(bin: &str) -> String {
    first_steps(bin)
}

/// Root `after_long_help` — rendered by `--help` and by the man page's
/// extra section, from this one source. Opens with the same golden
/// path as `-h` (under the invoked `bin` name), then the man-style
/// sections.
#[must_use]
pub fn after_long_help(bin: &str) -> String {
    format!("{}\n\n{LONG_HELP_SECTIONS}", first_steps(bin))
}

/// The man-style sections of the long help. Name-independent prose, so
/// it stays a single constant; only the golden-path examples above vary
/// with the invoked name. The `[coûteux]`/`[irréversible]` tokens are
/// the canonical effect markers derived from OAuth scopes
/// (`cosmon_surface_canon`), referenced verbatim here so the prose
/// matches what the verbs render.
const LONG_HELP_SECTIONS: &str = "\
AUTHENTICATION — two independent badges:\n  \
1. The API badge (JWT). Minted automatically from the profile's\n     \
oidc_url when needed, or supplied via --token / $COSMON_REMOTE_TOKEN.\n  \
2. The worker badge (Claude credential). Posed once with `auth login`\n     \
(a guided three-step flow — see `auth login --help`). Without it,\n     \
a tackled worker has nothing to spend.\n\n\
EFFECT MARKERS:\n  \
Verbs marked [coûteux] burn real Anthropic credit; verbs marked\n  \
[irréversible] cannot be undone. The marker is DERIVED from the OAuth\n  \
scope the route requires (one distinct scope per costly or\n  \
irreversible effect — ADR-080 §6.5), never from hand-written prose.\n\n\
FORMULAS — this help documents the call, never the formula:\n  \
`molecule nucleate <formula>` passes an opaque server-side name. What\n  \
a formula DOES (which steps it runs, which children it nucleates,\n  \
what it produces) is content of the deployment you target — NOT part\n  \
of the frozen API surface and NOT documented here or in the man page.\n  \
Discover the catalogue on the instance itself (its served docs, or\n  \
ask its operator); an unknown formula is refused with 404.\n\n\
EXIT CODES:\n  \
0   success\n  \
1   API or transport error (anything not listed below)\n  \
2   configuration error (profile missing or incomplete)\n  \
3   authentication flow error (PKCE)\n  \
4   authorization refused (HTTP 401/403)\n  \
5   not found (HTTP 404)\n\n\
SEE ALSO:\n  \
man cosmon-remote     # this same clap tree, rendered as a man page\n  \
cosmon-rpp API reference (smithy docs/specs/) — the wire-level view\
";

/// `auth login` `after_long_help` — the multi-step PKCE flow, written
/// once, rendered in `auth login --help` and the man page.
pub const AUTH_LOGIN_AFTER_LONG_HELP: &str = "\
WORKFLOW — login drives the three wire steps for you, in order:\n  \
1. start     opens a PKCE session on the adapter\n  \
2. email     submits your e-mail; the adapter returns a verification URL\n  \
3. confirm   you open the URL, authorize, paste the code back\n\n\
The pasted code travels only to your own adapter. Re-running login\n\
replaces the previously posed credential. Inspect or abort an\n\
in-flight session with `auth status <session_id>` / `auth logout\n\
<session_id>`.\
";

/// `molecule nucleate` `after_long_help` — nucleate's place in the
/// three-verb chain plus the formula-opacity boundary.
pub const NUCLEATE_AFTER_LONG_HELP: &str = "\
WORKFLOW — nucleate is step one of three:\n  \
nucleate <formula> ...   create the molecule (PENDING — nothing runs yet)\n  \
tackle <id>              dispatch a worker on it [coûteux]\n  \
result <id>              fetch the canonical deliverable when it lands\n\n\
The <formula> name is opaque to this CLI: its semantics (steps,\n\
children, outputs) live with the deployment and are not frozen with\n\
the API surface — discover the catalogue on the instance you target.\n\
An unknown formula is refused with 404.\
";
