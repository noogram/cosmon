// SPDX-License-Identifier: AGPL-3.0-only

//! Pane-signature registry — adapter name → set of accepted
//! `pane_current_command` values.
//!
//! # Why
//!
//! ADR-079 §6 names the §-leak in ADR-038 §5 (propulsion) and §6
//! (whisper): both perturbation gates compared the live worker's
//! foreground command against the hard-coded literal `"claude"`. A
//! second Adapter (`aider`, an API adapter, …) would silently fail
//! the gate even with a functioning worker pane, because the literal
//! does not name it.
//!
//! The registry is the indirection that replaces the literal. Each
//! Adapter registers its accepted pane signatures at construction
//! time; the gates look up the calling Worker's Adapter and check
//! the observed `pane_current_command` against the registered list.
//! ADR-097 PR-2 — the §-leak repair under IFBDD — pairs the lookup
//! with an [`EventV2::AdapterPaneSignatureChecked`](cosmon_core::event_v2::EventV2::AdapterPaneSignatureChecked)
//! emission on every check so that registration drift becomes
//! visible in `events.jsonl` without scraping the source.
//!
//! As of C4 the registry knows two Adapters:
//! - `claude` with signatures `["claude", "claude*", "node",
//!   "<version>"]` — the bare name, a prefix glob for wrapper/renamed
//!   installs, the Node.js entry-point fallback, and the
//!   [`VERSION_SENTINEL`] for the native binary's version-as-`comm`
//!   quirk (observed 2026-06-12: a real worker reported
//!   `pane_current_command=2.1.175`).
//! - `aider` with signatures `["aider", "python", "python3",
//!   "python3.10", "python3.11", "python3.12", "python3.13"]` — the
//!   set of pane names `pane_current_command` may report depending
//!   on how `aider-chat` was installed (`uv tool install` exposes
//!   `aider`; `pip install` inside a venv may show the underlying
//!   Python interpreter the entry script execs into). The list is
//!   explicit (no regex) so C6's TOML override has a stable shape
//!   to overwrite.
//!
//! The TOML-driven loader against
//! `.cosmon/config.toml::[adapters.<name>]` lands in C6 (PR-5);
//! until then the in-code [`default_registry`] is the singleton.

use std::collections::HashMap;
use std::process::Command;

/// Sentinel signature that matches any bare version-string pane command
/// (e.g. `2.1.175`).
///
/// # Why this exists
///
/// The `claude` CLI is now shipped as a self-contained native binary
/// that surfaces its **own version** as the pane's foreground command
/// name: a real worker observed on 2026-06-12 reported
/// `pane_current_command=2.1.175` rather than `claude`. The version
/// component changes with every release, so it cannot be enumerated in
/// the explicit signature list. This sentinel lets an Adapter declare
/// "accept any version-shaped token" once, instead of chasing the
/// version number on every `claude` upgrade.
///
/// The token is angle-bracketed so it can never collide with a real
/// process name (`<` is not legal in an executable's `comm`), which
/// keeps the C6 TOML override shape unambiguous.
pub const VERSION_SENTINEL: &str = "<version>";

/// `true` when `s` looks like a bare dot-separated version string such
/// as `2.1.175` — two or more non-empty all-ASCII-digit components.
///
/// Rejects anything with a non-numeric component (`claude`, `node`,
/// `2.1.x`), a leading/trailing/double dot (`2.`, `.2`, `2..1`), and the
/// empty string. Deliberately strict: the only goal is to recognise the
/// `claude` native binary's version-as-`comm` quirk without widening the
/// gate to arbitrary tokens.
#[must_use]
fn looks_like_version(s: &str) -> bool {
    let mut components = 0usize;
    for part in s.split('.') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        components += 1;
    }
    components >= 2
}

/// Strip a leading environment-assignment prefix (`VAR=value …`) from an
/// observed pane command and return the real binary token, basename'd.
///
/// # Why
///
/// Identity-pinned workers (delib-20260717-194b F3) are spawned with an
/// env prefix on the shell command — e.g.
/// `GIT_AUTHOR_NAME='Emmanuel Sérié' GIT_AUTHOR_EMAIL=… RUST_LOG=error
/// codex --yolo`. On some tmux/platform combinations
/// `pane_current_command` surfaces that full command line (or its first
/// token) instead of the foreground process `comm`, so the propulsion /
/// whisper gates observed `GIT_AUTHOR_NAME=Emmanuel…` and refused a
/// perfectly live codex worker (friction 2026-07-18, task-20260718-912b).
///
/// The parser walks shell words left to right, skipping `IDENT=value`
/// assignments (single- and double-quoted values with embedded spaces
/// included) and a bare `env` launcher, then returns the first real
/// command token with any directory prefix removed. A plain `comm` value
/// (`codex`, `node`, `zsh`, `2.1.175`) passes through unchanged, so the
/// gate's refusal semantics for crashed-into-shell panes are untouched.
#[must_use]
pub fn effective_pane_command(raw: &str) -> String {
    let mut rest = raw.trim_start();
    loop {
        let (word, remainder) = next_shell_word(rest);
        if word.is_empty() {
            return String::new();
        }
        if is_env_assignment(&word) || word == "env" {
            rest = remainder.trim_start();
            continue;
        }
        return word.rsplit('/').next().unwrap_or(word.as_str()).to_owned();
    }
}

/// Extract the next shell word from `input`, honouring single and double
/// quotes (a quoted region may contain whitespace without ending the
/// word). Returns the word with its quote characters stripped, plus the
/// unconsumed remainder. No escape-sequence handling — the spawn seam
/// quotes with `shell_escape` (single quotes), which this covers.
fn next_shell_word(input: &str) -> (String, &str) {
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = input.char_indices();
    for (i, c) in chars.by_ref() {
        match quote {
            Some(q) if c == q => quote = None,
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c.is_whitespace() => return (word, &input[i..]),
            Some(_) | None => word.push(c),
        }
    }
    (word, "")
}

/// `true` when a shell word is an environment assignment (`IDENT=…` with
/// a POSIX-shaped identifier before the `=`).
fn is_env_assignment(word: &str) -> bool {
    let Some(eq) = word.find('=') else {
        return false;
    };
    let ident = &word[..eq];
    let mut bytes = ident.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `true` when `pane_cmd` satisfies a single registered `signature`.
///
/// Three signature shapes are understood (checked in this order):
///
/// 1. the [`VERSION_SENTINEL`] (`<version>`) matches any version-shaped
///    token — see [`looks_like_version`];
/// 2. a trailing-`*` glob (`claude*`) matches any pane command sharing
///    that prefix — covers wrapper scripts and renamed binaries
///    (`claude-code`, `claude-wrapper`, …);
/// 3. otherwise an exact string match (the original behaviour).
///
/// The grammar is intentionally tiny — no general regex — so the C6
/// `[adapters.<name>].pane_signatures` TOML loader keeps a stable,
/// human-auditable shape.
#[must_use]
fn signature_matches(signature: &str, pane_cmd: &str) -> bool {
    if signature == VERSION_SENTINEL {
        return looks_like_version(pane_cmd);
    }
    if let Some(prefix) = signature.strip_suffix('*') {
        return pane_cmd.starts_with(prefix);
    }
    signature == pane_cmd
}

/// Static lookup table from adapter name → registered
/// `pane_current_command` signatures.
///
/// Insertion order is irrelevant — the lookup is by `signature_matches`
/// against each registered signature (exact, trailing-`*` prefix glob, or
/// the [`VERSION_SENTINEL`]). Cloning is cheap (one small `HashMap`) so
/// call sites that need a snapshot of `signatures_of` should clone the
/// slice rather than the whole registry.
#[derive(Debug, Clone, Default)]
pub struct PaneSignatureRegistry {
    entries: HashMap<String, Vec<String>>,
}

impl PaneSignatureRegistry {
    /// Construct an empty registry. Callers populate it via
    /// [`Self::register`] before the first lookup.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Register `adapter_name` with the supplied pane-signature list.
    ///
    /// A second `register` call for the same adapter overwrites the
    /// first — this mirrors the TOML-driven future loader, where the
    /// last `[adapters.<name>]` table wins.
    pub fn register(&mut self, adapter_name: impl Into<String>, signatures: Vec<String>) {
        self.entries.insert(adapter_name.into(), signatures);
    }

    /// `true` when `pane_cmd` matches at least one signature registered
    /// for `adapter_name`. Returns `false` for unknown adapters and for
    /// the empty `pane_cmd` (a tmux query that returned nothing is not
    /// a match — the worker pane may have died).
    ///
    /// The observed value is first normalised through
    /// [`effective_pane_command`] so an env-assignment spawn prefix
    /// (`GIT_AUTHOR_NAME=… codex …`, the F3 identity pinning) cannot mask
    /// the real binary from the gate. Each registered signature is then
    /// evaluated with `signature_matches`, so a literal (`claude`), a
    /// trailing-`*` prefix glob (`claude*`), and the [`VERSION_SENTINEL`]
    /// all participate in the check.
    #[must_use]
    pub fn matches(&self, adapter_name: &str, pane_cmd: &str) -> bool {
        let effective = effective_pane_command(pane_cmd);
        if effective.is_empty() {
            return false;
        }
        self.entries
            .get(adapter_name)
            .is_some_and(|sigs| sigs.iter().any(|s| signature_matches(s, &effective)))
    }

    /// Return the registered signatures for `adapter_name`, or an empty
    /// slice when the adapter is unknown.
    ///
    /// The borrow is fine for read-only audit-event construction; cloning
    /// is the caller's responsibility when the slice needs to outlive
    /// the registry borrow.
    #[must_use]
    pub fn signatures_of(&self, adapter_name: &str) -> &[String] {
        self.entries.get(adapter_name).map_or(&[], Vec::as_slice)
    }
}

/// How an Adapter's worker is supervised post-spawn.
///
/// **Migrated to [`cosmon_core::spawn_seam::SupervisionMode`] under
/// ADR-103.** This module re-exports the canonical type so existing
/// observer-side imports (`cs ensemble`, `cs peek`, the ghost
/// detector) keep compiling. The helper [`supervision_mode_for`]
/// remains as a thin wrapper around
/// [`cosmon_core::spawn_seam::axes_for_built_in`] so its callers can
/// migrate to the validator-driven triple on their own cadence.
pub use cosmon_core::spawn_seam::SupervisionMode;

/// Return the [`SupervisionMode`] for `adapter_name`.
///
/// Defaults to [`SupervisionMode::TmuxPane`] for unknown names so legacy
/// or third-party adapters keep the pre-ADR-100 contract.
///
/// **ADR-103 / shim.** The authoritative source is
/// [`cosmon_core::spawn_seam::validate_adapter_name`], which returns
/// the per-Adapter triple `(name, supervision, ownership)` at
/// validation time. New code paths should thread the triple rather
/// than re-deriving it from a string here; this helper exists so the
/// observer-side call sites (which only have the string) can migrate
/// incrementally.
#[must_use]
pub fn supervision_mode_for(adapter_name: &str) -> SupervisionMode {
    cosmon_core::spawn_seam::axes_for_built_in(adapter_name)
        .map_or(SupervisionMode::TmuxPane, |(s, _)| s)
}

/// Default in-code registry. Registers the `claude`, `aider`, `codex`,
/// `openai`, `anthropic`, and `llama-cpp` Adapter names (plus the
/// `llama` legacy alias). C6 (PR-5) supersedes this with a
/// `[adapters.<name>].pane_signatures` loader against
/// `.cosmon/config.toml`.
///
/// `openai`, `anthropic`, and `llama-cpp` (ADR-100 R2) are
/// **in-process** adapters — they have no
/// real tmux pane. The sentinel name is registered so the propulsion /
/// whisper gates can branch on `adapter_name in (in-process set)` and
/// skip the pane probe rather than silently mismatching on the empty
/// pane query. The authoritative answer to "does this adapter use
/// tmux?" lives in [`supervision_mode_for`] — observer-side code reads
/// it there.
#[must_use]
pub fn default_registry() -> PaneSignatureRegistry {
    let mut r = PaneSignatureRegistry::new();
    // The `claude` worker pane may surface its foreground command in
    // three different shapes, all of which must pass the gate:
    //   - `claude`            — the historical bare-binary name;
    //   - `claude*` (prefix)  — wrapper scripts / renamed installs
    //                           (`claude-code`, `claude-wrapper`, …);
    //   - `node`              — when the entry point execs into Node.js
    //                           (same quirk as the codex adapter);
    //   - `<version>`         — the native binary reports its OWN version
    //                           as the pane `comm` (observed 2026-06-12 on
    //                           a real worker: `pane_current_command=2.1.175`).
    // The version component changes every release, so it is matched by the
    // `VERSION_SENTINEL` pattern rather than enumerated. `claude*` already
    // covers the bare `claude` literal, but the literal is kept explicit so
    // the error message and `signatures_of` audit read clearly.
    r.register(
        crate::claude::ADAPTER_NAME,
        vec![
            crate::claude::ADAPTER_NAME.to_owned(),
            format!("{}*", crate::claude::ADAPTER_NAME),
            "node".to_owned(),
            VERSION_SENTINEL.to_owned(),
        ],
    );
    // `pip install aider-chat` may surface as `python` or `python3.x`
    // in `pane_current_command` depending on installer (`uv tool` vs
    // `pipx` vs raw `pip`). The list covers both the direct-binary
    // case and the venv-wrapped case so the propulsion / whisper
    // gate matches either way.
    r.register(
        crate::aider::ADAPTER_NAME,
        vec![
            crate::aider::ADAPTER_NAME.to_owned(),
            "python".to_owned(),
            "python3".to_owned(),
            "python3.10".to_owned(),
            "python3.11".to_owned(),
            "python3.12".to_owned(),
            "python3.13".to_owned(),
        ],
    );
    // `npm install -g @openai/codex` exposes the binary as `codex`,
    // but the underlying Node.js entry point may surface in
    // `pane_current_command` as `node`. Both names are registered so
    // the propulsion / whisper gates match either way (ADR-098 §C3:
    // pane signatures are an Adapter-name concern, not a
    // supervision-mode concern). `codex*` (prefix glob) mirrors claude's
    // `claude*` — a wrapper script or renamed install (`codex-cli`, …)
    // still passes the steerable-worker whisper gate (task-20260711-246d,
    // interactive codex parity).
    r.register(
        crate::codex::ADAPTER_NAME,
        vec![
            crate::codex::ADAPTER_NAME.to_owned(),
            format!("{}*", crate::codex::ADAPTER_NAME),
            "node".to_owned(),
        ],
    );
    // `opencode` (sst/opencode) installs as `opencode`; the binary is a
    // Bun/Node entry point so `pane_current_command` may surface `node` or
    // `bun` instead. All three are registered so the propulsion / whisper
    // gates match either way (ADR-098 §C3: pane signatures are an
    // Adapter-name concern, not a supervision-mode concern). External-CLI
    // sibling of codex — delib-20260615-73f9 / ADR-125.
    r.register(
        crate::opencode::ADAPTER_NAME,
        vec![
            crate::opencode::ADAPTER_NAME.to_owned(),
            "node".to_owned(),
            "bun".to_owned(),
        ],
    );
    // In-process Direct-API adapters (ADR-100). Single sentinel each;
    // dispatch site reads the sentinel to skip tmux readiness.
    r.register("openai", vec!["openai".to_owned()]);
    r.register("anthropic", vec!["anthropic".to_owned()]);
    // In-process llama.cpp adapter (C3 of `delib-20260519-a20b`,
    // ADR-103 / ADR-104). Path A v0 — the loop runs inside cosmon
    // via the `cosmon-llama` FFI library; no tmux pane exists, the
    // sentinel `"llama-cpp"` is registered so the propulsion /
    // whisper gates skip the pane probe by adapter name. The bare
    // `llama` row is the legacy alias preserved for operator
    // vocabulary (tolnay's name-stability table, delib synthesis
    // §B.2 D4) — same sentinel shape, different public name.
    r.register("llama-cpp", vec!["llama-cpp".to_owned()]);
    r.register("llama", vec!["llama".to_owned()]);
    r
}

/// Query `pane_current_command` for the named tmux session on the
/// given socket.
///
/// Returns `None` when tmux fails (dead session, wrong socket) or the
/// foreground command is empty. The gates treat both cases as a
/// non-match — the same way the original `validate_target` mapped a
/// failed tmux call to `observed = "<missing>"`.
///
/// This helper exists at the transport layer so both the propulsion
/// gate (`cosmon_cli::cmd::patrol`) and the whisper gate
/// (`cosmon_cli::cmd::whisper`) share one implementation. Both used
/// to shell out to tmux independently; the duplication was the
/// silent-skew risk forgemaster §8 names.
#[must_use]
pub fn pane_current_command(socket: &str, session: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args([
            "-L",
            socket,
            "list-panes",
            "-t",
            session,
            "-F",
            "#{pane_current_command}",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let first = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    if first.is_empty() {
        None
    } else {
        Some(first)
    }
}

/// How long the named tmux session's pane has produced **no output**, in
/// seconds, or `None` when tmux cannot answer (dead session, wrong socket,
/// unparsable format).
///
/// # Why a duration and not a capture
///
/// This is the transport clock the propulsion admission control
/// ([`cosmon_core::propel`]) reads to tell a *thinking* worker apart from an
/// *idle* one. A worker deep in one reasoning turn emits no cosmon events for
/// many minutes, so the control-plane progress clock alone cannot distinguish
/// it from a worker parked at a dead prompt — and patrol nudged the working
/// one nine times in ten minutes (2026-07-19).
///
/// It returns a *number of seconds of silence*, never pane text. ADR-137 §2
/// forbids reading a worker's **act** out of glyphs it authored, because a
/// guard keyed on a string arrests everyone who prints the string. There is no
/// string here to print: tmux stamps `session_activity` itself, the worker can
/// only ever make the clock fresher, and a fresher clock only ever *suppresses*
/// a nudge. No lifecycle transition keys off it.
///
/// `session_activity` is a Unix epoch in seconds. A clock that reads *ahead*
/// of `now` (NTP step, tmux server on another host) clamps to zero silence —
/// the safe direction, since zero means "recently active" means "do not poke".
#[must_use]
pub fn pane_idle_seconds(socket: &str, session: &str) -> Option<i64> {
    let output = Command::new("tmux")
        .args([
            "-L",
            socket,
            "display-message",
            "-p",
            "-t",
            session,
            "#{session_activity}",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let activity: i64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let now = i64::try_from(now).ok()?;
    Some((now - activity).max(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A socket with no such session yields no clock — the caller must treat
    /// that as *unknown*, not as *idle*.
    #[test]
    fn pane_idle_seconds_missing_session_is_none() {
        assert_eq!(
            pane_idle_seconds("cosmon-registry-test-nonexistent-socket", "no-such-session"),
            None
        );
    }

    #[test]
    fn default_registry_knows_claude() {
        let r = default_registry();
        assert!(r.matches(crate::claude::ADAPTER_NAME, "claude"));
        assert_eq!(
            r.signatures_of(crate::claude::ADAPTER_NAME),
            &[
                "claude".to_owned(),
                "claude*".to_owned(),
                "node".to_owned(),
                VERSION_SENTINEL.to_owned(),
            ]
        );
    }

    /// Regression for the 2026-06-12 field report: a real `claude`
    /// worker pane surfaced `pane_current_command=2.1.175` (the native
    /// binary reports its own version as the foreground `comm`), and the
    /// whisper / propulsion gates rejected it with
    /// `expected one of ["claude"]`. The version sentinel must accept any
    /// version-shaped token while still refusing genuine non-claude panes.
    #[test]
    fn default_registry_matches_claude_version_string_pane() {
        let r = default_registry();
        let name = crate::claude::ADAPTER_NAME;
        // The exact value observed in the field.
        assert!(r.matches(name, "2.1.175"));
        // Other plausible version shapes.
        assert!(r.matches(name, "2.1"));
        assert!(r.matches(name, "10.20.30"));
        // The Node.js entry-point fallback.
        assert!(r.matches(name, "node"));
        // Wrapper / renamed installs via the prefix glob.
        assert!(r.matches(name, "claude-code"));
        assert!(r.matches(name, "claude-wrapper"));
        // A worker that crashed into its shell must STILL be refused —
        // the gate's whole purpose is to refuse co-opting a non-claude pane.
        assert!(!r.matches(name, "zsh"));
        assert!(!r.matches(name, "bash"));
        assert!(!r.matches(name, "python"));
        // Partial / malformed version-ish tokens are not versions.
        assert!(!r.matches(name, "2."));
        assert!(!r.matches(name, ".2"));
        assert!(!r.matches(name, "2.1.x"));
        assert!(!r.matches(name, "v2.1.175"));
    }

    /// Unit coverage for the version-string recogniser, independent of
    /// the registry wiring.
    #[test]
    fn looks_like_version_accepts_dotted_numerics_only() {
        assert!(looks_like_version("2.1.175"));
        assert!(looks_like_version("0.0"));
        assert!(looks_like_version("1.2.3.4"));
        assert!(!looks_like_version("2"));
        assert!(!looks_like_version(""));
        assert!(!looks_like_version("2."));
        assert!(!looks_like_version(".2"));
        assert!(!looks_like_version("2..1"));
        assert!(!looks_like_version("claude"));
        assert!(!looks_like_version("2.1-rc1"));
    }

    /// The trailing-`*` prefix glob matches by prefix; without it, the
    /// signature is an exact literal (verified by the pre-existing tests).
    #[test]
    fn signature_matches_handles_prefix_glob_and_sentinel() {
        assert!(signature_matches("claude*", "claude"));
        assert!(signature_matches("claude*", "claude-code"));
        assert!(!signature_matches("claude*", "aider-claude"));
        assert!(signature_matches(VERSION_SENTINEL, "3.4.5"));
        assert!(!signature_matches(VERSION_SENTINEL, "node"));
        assert!(signature_matches("claude", "claude"));
        assert!(!signature_matches("claude", "claude-code"));
    }

    /// Regression for the 2026-07-18 friction (task-20260718-912b):
    /// `cs whisper` toward an identity-pinned codex worker failed with
    /// `pane_current_command=GIT_AUTHOR_NAME=… expected one of [claude,
    /// claude*, node, codex, codex*]`. The F3 identity pinning
    /// (delib-20260717-194b, `build_codex_command`) prefixes
    /// `GIT_AUTHOR_*` / `GIT_COMMITTER_*` env assignments onto the spawn
    /// command, and the observed pane command surfaced that prefix
    /// instead of the binary. The gate must parse past the assignments
    /// to the real binary.
    #[test]
    fn default_registry_matches_codex_behind_git_identity_env_prefix() {
        let r = default_registry();
        let name = crate::codex::ADAPTER_NAME;
        // The exact spawn shape produced by `build_codex_command` with
        // `git_identity: Some(..)` — quoted values with embedded spaces.
        let spawned = "GIT_AUTHOR_NAME='Emmanuel Sérié' \
                       GIT_AUTHOR_EMAIL='op@example.org' \
                       GIT_COMMITTER_NAME='Emmanuel Sérié' \
                       GIT_COMMITTER_EMAIL='op@example.org' \
                       RUST_LOG=error codex --yolo";
        assert!(r.matches(name, spawned));
        // First-token-only variant (what the field report showed).
        assert!(r.matches(name, "GIT_AUTHOR_NAME=Emmanuel codex"));
        // The claude adapter behind the same prefix shape keeps working.
        assert!(r.matches(
            crate::claude::ADAPTER_NAME,
            "GIT_AUTHOR_NAME='Ada Lovelace' claude --continue"
        ));
        // A crashed-into-shell pane behind an env prefix is STILL refused —
        // normalisation must not widen the gate to arbitrary panes.
        assert!(!r.matches(name, "GIT_AUTHOR_NAME='Emmanuel Sérié' zsh"));
        // A bare assignment with no binary after it is not a match.
        assert!(!r.matches(name, "GIT_AUTHOR_NAME=Emmanuel"));
    }

    /// Unit coverage for the env-prefix normaliser, independent of the
    /// registry wiring.
    #[test]
    fn effective_pane_command_strips_env_prefix() {
        // Plain comm values pass through untouched.
        assert_eq!(effective_pane_command("codex"), "codex");
        assert_eq!(effective_pane_command("zsh"), "zsh");
        assert_eq!(effective_pane_command("2.1.175"), "2.1.175");
        // Single assignment, unquoted value.
        assert_eq!(effective_pane_command("RUST_LOG=error codex"), "codex");
        // Quoted value with embedded whitespace does not split the word.
        assert_eq!(
            effective_pane_command("GIT_AUTHOR_NAME='Emmanuel Sérié' codex --yolo"),
            "codex"
        );
        assert_eq!(
            effective_pane_command("GIT_AUTHOR_NAME=\"Ada Lovelace\" node"),
            "node"
        );
        // A bare `env` launcher is skipped like an assignment.
        assert_eq!(effective_pane_command("env RUST_LOG=error codex"), "codex");
        // Absolute binary paths are basename'd to the comm shape.
        assert_eq!(
            effective_pane_command("RUST_LOG=error /usr/local/bin/codex"),
            "codex"
        );
        // Nothing after the assignments → empty (gate refuses).
        assert_eq!(effective_pane_command("GIT_AUTHOR_NAME=Emmanuel"), "");
        assert_eq!(effective_pane_command(""), "");
        assert_eq!(effective_pane_command("   "), "");
        // A non-identifier before `=` is NOT an assignment — it is the
        // command itself (weird, but the gate should see it verbatim).
        assert_eq!(
            effective_pane_command("2fast=furious codex"),
            "2fast=furious"
        );
    }

    #[test]
    fn unknown_adapter_never_matches() {
        let r = default_registry();
        assert!(!r.matches("ghostly", "ghostly"));
        assert!(!r.matches("ghostly", "claude"));
        assert_eq!(r.signatures_of("ghostly"), &[] as &[String]);
    }

    /// Codex Adapter must be in the default registry with `codex`,
    /// `codex*` (wrapper glob), and `node` pane signatures — the
    /// `@openai/codex` npm package may surface as any of these.
    #[test]
    fn default_registry_knows_codex_signatures() {
        let r = default_registry();
        assert!(r.matches(crate::codex::ADAPTER_NAME, "codex"));
        assert!(r.matches(crate::codex::ADAPTER_NAME, "node"));
        // `codex*` glob covers a wrapper / renamed install (task-20260711-246d).
        assert!(r.matches(crate::codex::ADAPTER_NAME, "codex-cli"));
        assert!(r.matches(crate::codex::ADAPTER_NAME, "codex-wrapper"));
        // Codex must not silently collide with claude or aider.
        assert!(!r.matches(crate::codex::ADAPTER_NAME, "claude"));
        assert!(!r.matches(crate::codex::ADAPTER_NAME, "aider"));
    }

    /// C4 (PR-3): the default registry must match every variant of
    /// Aider's pane signature so the propulsion gate cannot misfire
    /// on a live Aider worker regardless of how `aider-chat` was
    /// installed.
    #[test]
    fn default_registry_knows_aider_variants() {
        let r = default_registry();
        assert!(r.matches(crate::aider::ADAPTER_NAME, "aider"));
        assert!(r.matches(crate::aider::ADAPTER_NAME, "python"));
        assert!(r.matches(crate::aider::ADAPTER_NAME, "python3"));
        assert!(r.matches(crate::aider::ADAPTER_NAME, "python3.11"));
        assert!(r.matches(crate::aider::ADAPTER_NAME, "python3.12"));
        // Future python variant not in the explicit list — fine, the
        // operator can extend via C6 TOML loader.
        assert!(!r.matches(crate::aider::ADAPTER_NAME, "python4.0"));
        // The claude signature must not collide with the aider one.
        assert!(!r.matches(crate::aider::ADAPTER_NAME, "claude"));
    }

    #[test]
    fn empty_pane_cmd_never_matches() {
        let r = default_registry();
        assert!(!r.matches(crate::claude::ADAPTER_NAME, ""));
    }

    /// ADR-100 R2 wave 2: in-process Direct-API adapters must be present
    /// in the default registry so the propulsion / whisper gates can
    /// branch on adapter name without a fallback to the empty signature
    /// list (the silent-mismatch class).
    #[test]
    fn default_registry_knows_direct_api_adapters() {
        let r = default_registry();
        assert!(r.matches("openai", "openai"));
        assert!(r.matches("anthropic", "anthropic"));
        // Direct-API names must not silently collide with claude/aider.
        assert!(!r.matches("openai", "claude"));
        assert!(!r.matches("openai", "aider"));
    }

    /// Per ADR-103 / ADR-104: the in-process
    /// `llama-cpp` Adapter must be present in the default registry
    /// alongside its `llama` legacy alias, so the propulsion / whisper
    /// gates can branch on adapter name and skip the tmux probe
    /// instead of silently mismatching on the empty pane query. The
    /// sentinel signature is the adapter name itself — the same shape
    /// the `openai` / `anthropic` rows use.
    #[test]
    fn default_registry_knows_llama_cpp() {
        let r = default_registry();
        // Canonical CLI name.
        assert!(r.matches("llama-cpp", "llama-cpp"));
        assert_eq!(r.signatures_of("llama-cpp"), &["llama-cpp".to_owned()]);
        // Legacy alias preserved for operator vocabulary (tolnay's
        // name-stability table, delib synthesis §B.2 D4).
        assert!(r.matches("llama", "llama"));
        assert_eq!(r.signatures_of("llama"), &["llama".to_owned()]);
        // Must not silently collide with another in-process adapter
        // or with the bare-binary signature of an external one.
        assert!(!r.matches("llama-cpp", "claude"));
        assert!(!r.matches("llama-cpp", "openai"));
        assert!(!r.matches("llama-cpp", "llama"));
        assert!(!r.matches("llama", "llama-cpp"));
    }

    /// The in-process supervision
    /// classification must extend to `llama-cpp` (and its `llama`
    /// alias). A regression here would silently route the in-process
    /// FFI worker through the tmux pane-died supervision hook and
    /// mark the worker as dead immediately (no pane to probe).
    #[test]
    fn supervision_mode_marks_llama_cpp_in_process() {
        assert_eq!(
            supervision_mode_for("llama-cpp"),
            SupervisionMode::InProcess,
            "llama-cpp is an in-process FFI adapter (C3 of delib-20260519-a20b)"
        );
        assert_eq!(
            supervision_mode_for("llama"),
            SupervisionMode::InProcess,
            "llama is the legacy alias of the in-process llama-cpp adapter"
        );
    }

    #[test]
    fn second_register_overwrites_first() {
        let mut r = PaneSignatureRegistry::new();
        r.register("claude", vec!["claude".to_owned()]);
        r.register("claude", vec!["claude-wrapper".to_owned()]);
        assert!(!r.matches("claude", "claude"));
        assert!(r.matches("claude", "claude-wrapper"));
    }

    #[test]
    fn multi_signature_adapter_matches_any() {
        let mut r = PaneSignatureRegistry::new();
        r.register(
            "hybrid",
            vec!["claude".to_owned(), "claude-shim".to_owned()],
        );
        assert!(r.matches("hybrid", "claude"));
        assert!(r.matches("hybrid", "claude-shim"));
        assert!(!r.matches("hybrid", "aider"));
    }

    #[test]
    fn supervision_mode_marks_direct_api_in_process() {
        assert_eq!(
            supervision_mode_for("openai"),
            SupervisionMode::InProcess,
            "openai is a Direct-API in-process adapter (ADR-100 R2)"
        );
        assert_eq!(
            supervision_mode_for("anthropic"),
            SupervisionMode::InProcess,
            "anthropic is a Direct-API in-process adapter (ADR-100 R2)"
        );
    }

    #[test]
    fn supervision_mode_keeps_tmux_default_for_subprocess_adapters() {
        assert_eq!(supervision_mode_for("claude"), SupervisionMode::TmuxPane);
        assert_eq!(supervision_mode_for("aider"), SupervisionMode::TmuxPane);
        // Unknown adapters default to TmuxPane so the legacy pane-died
        // supervision contract is the conservative fallback.
        assert_eq!(supervision_mode_for("unknown"), SupervisionMode::TmuxPane);
    }

    #[test]
    fn pane_current_command_returns_none_for_dead_socket() {
        // A socket that definitely has no such session: tmux -L returns
        // non-zero. The helper maps that to None so the gate treats it
        // as a non-match.
        let observed =
            pane_current_command("cosmon-registry-test-socket-absent", "no-such-session-xyz");
        assert!(observed.is_none());
    }
}
