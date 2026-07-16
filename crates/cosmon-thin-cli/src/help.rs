// SPDX-License-Identifier: Apache-2.0

//! `cs-thin help` — operator-facing help surface symmetric to `cs help`.
//!
//! With no arguments, prints a structured overview of the cs-thin
//! command surface (the §8p RPP-exposable subset). With a verb name,
//! delegates to the per-subcommand `--help` rendered by clap.
//!
//! The intent is **UX parity** with `cs help` (see
//! `crates/cosmon-cli/src/cmd/help.rs`): the operator who knows
//! `cs help observe` should reach for `cs-thin help observe` and
//! receive a comparable experience. Section vocabulary, indentation
//! conventions, and pedagogical tone are aligned on purpose.
//!
//! The asymmetry that matters — the verbs cs-thin **cannot** carry
//! over the wire (`done`, `evolve`, `complete`, `security`, `run`,
//! `kill`, `purge`, `reconcile`, `verify`, `whisper`, `drop`) — is
//! surfaced explicitly under an `OPERATOR-ONLY` block citing
//! ADR-080 §5.1. operator-demo or any external auditor scanning the help
//! must see the boundary, not just the covered subset.
//!
//! NB: `tackle` was previously operator-only; remote-tackle V2
//! promoted it to a wire-exposed
//! `cosmon:molecule:write` route — the only §8p verb whose handler
//! is subprocess-based rather than library-direct, since spawning an
//! external agent (Claude Code via tmux) is fundamentally
//! out-of-process.

use std::process;

use clap::CommandFactory;

use crate::cli::{Cli, HelpArgs};
use crate::coverage::OPERATOR_ONLY;

/// Render the top-level grouped reference (`cs-thin help` with no
/// argument).
///
/// Layout mirrors `cs help`:
/// 1. headline + version
/// 2. typical workflow (5-line happy path)
/// 3. exposed verbs (HTTP method × path × scope)
/// 4. operator-only verbs (refused by §8p, with ADR pointer)
/// 5. authentication knobs (env vars + flags)
/// 6. exit codes
/// 7. SEE ALSO pointers
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn render_root_help() -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let v = crate::VERSION;

    let _ = writeln!(
        out,
        "cs-thin {v} — mechanical HTTP client for the §8p RPP-exposable cosmon verb subset\n"
    );
    let _ = writeln!(out, "Usage: cs-thin [OPTIONS] <COMMAND>");
    out.push('\n');

    out.push_str("TYPICAL WORKFLOW — JWT in, JSON out:\n");
    out.push_str("  export JWT=$(curl -s $IDP/issue?... | jq -r .access_token)\n");
    out.push_str("  export CS_THIN_BASE_URL=https://api.cosmon.example\n");
    out.push_str("  cs-thin observe task-YYYYMMDD-xxxx          # GET, returns molecule JSON\n");
    out.push_str("  cs-thin nucleate --formula task-work --var topic=hello --tag temp:warm\n");
    out.push_str("  cs-thin tag <id> --add temp:hot --remove temp:warm\n");
    out.push('\n');
    out.push_str("Each invocation is a single HTTP round-trip — there is no daemon, no\n");
    out.push_str("local state, and no JWT cache. The token lives in the env var or in\n");
    out.push_str("--jwt-file and dies with the process.\n");
    out.push('\n');

    out.push_str("THE 7 VERBS — RPP-exposable (ADR-080 §8p):\n");
    let mut verbs: Vec<_> = crate::registry::all().iter().collect();
    verbs.sort_by_key(|d| d.name);
    for d in &verbs {
        let scope = scope_label(d);
        let _ = writeln!(
            out,
            "  {:<10} {:<6} {:<32} {scope}",
            d.name, d.method, d.path,
        );
    }
    out.push('\n');

    out.push_str("OPERATOR-ONLY VERBS — refused by §8p (see ADR-080 §5.1):\n");
    out.push_str("  These verbs exist on local `cs` but DO NOT cross the wire. Their\n");
    out.push_str("  blast-radius (worktree teardown, branch deletion, fleet kill) is\n");
    out.push_str("  incompatible with a JWT-bearing remote principal. cs-thin will\n");
    out.push_str("  never grow a sub-command for them — by design.\n\n");
    for entry in OPERATOR_ONLY {
        let suffix = match entry.note {
            Some(n) => format!(" — {n}"),
            None => String::new(),
        };
        let _ = writeln!(
            out,
            "  ⚠ {:<22} operator-only ({}){suffix}",
            entry.name, entry.adr_ref,
        );
    }
    out.push('\n');

    out.push_str("AUTHENTICATION:\n");
    out.push_str("  --base-url <URL>            target endpoint (overrides CS_THIN_BASE_URL)\n");
    out.push_str("  --jwt-from-env <NAME>       env var to read JWT from (default: JWT)\n");
    out.push_str("  --jwt-file <PATH>           read JWT from disk (single-line ASCII)\n");
    out.push('\n');
    out.push_str("  Env: CS_THIN_BASE_URL, JWT (renameable via --jwt-from-env).\n");
    out.push_str("  --jwt-from-env and --jwt-file are mutually exclusive.\n");
    out.push('\n');

    out.push_str("EXIT CODES:\n");
    out.push_str("  0   success — JSON written to stdout\n");
    out.push_str("  1   backend error (HTTP 4xx/5xx, validation, decode)\n");
    out.push_str("  2   network error (DNS, TCP, TLS) — same shape as `curl 7`\n");
    out.push_str("  3   JWT missing or empty\n");
    out.push('\n');

    out.push_str("COVERAGE REPORT — proof that every annotated verb has dispatch:\n");
    out.push_str("  cs-thin --coverage-report --json   # machine-readable, CI gate\n");
    out.push_str("  cs-thin verbs --check              # human-readable screenshot form\n");
    out.push('\n');

    out.push_str("SEE ALSO:\n");
    out.push_str("  cs help              # the rich operator-side companion (local CLI)\n");
    out.push_str("  cosmon-remote --help # the installed tenant binary (A2 fusion)\n");
    out.push_str("  ADR-080 §5.1         # docs/adr/080-remote-pilot-port-https-oidc.md\n");
    out.push_str(
        "  Onboarding (FR)      # dist/tenant-demo-handover/docs/onboarding/cs-thin-quickstart.md\n",
    );
    out.push('\n');

    out.push_str("Run 'cs-thin help <verb>' for detailed flags and arguments.\n");
    out.push_str("Run 'cs-thin verbs' to list registered verbs (link-time slice).\n");

    out
}

/// JWT scope hint per verb, read from the §8p surface canon
/// ([`crate::surface_scopes::SURFACE_SCOPES`], folded at build time
/// from `data/surface_events.txt`). Replaces the former hand-mapped
/// `scope_for` mirror — the scope a verb prints
/// is the scope its canon line declares. A registered verb with no
/// canon line renders the catch-all and is caught by
/// [`tests::scope_map_covers_every_registered_verb`].
fn scope_label(d: &crate::registry::VerbDescriptor) -> String {
    match crate::surface_scopes::scope_for(d.method, d.path) {
        Some(scope) => format!("scope: {scope}"),
        None => "scope: (unmapped — append the route to data/surface_events.txt)".to_owned(),
    }
}

/// Run the help command (`cs-thin help [<verb>]`).
///
/// - No arg → write [`render_root_help`] to stdout, exit 0.
/// - With arg → re-exec `cs-thin <verb> --help` so clap renders the
///   per-subcommand detailed help. Special: the synthetic verbs
///   `verbs` and `help` route through the clap tree the same way.
///
/// # Errors
///
/// Returns [`crate::CliError::Local`] if the re-exec fails. The
/// no-arg branch is infallible (writes to the supplied sink).
pub fn run_help<W: std::io::Write>(args: &HelpArgs, out: &mut W) -> Result<(), crate::CliError> {
    match args.command.as_deref() {
        None => {
            let body = render_root_help();
            out.write_all(body.as_bytes())
                .map_err(|e| crate::CliError::Local(e.to_string()))?;
            Ok(())
        }
        Some(verb) => delegate_to_subcommand(verb),
    }
}

/// Re-exec the running binary as `cs-thin <verb> --help` so clap
/// generates the detailed view from the `#[arg]` attributes.
fn delegate_to_subcommand(verb: &str) -> Result<(), crate::CliError> {
    // Validate the verb is known to the clap tree before exec'ing —
    // otherwise we'd shell out to a process that immediately errors,
    // and the operator gets a less helpful message.
    let root = Cli::command();
    if root.find_subcommand(verb).is_none() {
        return Err(crate::CliError::Local(format!(
            "unknown verb `{verb}` — try `cs-thin help` for the list",
        )));
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| "cs-thin".into());
    let status = process::Command::new(exe)
        .args([verb, "--help"])
        .status()
        .map_err(|e| {
            crate::CliError::Local(format!("failed to exec cs-thin {verb} --help: {e}"))
        })?;
    process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_help_mentions_every_section_header() {
        let body = render_root_help();
        for header in [
            "TYPICAL WORKFLOW",
            "THE 7 VERBS",
            "OPERATOR-ONLY VERBS",
            "AUTHENTICATION",
            "EXIT CODES",
            "SEE ALSO",
        ] {
            assert!(body.contains(header), "missing section: {header}");
        }
    }

    #[test]
    fn root_help_lists_operator_only_verbs_with_adr_pointer() {
        let body = render_root_help();
        assert!(body.contains("ADR-080 §5.1"));
        for entry in OPERATOR_ONLY {
            assert!(
                body.contains(entry.name),
                "missing operator-only verb: {}",
                entry.name
            );
        }
    }

    #[test]
    fn scope_map_covers_every_registered_verb() {
        // A drift here means a #[verb] was annotated without a
        // matching line in data/surface_events.txt — the help would
        // render the catch-all and the operator would not know which
        // JWT scope they need. (The §8p bijection test in
        // cosmon-rpp-adapter fails first in CI, but this keeps the
        // help's own contract self-contained.)
        for d in crate::registry::all() {
            let s = scope_label(d);
            assert!(
                !s.contains("unmapped"),
                "verb `{}` ({} {}) has no canon line in data/surface_events.txt",
                d.name,
                d.method,
                d.path,
            );
        }
    }
}
