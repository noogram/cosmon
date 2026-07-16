// SPDX-License-Identifier: AGPL-3.0-only

//! `cs release-audit --dry-run` — drift detector for the cosmon public
//! distribution, run against the **live** instance tree.
//!
//! ## Role in the architecture
//!
//! This is the release-side analogue of [`cs reconcile --check`]: where
//! `reconcile --check` answers *"would the projected surfaces regress?"*,
//! `release-audit --dry-run` answers *"would the **distribution** regress
//! if we ran the canonical genericisation chain right now?"* — without
//! paying for a full scratch clone + `git filter-repo` rewrite.
//!
//! ## The membrane is deny-by-default (ADR-127)
//!
//! The audit's **primary** verdict is the allowlist: *ship nothing except
//! positively-cleared paths.* Any tracked, non-purged path with no permit in
//! `.cosmon/release-allowlist.toml` is a regression. This is Gate G's
//! deny-by-default polarity (already true for binaries) generalised to the
//! whole text tree — a brand-new confidential file is caught **by
//! construction** because *new* means *unpermitted* means *refused*, instead
//! of slipping silently past a frozen denylist (the 2026-06-10 failure
//! class). The legacy token/structural detectors do not
//! disappear; they demote to a **content backstop** that scans permitted files
//! for known-bad strings.
//!
//! The membrane is **armed by the presence of the allowlist file**. When it is
//! absent the path membrane is dormant and the audit behaves as the old
//! denylist did — but says so **loudly** (a warning, never a silent pass), so
//! the absence of the membrane is itself visible. See ADR-127 §7 for the
//! migration.
//!
//! ## Rules live in a private config (Bucket-3)
//!
//! The confidential denylist literals (client tokens, private domains,
//! private-infra crate names, the purge lists) are **not** in this source
//! file — that would publish the very roster the detector suppresses. They are
//! loaded at runtime from the private, purged-from-release
//! `.cosmon/release-rules.toml` (see [`ReleaseRules`]). When that file is
//! absent the token/structural backstop is inert and the audit warns; the
//! deny-by-default path membrane needs no secret tokens and is unaffected.
//!
//! ## What it is NOT
//!
//! Not a daemon, not an auto-remediator, not the full audit. It reports; the
//! operator (or the `release-resync` formula) acts. Read-only and idempotent —
//! blessing a path is a **separate** tool (`scripts/release/bless-allowlist.sh`),
//! never a write-mode bolted onto this read-only audit (write-read asymmetry,
//! CLAUDE.md checklist #8). Silent when the distribution is clean, loud (exit
//! 1) when it would regress.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use super::Context;

// ---------------------------------------------------------------------------
// Private rule set (Bucket-3) — loaded from .cosmon/release-rules.toml
// ---------------------------------------------------------------------------

/// Filename (relative to the audited repo root) holding the confidential
/// release-audit rule set.
const RELEASE_RULES_CONFIG: &str = ".cosmon/release-rules.toml";

/// One structural-string rule (the gate-C detector data, externalised).
///
/// `token` is the canonical string emitted on a hit. `strip_before` removes
/// substrings from the line before the contains-test (e.g. the author email,
/// so the operator author email does not trip the homeserver detector). `path_exempt`
/// tolerates the token inside one vendoring-provenance location (e.g. the
/// `claudion` crate's own provenance comment).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StructuralRule {
    token: String,
    #[serde(default)]
    strip_before: Vec<String>,
    #[serde(default)]
    path_exempt: Option<String>,
    detail: String,
}

/// The confidential rule set, loaded from `<repo>/.cosmon/release-rules.toml`.
///
/// An absent file yields the empty default: the token/structural/binary-name
/// backstop is inert (and the audit warns), but the deny-by-default **path**
/// membrane — which needs no secret tokens — keeps working. This is why a
/// foreign public clone, which never sees this file, still gets a correct
/// allowlist verdict. See ADR-127 §6.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseRules {
    #[serde(default)]
    purge_prefixes: Vec<String>,
    #[serde(default)]
    purge_exact: Vec<String>,
    #[serde(default)]
    purge_substrings: Vec<String>,
    #[serde(default)]
    client_tokens: Vec<String>,
    #[serde(default)]
    path_rename_tokens: Vec<String>,
    #[serde(default)]
    private_infra_crates: Vec<String>,
    #[serde(default)]
    allow_binary: Vec<String>,
    #[serde(default, rename = "structural_string")]
    structural_strings: Vec<StructuralRule>,
}

impl ReleaseRules {
    /// Whether any token/structural rule is present. When false the backstop
    /// is inert and the audit emits a loud warning rather than a silent pass.
    fn has_token_rules(&self) -> bool {
        !self.client_tokens.is_empty()
            || !self.private_infra_crates.is_empty()
            || !self.structural_strings.is_empty()
    }
}

/// Load `<repo>/.cosmon/release-rules.toml` if present; absent ⇒ empty default.
fn load_release_rules(repo: &Path) -> anyhow::Result<ReleaseRules> {
    let path = repo.join(RELEASE_RULES_CONFIG);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ReleaseRules::default()),
        Err(e) => anyhow::bail!("failed to read {}: {e}", path.display()),
    };
    toml::from_str(&text).map_err(|e| anyhow::anyhow!("invalid {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Deny-by-default allowlist (ADR-127) — loaded from .cosmon/release-allowlist.toml
// ---------------------------------------------------------------------------

/// Filename (relative to the audited repo root) holding the path permits.
const RELEASE_ALLOWLIST_CONFIG: &str = ".cosmon/release-allowlist.toml";

/// One clearance of one path. Per-path, never a glob (ADR-127 §4): a glob is a
/// rubber-stamp that re-opens the silent hole one directory at a time.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Permit {
    /// Exact path, repo-root-relative.
    path: String,
    /// Date of clearance (audit trail).
    cleared_at: String,
    /// Who reviewed it.
    cleared_by: String,
    /// One line — *why* this path is safe to publish. Enforced non-empty.
    reason: String,
    /// Optional `blake3:<hex>` of the live content at clearance time. Present
    /// ⇒ content-bound: any edit re-opens the permit (cleanliness-now, §5).
    #[serde(default)]
    seal: Option<String>,
}

/// The path allowlist, loaded from `<repo>/.cosmon/release-allowlist.toml`.
///
/// Absent ⇒ [`None`]: the membrane is dormant (legacy denylist behaviour) and
/// the audit warns loudly. Present ⇒ armed: every non-purged tracked path must
/// carry a permit.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Allowlist {
    #[serde(default, rename = "permit")]
    permits: Vec<Permit>,
}

impl Allowlist {
    /// The set of permitted paths, for the `path-not-permitted` detector.
    fn permitted_paths(&self) -> BTreeSet<&str> {
        self.permits.iter().map(|p| p.path.as_str()).collect()
    }
}

/// Load `<repo>/.cosmon/release-allowlist.toml` if present.
///
/// Missing file ⇒ [`None`] (legacy mode). A present-but-malformed file, or a
/// permit with a blank `reason`, is a hard error: an undocumented clearance is
/// exactly the rubber-stamp this mechanism exists to prevent.
fn load_allowlist(repo: &Path) -> anyhow::Result<Option<Allowlist>> {
    let path = repo.join(RELEASE_ALLOWLIST_CONFIG);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => anyhow::bail!("failed to read {}: {e}", path.display()),
    };
    let allow: Allowlist =
        toml::from_str(&text).map_err(|e| anyhow::anyhow!("invalid {}: {e}", path.display()))?;
    for p in &allow.permits {
        // Every permit must be documented AND attributed: an undated or
        // unattributed clearance is exactly the rubber-stamp this mechanism
        // prevents. Reading these fields here also makes them load-bearing.
        let missing = if p.reason.trim().is_empty() {
            Some("reason — why the path is safe to publish")
        } else if p.cleared_at.trim().is_empty() {
            Some("cleared_at — the date of clearance")
        } else if p.cleared_by.trim().is_empty() {
            Some("cleared_by — who reviewed it")
        } else {
            None
        };
        if let Some(field) = missing {
            anyhow::bail!(
                "{}: permit for path '{}' is missing {field} (ADR-127 §4)",
                path.display(),
                p.path
            );
        }
    }
    Ok(Some(allow))
}

/// Compute the `blake3:<hex>` seal of a file's bytes (the cosmon seal form).
fn content_seal(bytes: &[u8]) -> String {
    format!("blake3:{}", cosmon_hash::Hash::of_bytes(bytes).to_hex())
}

// ---------------------------------------------------------------------------
// Per-repo exemption config (referee-against-referee reconciliation)
// ---------------------------------------------------------------------------

/// Filename (relative to the audited repo root) holding gate-C exemptions.
const RELEASE_AUDIT_CONFIG: &str = ".cosmon/release-audit.toml";

/// Per-repo `cs release-audit` configuration, loaded from
/// `<repo>/.cosmon/release-audit.toml` when present. An absent file yields
/// [`ReleaseAuditConfig::default`] (no exemptions), so an unconfigured repo
/// audits byte-identically to before this knob existed.
///
/// The config exists to resolve a *referee-against-referee* conflict. A
/// structural string can be a genuine leak in one repo and the intended
/// public identity in another (the operator research domain is a leak inside
/// cosmon but the deliberately-published maintainer-contact domain inside
/// oxymake, whose own forbid-strings gate already exempts it). The audited
/// repo carries its own verdict here, in the same file its native
/// forbid-strings gate reads, so both referees share one exemption list.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseAuditConfig {
    /// Structural-string tokens this repo deliberately publishes. A
    /// `structural-string` finding whose token matches one of these is
    /// suppressed. Scoped to gate C only.
    #[serde(default)]
    structural_string_exemptions: Vec<StructuralStringExemption>,
}

/// One gate-C carve-out: a structural token plus the mandatory reason it is
/// allowed to ship in this repo.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StructuralStringExemption {
    /// The structural token to stop flagging, matched exactly against the
    /// canonical token a [`StructuralRule`] emits.
    token: String,
    /// Why this token is allowed in the published tree. Required and enforced
    /// non-empty at load — a blank justification is a hard error.
    justification: String,
}

impl ReleaseAuditConfig {
    /// The set of exempted structural tokens, for [`structural_hits_in_line`].
    fn exempted_tokens(&self) -> Vec<String> {
        self.structural_string_exemptions
            .iter()
            .map(|e| e.token.clone())
            .collect()
    }
}

/// Load `<repo>/.cosmon/release-audit.toml` if present.
///
/// Missing file ⇒ default (empty) config. A present-but-malformed file, or an
/// exemption with a blank `justification`, is a hard error.
fn load_release_audit_config(repo: &Path) -> anyhow::Result<ReleaseAuditConfig> {
    let path = repo.join(RELEASE_AUDIT_CONFIG);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReleaseAuditConfig::default());
        }
        Err(e) => anyhow::bail!("failed to read {}: {e}", path.display()),
    };
    let cfg: ReleaseAuditConfig =
        toml::from_str(&text).map_err(|e| anyhow::anyhow!("invalid {}: {e}", path.display()))?;
    for ex in &cfg.structural_string_exemptions {
        if ex.justification.trim().is_empty() {
            anyhow::bail!(
                "{}: exemption for token '{}' has an empty justification — every gate-C \
                 carve-out must document why the string is allowed in the published tree",
                path.display(),
                ex.token
            );
        }
    }
    Ok(cfg)
}

/// Git's own binary heuristic: a blob is binary if it contains a NUL byte in
/// the first 8000 bytes. Used by gate G to flag a tracked binary that `git
/// grep -I` (and therefore the text detectors) cannot see.
fn is_binary_content(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(8000)];
    window.contains(&0)
}

// ---------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------

/// Arguments for `cs release-audit`.
#[derive(clap::Args)]
pub struct Args {
    /// Simulate the `release-resync` transformation chain against the live
    /// working tree (no scratch clone) and report regressions. This is
    /// currently the only mode; the flag is accepted so the documented
    /// invocation `cs release-audit --dry-run` is exact.
    #[arg(long)]
    pub dry_run: bool,

    /// Repository root to audit. Defaults to the toplevel discovered by
    /// `git rev-parse --show-toplevel` from the current directory.
    #[arg(long, value_name = "PATH")]
    pub repo: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Report types (serialised verbatim under --json)
// ---------------------------------------------------------------------------

/// One detected regression: a thing that would survive the canonical chain
/// and degrade the public distribution.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Regression {
    /// Stable detector tag, for scripting (`jq 'select(.detector=="…")'`).
    pub detector: &'static str,
    /// Path of the offending file, relative to the repo root.
    pub path: String,
    /// 1-based line number when the finding is content-scoped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// The offending token / crate name, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Human-readable explanation of why this regresses the distribution.
    pub detail: String,
}

/// The full audit report.
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseAuditReport {
    /// Always `true` today (only mode).
    pub dry_run: bool,
    /// Repo root that was audited.
    pub repo: String,
    /// `"allowlist"` when the deny-by-default membrane is armed (an allowlist
    /// file is present), `"legacy-denylist"` when it is dormant.
    pub membrane_mode: &'static str,
    /// Number of tracked files inspected.
    pub files_scanned: usize,
    /// Non-fatal advisories (e.g. dormant membrane, inert backstop). These do
    /// not fail the audit but are printed loudly so an absent control can
    /// never masquerade as a clean tree.
    pub warnings: Vec<String>,
    /// All detected regressions; empty ⇒ clean.
    pub regressions: Vec<Regression>,
}

impl ReleaseAuditReport {
    /// Whether the distribution would NOT regress.
    pub fn is_clean(&self) -> bool {
        self.regressions.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Pure detection logic (unit-tested, no I/O)
// ---------------------------------------------------------------------------

/// Whether `path` is dropped by the canonical purge and therefore never
/// published.
fn is_purged(rules: &ReleaseRules, path: &str) -> bool {
    rules.purge_exact.iter().any(|p| p == path)
        || rules
            .purge_prefixes
            .iter()
            .any(|p| path == p || path.starts_with(&format!("{p}/")))
        || rules.purge_substrings.iter().any(|s| path.contains(s))
}

/// Case-insensitive substring test (Unicode-aware via `to_lowercase`).
#[cfg(test)]
fn ci_contains(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Apply the simulated path-rename (lowercase substring removal of every
/// `path_rename_tokens` entry), then report any client token that still
/// survives in the path. The chain rewrites content for every client token,
/// so a *path* token the rename map omits is the genuine regression.
fn client_tokens_surviving_path_rename(rules: &ReleaseRules, path: &str) -> Vec<String> {
    let mut renamed = path.to_lowercase();
    for tok in &rules.path_rename_tokens {
        renamed = renamed.replace(&tok.to_lowercase(), "-");
    }
    rules
        .client_tokens
        .iter()
        .filter(|t| renamed.contains(&t.to_lowercase()))
        .map(ToString::to_string)
        .collect()
}

/// A structural-string finding within a single line of content.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StructuralHit {
    token: String,
    detail: String,
}

/// Scan one line of (non-purged) content for the structural strings the
/// canonical chain does **not** scrub, per the loaded [`StructuralRule`]s.
/// `exempted` lists tokens the audited repo declares intentionally public.
fn structural_hits_in_line(
    line: &str,
    path: &str,
    rules: &ReleaseRules,
    exempted: &[String],
) -> Vec<StructuralHit> {
    let lc = line.to_lowercase();
    let mut hits = Vec::new();
    for rule in &rules.structural_strings {
        // Tolerate the token inside its one provenance location.
        if let Some(ex) = &rule.path_exempt {
            if path.contains(ex.as_str()) {
                continue;
            }
        }
        // Strip the disambiguating substrings (e.g. the author email) first.
        let mut hay = lc.clone();
        for s in &rule.strip_before {
            hay = hay.replace(&s.to_lowercase(), " ");
        }
        if hay.contains(&rule.token.to_lowercase()) {
            hits.push(StructuralHit {
                token: rule.token.clone(),
                detail: rule.detail.clone(),
            });
        }
    }
    hits.retain(|h| !exempted.iter().any(|e| e == &h.token));
    hits
}

/// A private-sibling path-dependency finding in a Cargo manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ManifestHit {
    line: usize,
    token: Option<String>,
    detail: String,
}

/// Scan a `Cargo.toml`'s text for private-sibling path dependencies: a
/// `path = "../…"` escaping the repo, or any `private_infra_crates` name.
fn manifest_private_path_deps(rules: &ReleaseRules, text: &str) -> Vec<ManifestHit> {
    let mut hits = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let lc = line.to_lowercase();
        for crate_name in &rules.private_infra_crates {
            if lc.contains(&crate_name.to_lowercase()) {
                hits.push(ManifestHit {
                    line: idx + 1,
                    token: Some(crate_name.clone()),
                    detail: format!(
                        "private-infra crate '{crate_name}' referenced in a manifest — \
                         a foreign public clone cannot resolve it (vendor it in-tree)"
                    ),
                });
            }
        }
        if let Some(dep) = extract_path_value(line) {
            if dep.starts_with("../../") {
                hits.push(ManifestHit {
                    line: idx + 1,
                    token: Some(dep.clone()),
                    detail: format!(
                        "path dependency '{dep}' climbs above the repo root — a private \
                         sibling that a foreign clone will not have"
                    ),
                });
            }
        }
    }
    hits
}

/// Extract the value of a `path = "…"` key from a manifest line, if present.
fn extract_path_value(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let after = line
        .match_indices("path")
        .find_map(|(i, _)| {
            let rest = line[i + 4..].trim_start();
            rest.strip_prefix('=').map(str::trim_start)
        })
        .or_else(|| {
            if trimmed.contains("path") {
                line.split_once("path")
                    .map(|(_, r)| r.trim_start())
                    .and_then(|r| r.strip_prefix('='))
                    .map(str::trim_start)
            } else {
                None
            }
        })?;
    let after = after.trim_start();
    let quote = after.starts_with('"');
    if !quote {
        return None;
    }
    let inner = &after[1..];
    inner.find('"').map(|end| inner[..end].to_string())
}

/// Whether a tracked path is a live instance OIDC binding (not the `.example`
/// template) — audit gate B.
fn is_instance_binding(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    base == "oidc-identity.toml"
}

/// Whether `path` is this detector's own rule-definition source.
///
/// After Bucket-3 (ADR-127 §6) the confidential literals live in the private
/// `.cosmon/release-rules.toml`, not here — so this source carries no client
/// roster and would pass a content scan anyway. The carve-out is kept as
/// belt-and-suspenders against an example token reappearing in a doc comment.
fn is_rule_definition_source(path: &str) -> bool {
    path.ends_with("cmd/release_audit.rs")
}

// ---------------------------------------------------------------------------
// I/O wrapper — git-backed collection + report assembly
// ---------------------------------------------------------------------------

/// Run `git` in `repo` and return trimmed stdout lines on success.
fn git_lines(repo: &Path, args: &[&str]) -> anyhow::Result<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

/// Resolve the repo root: `--repo` if given, else `git rev-parse
/// --show-toplevel` from the current directory.
fn resolve_repo(args: &Args) -> anyhow::Result<PathBuf> {
    if let Some(p) = &args.repo {
        return Ok(p.clone());
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let top = git_lines(&cwd, &["rev-parse", "--show-toplevel"])?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("git rev-parse --show-toplevel returned nothing"))?;
    Ok(PathBuf::from(top))
}

/// Run the per-file content detectors (binary, instance binding, path-rename
/// client token, manifest path-dep) over a single tracked `path`.
fn audit_tracked_file(repo: &Path, rules: &ReleaseRules, path: &str) -> Vec<Regression> {
    let mut regressions = Vec::new();

    // Detector: client name in a path the rename chain misses.
    for tok in client_tokens_surviving_path_rename(rules, path) {
        regressions.push(Regression {
            detector: "client-name-path",
            path: path.to_string(),
            line: None,
            token: Some(tok.clone()),
            detail: format!(
                "client token '{tok}' survives the canonical path-rename — the tracked \
                 path ships the client name"
            ),
        });
    }

    // Detector: instance binding re-tracked.
    if is_instance_binding(path) {
        regressions.push(Regression {
            detector: "instance-file-tracked",
            path: path.to_string(),
            line: None,
            token: None,
            detail: "live instance OIDC binding tracked under a non-purged path — \
                     only oidc-identity.toml.example should be published (audit gate B)"
                .to_string(),
        });
    }

    // Detector: tracked binary blob (gate G — deny-by-default).
    if !rules.allow_binary.iter().any(|a| a == path) {
        if let Ok(bytes) = std::fs::read(repo.join(path)) {
            if !bytes.is_empty() && is_binary_content(&bytes) {
                regressions.push(Regression {
                    detector: "binary-blob",
                    path: path.to_string(),
                    line: None,
                    token: None,
                    detail: "tracked binary file under a non-purged path — git grep -I is \
                             blind to pixel/encoded content (gate G, deny-by-default; OCR \
                             refused). Purge it or regenerate with fixture data and allowlist."
                        .to_string(),
                });
            }
        }
    }

    // Detector: private-sibling path-dependency (manifests only).
    if path == "Cargo.toml" || path.ends_with("/Cargo.toml") {
        if let Ok(text) = std::fs::read_to_string(repo.join(path)) {
            for hit in manifest_private_path_deps(rules, &text) {
                regressions.push(Regression {
                    detector: "private-sibling-path-dep",
                    path: path.to_string(),
                    line: Some(hit.line),
                    token: hit.token,
                    detail: hit.detail,
                });
            }
        }
    }

    regressions
}

/// The deny-by-default path membrane (ADR-127): when an allowlist is present,
/// every non-purged tracked path must carry a permit, and every content-bound
/// permit's seal must still match the live content.
fn audit_allowlist(
    repo: &Path,
    rules: &ReleaseRules,
    allow: &Allowlist,
    tracked: &[String],
) -> Vec<Regression> {
    let permitted = allow.permitted_paths();
    let mut regressions = Vec::new();

    // Every non-purged tracked path must be positively cleared.
    for path in tracked {
        if is_purged(rules, path) {
            continue;
        }
        if !permitted.contains(path.as_str()) {
            regressions.push(Regression {
                detector: "path-not-permitted",
                path: path.clone(),
                line: None,
                token: None,
                detail: "tracked path has no permit in .cosmon/release-allowlist.toml — \
                         ship nothing except positively-cleared paths (ADR-127, deny-by-default). \
                         Review and bless it, or purge it."
                    .to_string(),
            });
        }
    }

    let tracked_set: BTreeSet<&str> = tracked.iter().map(String::as_str).collect();
    for permit in &allow.permits {
        // A permit for a path that no longer ships is stale clearance.
        if !tracked_set.contains(permit.path.as_str()) {
            regressions.push(Regression {
                detector: "permit-orphan",
                path: permit.path.clone(),
                line: None,
                token: None,
                detail: "permit names a path that is no longer tracked — remove the stale \
                         clearance from .cosmon/release-allowlist.toml"
                    .to_string(),
            });
            continue;
        }
        // Content-bound permits: the clearance is only valid for the bytes it
        // was granted against (cleanliness-now, ADR-127 §5).
        if let Some(seal) = &permit.seal {
            match std::fs::read(repo.join(&permit.path)) {
                Ok(bytes) => {
                    let now = content_seal(&bytes);
                    if &now != seal {
                        regressions.push(Regression {
                            detector: "permit-stale",
                            path: permit.path.clone(),
                            line: None,
                            token: None,
                            detail: format!(
                                "content-bound permit is stale — file changed since clearance \
                                 (sealed {seal}, now {now}). Re-review and re-bless."
                            ),
                        });
                    }
                }
                Err(e) => regressions.push(Regression {
                    detector: "permit-stale",
                    path: permit.path.clone(),
                    line: None,
                    token: None,
                    detail: format!("content-bound permit cannot read its file: {e}"),
                }),
            }
        }
    }

    regressions
}

/// Collect all regressions by running every detector over the repo's tracked
/// files.
fn audit_repo(repo: &Path) -> anyhow::Result<ReleaseAuditReport> {
    let rules = load_release_rules(repo)?;
    let allowlist = load_allowlist(repo)?;
    let config = load_release_audit_config(repo)?;
    let exempted = config.exempted_tokens();
    let tracked = git_lines(repo, &["ls-files"])?;

    let mut warnings = Vec::new();
    let mut regressions = Vec::new();

    // Primary membrane: deny-by-default allowlist (when armed).
    let membrane_mode = if let Some(allow) = &allowlist {
        regressions.extend(audit_allowlist(repo, &rules, allow, &tracked));
        "allowlist"
    } else {
        warnings.push(
            "membrane in LEGACY denylist mode — no .cosmon/release-allowlist.toml present. \
             NEW confidential paths are NOT caught by construction; only known-bad patterns \
             are. Bootstrap the allowlist to arm deny-by-default (ADR-127 §7)."
                .to_string(),
        );
        "legacy-denylist"
    };

    if !rules.has_token_rules() {
        warnings.push(format!(
            "token/structural backstop INERT — no {RELEASE_RULES_CONFIG} (or it is empty). \
             Client-name / homeserver / private-crate detection is disabled in this run."
        ));
    }

    // Content backstop: per-file detectors over the shipping tree.
    for path in &tracked {
        if is_purged(&rules, path) {
            continue;
        }
        regressions.extend(audit_tracked_file(repo, &rules, path));
    }

    // Content backstop: structural strings the chain does not scrub. Narrow
    // the I/O to files git reports as containing one of the rule tokens.
    if !rules.structural_strings.is_empty() {
        let pattern = rules
            .structural_strings
            .iter()
            .map(|r| r.token.replace('.', "\\."))
            .collect::<Vec<_>>()
            .join("|");
        let marker_files =
            git_lines(repo, &["grep", "-lI", "-i", "-E", &pattern, "--", "."]).unwrap_or_default();
        for path in marker_files {
            if is_purged(&rules, &path) || is_rule_definition_source(&path) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(repo.join(&path)) else {
                continue;
            };
            for (idx, line) in text.lines().enumerate() {
                for hit in structural_hits_in_line(line, &path, &rules, &exempted) {
                    regressions.push(Regression {
                        detector: "structural-string",
                        path: path.clone(),
                        line: Some(idx + 1),
                        token: Some(hit.token),
                        detail: hit.detail,
                    });
                }
            }
        }
    }

    // Deterministic order for stable output / golden comparisons.
    regressions.sort_by(|a, b| {
        (a.path.as_str(), a.line, a.detector).cmp(&(b.path.as_str(), b.line, b.detector))
    });

    Ok(ReleaseAuditReport {
        dry_run: true,
        repo: repo.display().to_string(),
        membrane_mode,
        files_scanned: tracked.len(),
        warnings,
        regressions,
    })
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run `cs release-audit`.
///
/// Exit code is 0 on a clean report, 1 when the distribution would regress.
/// Warnings (dormant membrane, inert backstop) are printed loudly but do not
/// fail the audit.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let _ = args.dry_run; // only mode today; see Args doc.
    let repo = resolve_repo(args)?;
    let report = audit_repo(&repo)?;

    if ctx.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print_human(&report);
    }

    if report.is_clean() {
        Ok(())
    } else {
        anyhow::bail!(
            "release-audit (dry-run): {} regression(s) — the public distribution would regress",
            report.regressions.len()
        )
    }
}

/// Human-readable report.
fn print_human(report: &ReleaseAuditReport) {
    for w in &report.warnings {
        println!("\u{26A0}\u{FE0F}  {w}");
    }
    if report.is_clean() {
        println!(
            "\u{2705} release-audit (dry-run) clean: {} tracked files, no distribution regression \
             [membrane: {}] ({})",
            report.files_scanned, report.membrane_mode, report.repo,
        );
        return;
    }
    println!(
        "\u{274C} release-audit (dry-run): {} regression(s) in {} ({} files scanned, membrane: {})",
        report.regressions.len(),
        report.repo,
        report.files_scanned,
        report.membrane_mode,
    );
    for r in &report.regressions {
        let loc = match r.line {
            Some(n) => format!("{}:{n}", r.path),
            None => r.path.clone(),
        };
        let tok = r
            .token
            .as_deref()
            .map(|t| format!(" [{t}]"))
            .unwrap_or_default();
        println!("   [{}] {loc}{tok} — {}", r.detector, r.detail);
    }
}

// ---------------------------------------------------------------------------
// Tests
//
// NB: these tests use SYNTHETIC tokens (tenant-demo / widgetco / example-private.test
// / secret-broker), never real client names — the roster lives only in the
// private, purged `.cosmon/release-rules.toml` (Bucket-3, ADR-127 §6). The
// tests prove the MECHANISM; they must not re-introduce the leak they fix.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic rule set mirroring the SHAPE of the real one, with fake
    /// tokens, for exercising the pure detectors without naming a client.
    fn test_rules() -> ReleaseRules {
        ReleaseRules {
            purge_prefixes: vec![
                "docs/lore".into(),
                "docs/founding".into(),
                ".cosmon/state".into(),
                "dist".into(),
                "secretclient".into(),
            ],
            purge_exact: vec![
                "CLAUDE.local.md".into(),
                ".cosmon/release-rules.toml".into(),
                ".cosmon/release-allowlist.toml".into(),
            ],
            purge_substrings: vec!["premortem".into(), "_TTrace_".into()],
            client_tokens: vec!["tenant-demo".into(), "widgetco".into(), "surname".into()],
            // 'surname' is intentionally absent here ⇒ path-rename misses it.
            path_rename_tokens: vec!["tenant-demo".into(), "widgetco".into()],
            private_infra_crates: vec!["secret-broker".into()],
            allow_binary: vec![],
            structural_strings: vec![
                StructuralRule {
                    token: "example-private.test".into(),
                    strip_before: vec![],
                    path_exempt: None,
                    detail: "private research domain".into(),
                },
                StructuralRule {
                    token: "example.test".into(),
                    strip_before: vec!["@example.test".into(), "example-private.test".into()],
                    path_exempt: None,
                    detail: "homeserver; author email preserved".into(),
                },
                StructuralRule {
                    token: "secret-broker".into(),
                    strip_before: vec![],
                    path_exempt: Some("vendored".into()),
                    detail: "private-infra crate outside provenance".into(),
                },
            ],
        }
    }

    #[test]
    fn purged_paths_are_recognised() {
        let r = test_rules();
        assert!(is_purged(&r, "docs/lore/CHRONICLES.md"));
        assert!(is_purged(&r, "docs/founding/founding-thesis.md"));
        assert!(is_purged(&r, ".cosmon/state/events.jsonl"));
        assert!(is_purged(&r, "dist/handover/state.json"));
        assert!(is_purged(&r, "CLAUDE.local.md"));
        assert!(is_purged(&r, "docs/adr/050-something-premortem-notes.md"));
        assert!(is_purged(&r, "secretclient/QUICKSTART.md"));
        // The rules + allowlist files are themselves purged.
        assert!(is_purged(&r, ".cosmon/release-rules.toml"));
        assert!(is_purged(&r, ".cosmon/release-allowlist.toml"));
        // Non-purged shipping surface:
        assert!(!is_purged(&r, "crates/cosmon-core/src/lib.rs"));
        assert!(!is_purged(&r, "README.md"));
    }

    #[test]
    fn path_rename_covers_common_client_tokens() {
        let r = test_rules();
        assert!(
            client_tokens_surviving_path_rename(&r, "deploy/state/nucleons/nuc-tenant-demo/x.toml")
                .is_empty()
        );
        assert!(client_tokens_surviving_path_rename(&r, "docs/install-widgetco.md").is_empty());
    }

    #[test]
    fn path_rename_misses_paired_surnames() {
        let r = test_rules();
        // 'surname' is in client_tokens but NOT path_rename_tokens → survives.
        let hits = client_tokens_surviving_path_rename(&r, "docs/guides/surname-handover.md");
        assert_eq!(hits, vec!["surname".to_string()]);
    }

    #[test]
    fn structural_homeserver_leak_fires_but_author_email_is_preserved() {
        let r = test_rules();
        // Author email preserved (oxymake golden rule).
        assert!(structural_hits_in_line(
            "// contact: someone@example.test for provenance",
            "crates/x/src/lib.rs",
            &r,
            &[]
        )
        .is_empty());
        // Homeserver leak fires.
        let hits = structural_hits_in_line(
            "const HOMESERVER: &str = \"https://matrix.example.test\";",
            "crates/x/src/lib.rs",
            &r,
            &[],
        );
        assert!(hits.iter().any(|h| h.token == "example.test"));
        // Research domain fires independently.
        let hits =
            structural_hits_in_line("see example-private.test/lab", "docs/notes.md", &r, &[]);
        assert!(hits.iter().any(|h| h.token == "example-private.test"));
    }

    #[test]
    fn private_crate_string_fires_outside_provenance_only() {
        let r = test_rules();
        assert!(structural_hits_in_line(
            "// pulled from secret-broker",
            "crates/cosmon-state/src/lib.rs",
            &r,
            &[]
        )
        .iter()
        .any(|h| h.token == "secret-broker"));
        // Inside the vendoring-provenance location: tolerated.
        assert!(structural_hits_in_line(
            "// provenance: secret-broker snapshot",
            "crates/vendored/Cargo.toml",
            &r,
            &[]
        )
        .is_empty());
    }

    #[test]
    fn exemption_suppresses_one_token_but_not_the_others() {
        let r = test_rules();
        let exempted = vec!["example-private.test".to_string()];
        let hits = structural_hits_in_line(
            "# Security contact: security@example-private.test",
            "SECURITY.md",
            &r,
            &exempted,
        );
        assert!(
            hits.is_empty(),
            "exempted research domain should not flag: {hits:?}"
        );
        // Token-scoped: a genuine homeserver leak on a different token still fires.
        let hits = structural_hits_in_line(
            "const HOMESERVER: &str = \"https://matrix.example.test\";",
            "crates/x/src/lib.rs",
            &r,
            &exempted,
        );
        assert!(hits.iter().any(|h| h.token == "example.test"));
    }

    #[test]
    fn empty_rules_make_backstop_inert() {
        // A foreign clone with no rules file: token detectors disabled.
        let r = ReleaseRules::default();
        assert!(!r.has_token_rules());
        assert!(client_tokens_surviving_path_rename(&r, "docs/anything.md").is_empty());
        assert!(structural_hits_in_line("matrix.example.test", "x.rs", &r, &[]).is_empty());
        // But the purge model is also empty ⇒ nothing is treated as purged.
        assert!(!is_purged(&r, "docs/lore/CHRONICLES.md"));
    }

    #[test]
    fn audit_config_parses_exemptions_with_justification() {
        let toml = r#"
[[structural_string_exemptions]]
token = "example-private.test"
justification = "intended public maintainer-contact domain"
"#;
        let cfg: ReleaseAuditConfig = toml::from_str(toml).expect("valid config parses");
        assert_eq!(
            cfg.exempted_tokens(),
            vec!["example-private.test".to_string()]
        );
    }

    #[test]
    fn missing_config_file_is_empty_default() {
        let tmp = std::env::temp_dir().join("cosmon-release-audit-no-config-xyz");
        let cfg = load_release_audit_config(&tmp).expect("absent file is not an error");
        assert!(cfg.structural_string_exemptions.is_empty());
    }

    #[test]
    fn empty_justification_is_a_hard_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".cosmon")).expect("mkdir .cosmon");
        std::fs::write(
            dir.path().join(RELEASE_AUDIT_CONFIG),
            "[[structural_string_exemptions]]\ntoken = \"x.test\"\njustification = \"  \"\n",
        )
        .expect("write config");
        let err = load_release_audit_config(dir.path()).expect_err("blank justification rejected");
        assert!(
            err.to_string().contains("empty justification"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn manifest_detects_private_sibling_path_dep() {
        let r = test_rules();
        let toml = r#"
[dependencies]
vendored = { path = "../secret-broker/crates/vendored" }
serde = "1"
"#;
        let hits = manifest_private_path_deps(&r, toml);
        assert!(hits
            .iter()
            .any(|h| h.token.as_deref() == Some("secret-broker")));
    }

    #[test]
    fn manifest_flags_escape_above_repo_root() {
        let r = test_rules();
        let toml = "dep = { path = \"../../sibling-private/crate\" }\n";
        let hits = manifest_private_path_deps(&r, toml);
        assert!(hits
            .iter()
            .any(|h| h.detail.contains("climbs above the repo root")));
    }

    #[test]
    fn manifest_allows_in_repo_sibling() {
        let r = test_rules();
        let toml = "vendored = { path = \"../vendored\" }\nserde = \"1\"\n";
        let hits = manifest_private_path_deps(&r, toml);
        assert!(hits.is_empty(), "in-repo sibling should not flag: {hits:?}");
    }

    #[test]
    fn detector_excludes_its_own_definition_source() {
        assert!(is_rule_definition_source(
            "crates/cosmon-cli/src/cmd/release_audit.rs"
        ));
        assert!(!is_rule_definition_source(
            "crates/cosmon-cli/src/cmd/examples.rs"
        ));
    }

    #[test]
    fn instance_binding_detection() {
        assert!(is_instance_binding(
            "deploy/state/nucleons/nuc-x/oidc-identity.toml"
        ));
        assert!(!is_instance_binding(
            "deploy/state/oidc-identity.toml.example"
        ));
    }

    #[test]
    fn extract_path_value_handles_inline_and_plain() {
        assert_eq!(
            extract_path_value("vendored = { path = \"../secret-broker/x\" }"),
            Some("../secret-broker/x".to_string())
        );
        assert_eq!(
            extract_path_value("path = \"../../foo\""),
            Some("../../foo".to_string())
        );
        assert_eq!(extract_path_value("serde = \"1.0\""), None);
    }

    #[test]
    fn binary_content_detected_by_nul_byte() {
        assert!(is_binary_content(b"\x89PNG\r\n\x1a\n\x00\x00\x00"));
        assert!(is_binary_content(&[0x47, 0x49, 0x46, 0x00]));
        assert!(!is_binary_content(b"plain ascii text"));
        assert!(!is_binary_content("Crème brûlée".as_bytes()));
        assert!(!is_binary_content(b""));
    }

    // -- Allowlist (deny-by-default) tests --------------------------------

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(p, body).expect("write");
    }

    #[test]
    fn missing_allowlist_is_legacy_mode() {
        let tmp = std::env::temp_dir().join("cosmon-allowlist-absent-xyz");
        let allow = load_allowlist(&tmp).expect("absent file is not an error");
        assert!(allow.is_none(), "absent allowlist ⇒ legacy mode");
    }

    #[test]
    fn permit_with_blank_reason_is_a_hard_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            ".cosmon/release-allowlist.toml",
            "[[permit]]\npath=\"README.md\"\ncleared_at=\"2026-06-17\"\ncleared_by=\"op\"\nreason=\"  \"\n",
        );
        let err = load_allowlist(dir.path()).expect_err("blank reason rejected");
        assert!(
            err.to_string().contains("missing reason"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn path_not_permitted_fires_for_uncleared_tracked_path() {
        let r = ReleaseRules::default();
        let allow = Allowlist {
            permits: vec![Permit {
                path: "README.md".into(),
                cleared_at: "2026-06-17".into(),
                cleared_by: "op".into(),
                reason: "readme".into(),
                seal: None,
            }],
        };
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "README.md", "hello");
        write(dir.path(), "secret.rs", "client surface");
        let tracked = vec!["README.md".to_string(), "secret.rs".to_string()];
        let regs = audit_allowlist(dir.path(), &r, &allow, &tracked);
        // README permitted; secret.rs is not → exactly one path-not-permitted.
        let np: Vec<_> = regs
            .iter()
            .filter(|x| x.detector == "path-not-permitted")
            .collect();
        assert_eq!(np.len(), 1);
        assert_eq!(np[0].path, "secret.rs");
    }

    #[test]
    fn purged_paths_need_no_permit() {
        let mut r = ReleaseRules::default();
        r.purge_prefixes = vec!["docs/lore".into()];
        let allow = Allowlist { permits: vec![] };
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "docs/lore/x.md", "secret chronicle");
        let tracked = vec!["docs/lore/x.md".to_string()];
        let regs = audit_allowlist(dir.path(), &r, &allow, &tracked);
        assert!(regs.is_empty(), "purged path needs no permit: {regs:?}");
    }

    #[test]
    fn content_bound_permit_goes_stale_on_edit() {
        let r = ReleaseRules::default();
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "tmpl.toml", "version one");
        let good_seal = content_seal(b"version one");
        let allow = Allowlist {
            permits: vec![Permit {
                path: "tmpl.toml".into(),
                cleared_at: "2026-06-17".into(),
                cleared_by: "op".into(),
                reason: "template".into(),
                seal: Some(good_seal.clone()),
            }],
        };
        let tracked = vec!["tmpl.toml".to_string()];
        // Matching seal ⇒ clean.
        assert!(audit_allowlist(dir.path(), &r, &allow, &tracked).is_empty());
        // Edit the file ⇒ permit-stale.
        write(
            dir.path(),
            "tmpl.toml",
            "version two — now with a client name",
        );
        let regs = audit_allowlist(dir.path(), &r, &allow, &tracked);
        assert!(
            regs.iter().any(|x| x.detector == "permit-stale"),
            "{regs:?}"
        );
    }

    #[test]
    fn orphan_permit_is_flagged() {
        let r = ReleaseRules::default();
        let allow = Allowlist {
            permits: vec![Permit {
                path: "deleted.rs".into(),
                cleared_at: "2026-06-17".into(),
                cleared_by: "op".into(),
                reason: "gone".into(),
                seal: None,
            }],
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let tracked: Vec<String> = vec![]; // deleted.rs no longer tracked
        let regs = audit_allowlist(dir.path(), &r, &allow, &tracked);
        assert!(
            regs.iter().any(|x| x.detector == "permit-orphan"),
            "{regs:?}"
        );
    }

    #[test]
    fn content_seal_is_blake3_prefixed() {
        let s = content_seal(b"abc");
        assert!(s.starts_with("blake3:"));
        assert_eq!(content_seal(b"abc"), s, "seal is deterministic");
        assert_ne!(content_seal(b"abd"), s, "seal is content-sensitive");
    }

    #[test]
    fn clean_report_is_clean() {
        let report = ReleaseAuditReport {
            dry_run: true,
            repo: "/tmp/x".to_string(),
            membrane_mode: "allowlist",
            files_scanned: 3,
            warnings: vec![],
            regressions: vec![],
        };
        assert!(report.is_clean());
    }

    #[test]
    fn ci_contains_is_unicode_case_insensitive() {
        assert!(ci_contains("Path/To/TENANT-DEMO/file", "tenant-demo"));
        assert!(ci_contains("CRÈME", "crème"));
        assert!(!ci_contains("clean/path", "tenant-demo"));
    }
}
