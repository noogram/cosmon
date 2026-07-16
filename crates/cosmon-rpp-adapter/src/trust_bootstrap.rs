// SPDX-License-Identifier: AGPL-3.0-only

//! Boot-time trust bootstrap — the server provisions its **own** authn
//! allowlist and authz binding (ADR-141), absorbing the v3.0
//! `cosmon-seed` init-container.
//!
//! # What this module owns
//!
//! The **convergence** of two declaration files, at boot, before the
//! JWKS fetch is armed:
//!
//! - `<state_dir>/security/trusted-issuers.toml` — the authn allowlist
//!   ([`crate::jwks_fetch::TrustedIssuers`] canon: `[[issuer]]` with
//!   `iss` / `jwks_uri` / `audiences`);
//! - `<state_dir>/nucleons/<id>/oidc-identity.toml` — the authz binding,
//!   rendered by the **same** audited renderer as the operator path
//!   ([`crate::nucleon_map::render_oidc_identity_toml`] — one writer
//!   schema, zero drift).
//!
//! Declarations come from three sources, highest precedence first:
//!
//! 1. **handoff files** — `*.toml` under the configured `handoff_dir`,
//!    written by a sibling `IdP` image (the self-provisioning Forgejo,
//!    ADR-141 contract C) on a dedicated volume the server mounts
//!    **read-only**. This is how the `OAuth2` `client_id` crosses the
//!    container boundary: a file on a shared volume, never an env var.
//! 2. the **env trio** `TRUSTED_ISS` / `TRUSTED_JWKS_URI` /
//!    `TRUSTED_AUDIENCES` — compat with the retired seed contract, for
//!    external-`IdP` deployments with no handoff.
//! 3. static `[[trust_bootstrap.issuer]]` entries in `rpp.toml` — the
//!    LEAN-canonical global-config surface.
//!
//! # Convergence semantics (auth-B1 lineage, in Rust)
//!
//! The round-2 re-review blockers this design carries forward:
//!
//! - **merge-preserving** — a declared entry is matched by `iss` and
//!   rewritten on drift; every *foreign* `[[issuer]]` block on the
//!   volume is preserved **verbatim** (comments included). A
//!   legitimately-enriched multi-issuer allowlist survives every boot.
//! - **fail-closed parse-back** — the merged text is deserialized with
//!   the *same* serde shape the fetcher consumes, plus shape checks
//!   (≥ 1 issuer, non-empty `iss`, ≥ 1 non-empty audience), **before**
//!   it atomically replaces the on-volume file. A degenerate result
//!   refuses the boot; under `restart: unless-stopped` that is a
//!   self-healing crash-loop, never a silent deny-all.
//! - **reset gesture** — `TRUSTED_FORCE=1` rewrites the whole allowlist
//!   from the declaration (cross-tenant volume reuse, corrupt-file
//!   recovery).
//! - **bounded wait** — with a `handoff_dir` configured and
//!   `handoff_wait_secs > 0`, a boot that finds *nothing* declared and
//!   *nothing* on the volume polls for the handoff and errors out when
//!   it never lands. No `depends_on` ordering is needed — this survives
//!   the engine-level restart-after-host-reboot where compose ordering
//!   does not apply.
//!
//! The operator one-shot `cosmon-rpp-adapter trust converge` runs the
//! same [`converge`] and exits — the validation bench drives it exactly
//! like the old seed entrypoint.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::auth::scopes;
use crate::jwks_fetch::TrustedIssuers;
use crate::nucleon_map::{render_oidc_identity_toml, HabilitationBindingSpec, RenderError};

/// Handoff schema identifier accepted by this reader (contract C).
pub const HANDOFF_SCHEMA: &str = "cosmon-issuer-handoff/v1";

/// Default scopes granted by a handoff-declared binding when the
/// handoff does not name its own. The v1 tenant surface: read/write
/// molecules, spawn workers, read artifacts.
pub const DEFAULT_BINDING_SCOPES: &[&str] = &[
    scopes::MOLECULE_READ,
    scopes::MOLECULE_WRITE,
    scopes::WORKER_SPAWN,
    scopes::ARTIFACT_READ,
];

/// `[trust_bootstrap]` section of `rpp.toml` (global-config surface,
/// LEAN.md working agreement — per-deployment trust is *declared*, the
/// server converges to it).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TrustBootstrapSection {
    /// Directory scanned for `*.toml` handoff files (ADR-141 contract
    /// C). Mounted read-only from the `provision-handoff` volume in the
    /// Tenant-Demo compose. Absent → no handoff ingestion.
    pub handoff_dir: Option<PathBuf>,
    /// Upper bound, in seconds, on the boot-time wait for a first
    /// handoff when nothing is declared and nothing is on the volume.
    /// `0` (default) → never wait, never fail for absence (legacy
    /// benches and file-stage deployments keep booting).
    pub handoff_wait_secs: Option<u64>,
    /// Static issuer declarations (`[[trust_bootstrap.issuer]]`) —
    /// the complete-allowlist declaration surface that replaces the
    /// seed's `TRUSTED_ISSUERS_FILE` path.
    pub issuer: Vec<DeclaredIssuer>,
}

/// One declared issuer — the same three load-bearing fields as the
/// [`crate::jwks_fetch::TrustedIssuer`] canon (`iss` external, matched
/// byte-for-byte; `jwks_uri` internal fetch target; `audiences` the
/// `aud` allowlist).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct DeclaredIssuer {
    /// External issuer URL burned into the token. Never a fetch target.
    pub iss: String,
    /// Internal JWKS fetch URL. Omitted → `.well-known` discovery
    /// (mono-DNS providers only).
    #[serde(default)]
    pub jwks_uri: Option<String>,
    /// `aud` claims this issuer may mint. Must be non-empty.
    #[serde(default)]
    pub audiences: Vec<String>,
}

/// Authz half of a handoff document — becomes an `oidc-identity.toml`
/// rendered through the audited [`crate::nucleon_map`] path.
#[derive(Clone, Debug, Deserialize)]
pub struct HandoffBinding {
    /// Tenant axis (galaxy slot) the `(iss, sub)` is scoped to.
    pub noyau: String,
    /// JWT `sub` claim — the real `IdP` user id, never assumed.
    pub sub: String,
    /// JWT `aud` pinned by the binding. Omitted → first issuer audience.
    #[serde(default)]
    pub audience: Option<String>,
    /// Directory name under `nucleons/`. Omitted → `<noyau>`.
    #[serde(default)]
    pub nucleon_id: Option<String>,
    /// Binding-granted scopes. Omitted → [`DEFAULT_BINDING_SCOPES`].
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    /// Cognitive-substrate label (ADR-063). Omitted → `Biological`.
    #[serde(default)]
    pub phase: Option<String>,
}

/// One handoff file (ADR-141 contract C).
#[derive(Clone, Debug, Deserialize)]
pub struct HandoffDoc {
    /// Schema tag — must be [`HANDOFF_SCHEMA`] when present.
    #[serde(default)]
    pub schema: Option<String>,
    /// The issuer this `IdP` asks the server to trust.
    pub issuer: DeclaredIssuer,
    /// Optional authz binding for the provisioned principal — the
    /// legacy single-`[binding]` shape, kept verbatim for back-compat
    /// (it converges to `nucleons/<nucleon_id>/oidc-identity.toml`
    /// exactly as before). New provisioners that publish more than one
    /// audience use the plural [`Self::bindings`] array instead.
    #[serde(default)]
    pub binding: Option<HandoffBinding>,
    /// Additional authz bindings — the plural `[[bindings]]` array
    /// (`task-20260710-6ffc`, the two-OAuth-app / two-audience
    /// provisioner). This is the G5 fan-out on the handoff side: **one**
    /// self-provisioning gesture publishes **N** habilitations, one per
    /// audience (`aud=A` = the CLI/API app `cs-rpp-adapter`, `aud=B` =
    /// the MCP connector app `claude-web`). Each entry MUST carry a
    /// **distinct** effective `nucleon_id` — the audience pins the galaxy
    /// but the *file* is keyed by `nucleon_id`, so two bindings sharing a
    /// `nucleon_id` would collide on the same `oidc-identity.toml`; the
    /// convergence refuses that with a loud [`TrustBootstrapError::InvalidHandoff`].
    /// Additive + unknown-field-tolerant: a server that predates this
    /// field simply ignores it and seals only the legacy `[binding]`
    /// (graceful, since the kernel ships server + provisioner together).
    #[serde(default)]
    pub bindings: Vec<HandoffBinding>,
}

impl HandoffDoc {
    /// Every authz binding this handoff declares, legacy single-`[binding]`
    /// first (stable order), then each plural `[[bindings]]` entry. Empty
    /// when the handoff is issuer-only (an allowlist-without-binding
    /// declaration is legal — the binding half is optional).
    fn all_bindings(&self) -> Vec<&HandoffBinding> {
        let mut out: Vec<&HandoffBinding> = Vec::new();
        if let Some(b) = &self.binding {
            out.push(b);
        }
        out.extend(self.bindings.iter());
        out
    }
}

/// Errors that refuse the boot (fail-closed — the server never arms
/// itself off a degenerate trust declaration).
#[derive(Debug, thiserror::Error)]
pub enum TrustBootstrapError {
    /// Filesystem error touching the allowlist, a handoff, or a binding.
    #[error("io on {path}: {source}")]
    Io {
        /// Path the operation failed on.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// A handoff file exists but is not a valid handoff document.
    #[error("handoff {path} is invalid: {reason} — fix or remove the file (fail-closed)")]
    InvalidHandoff {
        /// Offending handoff file.
        path: PathBuf,
        /// Parse or schema failure.
        reason: String,
    },
    /// A declared value cannot be rendered into a TOML string safely.
    #[error("declared {field} contains a character unfit for a TOML string: {value:?}")]
    UnrenderableValue {
        /// Field name (`iss`, `jwks_uri`, `audience`).
        field: &'static str,
        /// Offending value.
        value: String,
    },
    /// A declared issuer is structurally unusable (empty `iss` or no
    /// audience).
    #[error("declared issuer {iss:?} is degenerate: {reason}")]
    DegenerateDeclaration {
        /// The declared `iss` (possibly empty).
        iss: String,
        /// Why it is refused.
        reason: String,
    },
    /// The merged allowlist failed the fail-closed parse-back. The
    /// on-volume file was NOT replaced.
    #[error(
        "merged allowlist failed parse-back ({reason}) — on-volume file untouched; \
         recover with TRUSTED_FORCE=1 (full rewrite from the declaration)"
    )]
    ParseBack {
        /// Validation failure detail.
        reason: String,
    },
    /// The bounded handoff wait expired with nothing declared and
    /// nothing on the volume.
    #[error(
        "no trust declared after waiting {waited_secs}s for a handoff in {handoff_dir} — \
         refusing to boot deny-all; the restart policy will retry (self-healing)"
    )]
    HandoffTimeout {
        /// Seconds actually waited.
        waited_secs: u64,
        /// Directory that was polled.
        handoff_dir: PathBuf,
    },
    /// A handoff binding could not be rendered by the audited renderer.
    #[error("handoff binding for {nucleon_id}: {source}")]
    Binding {
        /// Target directory name under `nucleons/`.
        nucleon_id: String,
        /// Renderer refusal.
        source: RenderError,
    },
}

/// What one [`converge`] pass did — logged at boot, printed by the
/// `trust converge` operator one-shot.
#[derive(Debug, Default)]
pub struct ConvergeReport {
    /// Issuers declared this pass (handoff + env + static, deduped).
    pub declared: usize,
    /// Handoff files ingested.
    pub handoff_files: usize,
    /// `true` ⇒ the allowlist file was (re)written this pass.
    pub wrote_allowlist: bool,
    /// Total `[[issuer]]` entries in the final allowlist.
    pub issuers_total: usize,
    /// Foreign (non-declared) entries preserved verbatim.
    pub foreign_preserved: usize,
    /// Bindings written or rewritten this pass (nucleon ids).
    pub bindings_written: Vec<String>,
    /// Bindings already semantically converged (left untouched).
    pub bindings_unchanged: usize,
    /// Seconds spent waiting for a first handoff (0 = no wait).
    pub waited_secs: u64,
}

impl ConvergeReport {
    /// Structured boot log of the pass.
    pub fn log(&self) {
        tracing::info!(
            event = "boot.trust_bootstrap",
            declared = self.declared,
            handoff_files = self.handoff_files,
            wrote_allowlist = self.wrote_allowlist,
            issuers_total = self.issuers_total,
            foreign_preserved = self.foreign_preserved,
            bindings_written = ?self.bindings_written,
            bindings_unchanged = self.bindings_unchanged,
            waited_secs = self.waited_secs,
            "trust bootstrap converged (ADR-141)",
        );
    }
}

/// Read the env-trio declaration (`TRUSTED_ISS` / `TRUSTED_JWKS_URI` /
/// `TRUSTED_AUDIENCES`) from the process environment. Absent or empty
/// `TRUSTED_ISS` → no env declaration.
#[must_use]
pub fn env_declared_issuer() -> Option<DeclaredIssuer> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let iss = get("TRUSTED_ISS")?;
    let jwks_uri = get("TRUSTED_JWKS_URI");
    let audiences = get("TRUSTED_AUDIENCES")
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(DeclaredIssuer {
        iss,
        jwks_uri,
        audiences,
    })
}

/// `TRUSTED_FORCE=1` in the environment — the full-rewrite reset
/// gesture (cross-tenant volume reuse, corrupt-file recovery).
#[must_use]
pub fn env_force() -> bool {
    std::env::var("TRUSTED_FORCE").is_ok_and(|v| v.trim() == "1")
}

/// Converge the trust state under `state_dir` from the configured
/// declaration sources, reading the env trio and `TRUSTED_FORCE` from
/// the process environment. See module docs for semantics.
///
/// # Errors
///
/// Any [`TrustBootstrapError`] — every one of them means the server
/// must NOT arm itself (fail-closed); the caller refuses the boot.
pub fn converge(
    state_dir: &Path,
    section: &TrustBootstrapSection,
) -> Result<ConvergeReport, TrustBootstrapError> {
    converge_with(state_dir, section, env_declared_issuer(), env_force())
}

/// [`converge`] with the environment-derived inputs made explicit
/// (deterministic for tests).
///
/// # Errors
///
/// See [`converge`].
pub fn converge_with(
    state_dir: &Path,
    section: &TrustBootstrapSection,
    env_issuer: Option<DeclaredIssuer>,
    force: bool,
) -> Result<ConvergeReport, TrustBootstrapError> {
    let mut report = ConvergeReport::default();
    let allowlist_path = state_dir.join("security/trusted-issuers.toml");

    let handoffs = gather_handoffs(section, env_issuer.as_ref(), &allowlist_path, &mut report)?;
    report.handoff_files = handoffs.len();

    // ── gather declarations, highest precedence first, dedup by iss ──
    let mut declared: Vec<DeclaredIssuer> = Vec::new();
    let mut push = |it: DeclaredIssuer| {
        if !declared.iter().any(|d| d.iss == it.iss) {
            declared.push(it);
        }
    };
    for doc in &handoffs {
        push(doc.1.issuer.clone());
    }
    if let Some(env_it) = env_issuer {
        push(env_it);
    }
    for it in &section.issuer {
        push(it.clone());
    }
    report.declared = declared.len();

    // ── converge the allowlist (skip entirely when nothing declared:
    //    a hand-posed file, or file-stage fallback, stays untouched) ──
    if !declared.is_empty() {
        for d in &declared {
            validate_declared(d)?;
        }
        let existing = match std::fs::read_to_string(&allowlist_path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(TrustBootstrapError::Io {
                    path: allowlist_path.clone(),
                    source: e,
                })
            }
        };
        let merged = merge_allowlist(existing.as_deref(), &declared, force);
        let total = validate_allowlist(&merged)?;
        report.issuers_total = total;
        report.foreign_preserved = total.saturating_sub(declared.len());
        if existing.as_deref() != Some(merged.as_str()) {
            write_atomically(&allowlist_path, &merged)?;
            report.wrote_allowlist = true;
        }
    }

    // ── converge the handoff-declared bindings (authz half) ──
    for (path, doc) in &handoffs {
        converge_doc_bindings(state_dir, path, doc, &mut report)?;
    }

    Ok(report)
}

/// The effective `nucleon_id` a binding converges to (the directory name
/// under `nucleons/`): the explicit `nucleon_id`, or `noyau` when omitted.
/// This is the *file key* — two bindings with the same effective
/// `nucleon_id` would write the same `oidc-identity.toml`.
fn effective_nucleon_id(binding: &HandoffBinding) -> String {
    binding
        .nucleon_id
        .clone()
        .unwrap_or_else(|| binding.noyau.clone())
}

/// The effective `aud` a binding pins: its explicit `audience`, or the
/// issuer's first declared audience as the documented fallback. This is
/// the third component of the `(iss, sub, audience)` triple that
/// [`crate::nucleon_map::HabilitationMap::load`] keys every habilitation
/// by — so it is also the third component of the *load key* the
/// convergence must dedup on (not the `nucleon_id` file key alone).
fn effective_audience(
    doc: &HandoffDoc,
    binding: &HandoffBinding,
    path: &Path,
) -> Result<String, TrustBootstrapError> {
    match &binding.audience {
        Some(a) => Ok(a.clone()),
        None => doc.issuer.audiences.first().cloned().ok_or_else(|| {
            TrustBootstrapError::InvalidHandoff {
                path: path.to_path_buf(),
                reason: "binding has no audience and issuer declares no audiences".into(),
            }
        }),
    }
}

/// Converge every binding a handoff declares (legacy `[binding]` +
/// plural `[[bindings]]`), each into its own
/// `nucleons/<nucleon_id>/oidc-identity.toml`.
///
/// The plural array is the two-audience provisioner's G5 fan-out
/// (`task-20260710-6ffc`): `aud=A` and `aud=B` for the same `(iss, sub)`
/// land in two physically distinct sealed slots, so a token carrying
/// `aud=A` can only ever resolve binding A (audience isolation is
/// structural, not aspirational). Because the *file* is keyed by
/// `nucleon_id` rather than by the (rotating) audience, a `client_id`
/// rotation rewrites the same file in place — no orphaned stale binding
/// for a defunct audience.
///
/// Fails closed on **either** of two distinct collisions within one
/// handoff:
///
/// 1. A **duplicate effective `nucleon_id`** (the *file* key): two
///    bindings targeting the same `nucleons/<id>/` directory would
///    silently overwrite each other's `oidc-identity.toml` (only one
///    audience ends up sealed).
/// 2. A **duplicate effective `(iss, sub, audience)` triple** (the *load*
///    key): [`crate::nucleon_map::HabilitationMap::load`] keys every
///    habilitation by that triple over an **unsorted** `read_dir`. Two
///    bindings that resolve to *distinct* `nucleon_id` directories but
///    share the triple — the canonical way to trip this is both omitting
///    `audience`, so both fall back to `issuer.audiences[0]` — each seal
///    their own file, pass guard (1), then silently collapse to **one**
///    at load. Which one wins is last-writer over the unsorted directory
///    walk: non-deterministic across reboots, and both seals read green
///    because each file is internally intact. This is the F1 finding of
///    review `task-20260710-37f8`: audience isolation must be enforced on
///    the *real* isolation key, not on `nucleon_id` alone.
///
/// Both refuse the boot loudly instead of arming a non-deterministic map.
fn converge_doc_bindings(
    state_dir: &Path,
    path: &Path,
    doc: &HandoffDoc,
    report: &mut ConvergeReport,
) -> Result<(), TrustBootstrapError> {
    let bindings = doc.all_bindings();
    if bindings.is_empty() {
        return Ok(());
    }
    let mut seen_nid = std::collections::BTreeSet::new();
    let mut seen_triple = std::collections::BTreeSet::new();
    for binding in &bindings {
        // Guard (1): the file key — distinct nucleons/<id>/ directories.
        let nid = effective_nucleon_id(binding);
        if !seen_nid.insert(nid.clone()) {
            return Err(TrustBootstrapError::InvalidHandoff {
                path: path.to_path_buf(),
                reason: format!(
                    "two bindings share nucleon_id {nid:?} — each binding must \
                     target a distinct nucleons/<id>/ directory (the file is keyed \
                     by nucleon_id, not by audience)"
                ),
            });
        }
        // Guard (2): the load key — distinct (iss, sub, audience) triples.
        // This is the real isolation key HabilitationMap resolves on, and
        // is what guard (1) alone silently lets collide (F1).
        let triple = (
            doc.issuer.iss.clone(),
            binding.sub.clone(),
            effective_audience(doc, binding, path)?,
        );
        if !seen_triple.insert(triple.clone()) {
            let (iss, sub, aud) = triple;
            return Err(TrustBootstrapError::InvalidHandoff {
                path: path.to_path_buf(),
                reason: format!(
                    "two bindings resolve to the same effective (iss, sub, audience) = \
                     ({iss:?}, {sub:?}, {aud:?}) — HabilitationMap keys every habilitation \
                     by this triple, so distinct nucleon_id directories would collapse to \
                     one at load (last-writer-wins over an unsorted directory walk, \
                     non-deterministic across reboots). Pin each binding to a distinct \
                     audience rather than letting both fall back to issuer.audiences[0]"
                ),
            });
        }
    }
    for binding in bindings {
        converge_binding(state_dir, path, doc, binding, report)?;
    }
    Ok(())
}

/// Read the handoff dir, applying the bounded first-boot wait.
///
/// The wait engages only when the handoff dir actually EXISTS — i.e.
/// the provision-handoff volume is mounted, so an `IdP` sibling is part
/// of this deployment — and nothing else is declared or already on the
/// volume. A standalone `docker run` (no volumes; the configured dir is
/// absent) boots immediately, deny-all, exactly as before ADR-141.
fn gather_handoffs(
    section: &TrustBootstrapSection,
    env_issuer: Option<&DeclaredIssuer>,
    allowlist_path: &Path,
    report: &mut ConvergeReport,
) -> Result<Vec<(PathBuf, HandoffDoc)>, TrustBootstrapError> {
    let wait = Duration::from_secs(section.handoff_wait_secs.unwrap_or(0));
    let handoff_dir_mounted = section.handoff_dir.as_deref().is_some_and(Path::is_dir);
    let mut handoffs = read_handoff_dir(section.handoff_dir.as_deref())?;
    if handoffs.is_empty()
        && handoff_dir_mounted
        && section.issuer.is_empty()
        && env_issuer.is_none()
        && !wait.is_zero()
        && !allowlist_has_entries(allowlist_path)
    {
        let handoff_dir = section
            .handoff_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));
        let started = std::time::Instant::now();
        while started.elapsed() < wait {
            std::thread::sleep(Duration::from_secs(1));
            handoffs = read_handoff_dir(section.handoff_dir.as_deref())?;
            if !handoffs.is_empty() {
                break;
            }
        }
        report.waited_secs = started.elapsed().as_secs();
        if handoffs.is_empty() {
            return Err(TrustBootstrapError::HandoffTimeout {
                waited_secs: report.waited_secs,
                handoff_dir,
            });
        }
    }
    Ok(handoffs)
}

/// Converge ONE handoff binding into `nucleons/<id>/oidc-identity.toml`,
/// via the audited renderer. Semantic idempotence: an existing file that
/// parses to the same TOML value is left untouched. The `doc` supplies
/// the issuer (`iss` byte-for-byte) and the audience fallback when the
/// binding omits its own `audience`.
fn converge_binding(
    state_dir: &Path,
    path: &Path,
    doc: &HandoffDoc,
    binding: &HandoffBinding,
    report: &mut ConvergeReport,
) -> Result<(), TrustBootstrapError> {
    let audience = effective_audience(doc, binding, path)?;
    let spec = HabilitationBindingSpec {
        noyau: binding.noyau.clone(),
        sub: binding.sub.clone(),
        issuer: doc.issuer.iss.clone(),
        audience,
        nucleon_id: binding.nucleon_id.clone(),
        phase: binding.phase.clone(),
        scopes: binding.scopes.clone().unwrap_or_else(|| {
            DEFAULT_BINDING_SCOPES
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        }),
        sealed_at: None,
    };
    let nucleon_id = spec
        .nucleon_id
        .clone()
        .unwrap_or_else(|| spec.noyau.clone());
    let rendered =
        render_oidc_identity_toml(&spec).map_err(|source| TrustBootstrapError::Binding {
            nucleon_id: nucleon_id.clone(),
            source,
        })?;
    let binding_path = state_dir
        .join("nucleons")
        .join(&nucleon_id)
        .join("oidc-identity.toml");
    if binding_semantically_equal(&binding_path, &rendered) {
        report.bindings_unchanged += 1;
    } else {
        write_atomically(&binding_path, &rendered)?;
        report.bindings_written.push(nucleon_id);
    }
    Ok(())
}

/// `true` when the allowlist file exists and declares at least one
/// `[[issuer]]` entry (used only to decide whether a first-boot wait is
/// warranted — the authoritative validation is [`validate_allowlist`]).
fn allowlist_has_entries(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str::<TrustedIssuers>(&text).is_ok_and(|t| !t.is_empty()),
        Err(_) => false,
    }
}

/// Read and parse every `*.toml` under `dir` (sorted by file name for
/// deterministic precedence). A missing or unset dir is an empty set; a
/// present-but-malformed handoff is a hard error (fail-closed).
fn read_handoff_dir(dir: Option<&Path>) -> Result<Vec<(PathBuf, HandoffDoc)>, TrustBootstrapError> {
    let Some(dir) = dir else {
        return Ok(Vec::new());
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(TrustBootstrapError::Io {
                path: dir.to_path_buf(),
                source: e,
            })
        }
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    paths.sort();
    let mut docs = Vec::with_capacity(paths.len());
    for path in paths {
        let text = std::fs::read_to_string(&path).map_err(|source| TrustBootstrapError::Io {
            path: path.clone(),
            source,
        })?;
        let doc: HandoffDoc =
            toml::from_str(&text).map_err(|e| TrustBootstrapError::InvalidHandoff {
                path: path.clone(),
                reason: e.to_string(),
            })?;
        if let Some(schema) = &doc.schema {
            if schema != HANDOFF_SCHEMA {
                return Err(TrustBootstrapError::InvalidHandoff {
                    path: path.clone(),
                    reason: format!("unknown schema {schema:?} (expected {HANDOFF_SCHEMA:?})"),
                });
            }
        }
        docs.push((path, doc));
    }
    Ok(docs)
}

/// Refuse a declared issuer that is degenerate or unrenderable —
/// BEFORE it can reach the on-volume file.
fn validate_declared(d: &DeclaredIssuer) -> Result<(), TrustBootstrapError> {
    if d.iss.trim().is_empty() {
        return Err(TrustBootstrapError::DegenerateDeclaration {
            iss: d.iss.clone(),
            reason: "empty iss".into(),
        });
    }
    if d.audiences.iter().all(|a| a.trim().is_empty()) {
        return Err(TrustBootstrapError::DegenerateDeclaration {
            iss: d.iss.clone(),
            reason: "no non-empty audience".into(),
        });
    }
    renderable("iss", &d.iss)?;
    if let Some(u) = &d.jwks_uri {
        renderable("jwks_uri", u)?;
    }
    for a in &d.audiences {
        renderable("audience", a)?;
    }
    Ok(())
}

/// A value is renderable when it can be emitted inside a basic TOML
/// double-quoted string with **no escaping** — anything else (quotes,
/// backslashes, control chars) is refused rather than escaped, because
/// no legitimate URL or OAuth client id contains them.
fn renderable(field: &'static str, value: &str) -> Result<(), TrustBootstrapError> {
    if value.contains(['"', '\\']) || value.chars().any(char::is_control) {
        return Err(TrustBootstrapError::UnrenderableValue {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// Render one declared entry in the `jwks_fetch.rs` canon shape.
fn render_entry(d: &DeclaredIssuer) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("[[issuer]]\n");
    out.push_str("# managed by trust-bootstrap (declared entry; foreign entries are preserved)\n");
    // write! into a String is infallible; the results are discarded.
    let _ = writeln!(out, "iss = \"{}\"", d.iss);
    if let Some(u) = &d.jwks_uri {
        let _ = writeln!(out, "jwks_uri = \"{u}\"");
    }
    let auds: Vec<String> = d
        .audiences
        .iter()
        .filter(|a| !a.trim().is_empty())
        .map(|a| format!("\"{a}\""))
        .collect();
    let _ = writeln!(out, "audiences = [{}]", auds.join(", "));
    out
}

/// Header written when the allowlist is created from scratch (or force-
/// rewritten).
const FRESH_HEADER: &str = "# Converged by the cosmon-server trust bootstrap at boot (ADR-141,\n\
     # self-service auth). Its presence arms the JWKS HTTP-fetch path.\n\
     # Canon: jwks_fetch.rs — [[issuer]] with iss / jwks_uri / audiences.\n";

/// Merge the declared entries into the existing allowlist text.
///
/// Block-based, byte-preserving for everything not declared: the
/// existing file is split on `[[issuer]]` headers; a block whose `iss`
/// matches a declared entry is replaced by the freshly-rendered entry
/// (first occurrence — duplicates of the same `iss` are collapsed);
/// every other block travels verbatim, comments included. Declared
/// entries absent from the file are appended. `force` (or no existing
/// file) renders the declaration wholesale.
#[must_use]
pub fn merge_allowlist(existing: Option<&str>, declared: &[DeclaredIssuer], force: bool) -> String {
    let rendered: Vec<(String, String)> = declared
        .iter()
        .map(|d| (d.iss.clone(), render_entry(d)))
        .collect();

    let fresh = || {
        let mut out = String::from(FRESH_HEADER);
        for (_, entry) in &rendered {
            out.push('\n');
            out.push_str(entry);
        }
        out
    };

    let Some(existing) = existing else {
        return fresh();
    };
    if force {
        return fresh();
    }

    // Split into preamble + [[issuer]] blocks.
    let mut preamble = String::new();
    let mut blocks: Vec<String> = Vec::new();
    for line in existing.lines() {
        if line.trim_start().starts_with("[[issuer]]") {
            blocks.push(String::new());
        }
        let target = blocks.last_mut().unwrap_or(&mut preamble);
        target.push_str(line);
        target.push('\n');
    }

    let block_iss = |block: &str| -> Option<String> {
        block.lines().find_map(|l| {
            let rest = l.trim_start().strip_prefix("iss")?;
            let rest = rest.trim_start().strip_prefix('=')?;
            let rest = rest.trim_start().strip_prefix('"')?;
            rest.split('"').next().map(str::to_owned)
        })
    };

    let mut out = preamble;
    let mut replaced: Vec<&str> = Vec::new();
    for block in &blocks {
        match block_iss(block) {
            Some(iss) if rendered.iter().any(|(d_iss, _)| *d_iss == iss) => {
                if replaced.iter().any(|r| *r == iss) {
                    continue; // collapse duplicate declared-iss blocks
                }
                if let Some((d_iss, entry)) = rendered.iter().find(|(d_iss, _)| *d_iss == iss) {
                    // Rewrite ONLY on drift: when the on-volume block already
                    // carries the declared values, keep its bytes verbatim so
                    // an identical re-declaration is a write-free no-op on
                    // the FIRST pass (not merely at the second-pass fixed
                    // point) — reboots never touch the file.
                    let declared_entry = declared.iter().find(|d| d.iss == *d_iss);
                    if declared_entry.is_some_and(|d| block_matches_declared(block, d)) {
                        out.push_str(block);
                    } else {
                        out.push_str(entry);
                    }
                    replaced.push(d_iss);
                }
            }
            _ => out.push_str(block),
        }
    }
    for (d_iss, entry) in &rendered {
        if !replaced.iter().any(|r| r == d_iss) {
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str(entry);
        }
    }
    out
}

/// `true` when a single `[[issuer]]` block's parsed values equal the
/// declared entry (`iss`, `jwks_uri`, `audiences`) — formatting and
/// comments ignored. Drives the rewrite-only-on-drift rule of
/// [`merge_allowlist`].
fn block_matches_declared(block: &str, declared: &DeclaredIssuer) -> bool {
    let Ok(parsed) = toml::from_str::<TrustedIssuers>(block) else {
        return false;
    };
    let [entry] = parsed.issuers.as_slice() else {
        return false;
    };
    entry.iss == declared.iss
        && entry.jwks_uri == declared.jwks_uri
        && entry.audiences == declared.audiences
}

/// Fail-closed parse-back: deserialize `text` with the SAME shape the
/// fetcher consumes and apply the shape checks (≥ 1 issuer, non-empty
/// `iss`, ≥ 1 non-empty audience per entry). Returns the issuer count.
///
/// # Errors
///
/// [`TrustBootstrapError::ParseBack`] describing the first defect.
pub fn validate_allowlist(text: &str) -> Result<usize, TrustBootstrapError> {
    let parsed: TrustedIssuers =
        toml::from_str(text).map_err(|e| TrustBootstrapError::ParseBack {
            reason: format!("not parseable as the jwks_fetch canon: {e}"),
        })?;
    if parsed.is_empty() {
        return Err(TrustBootstrapError::ParseBack {
            reason: "no [[issuer]] entry at all".into(),
        });
    }
    for (i, issuer) in parsed.issuers.iter().enumerate() {
        if issuer.iss.trim().is_empty() {
            return Err(TrustBootstrapError::ParseBack {
                reason: format!("issuer #{} has an empty iss", i + 1),
            });
        }
        if issuer.audiences.iter().all(|a| a.trim().is_empty()) {
            return Err(TrustBootstrapError::ParseBack {
                reason: format!(
                    "issuer #{} ({}) has no non-empty audience",
                    i + 1,
                    issuer.iss
                ),
            });
        }
    }
    Ok(parsed.len())
}

/// `true` when the existing binding file parses to the same TOML value
/// as the freshly-rendered one (comments and formatting ignored) — the
/// idempotence test that keeps reboots write-free.
fn binding_semantically_equal(existing_path: &Path, rendered: &str) -> bool {
    let Ok(existing_text) = std::fs::read_to_string(existing_path) else {
        return false;
    };
    let (Ok(a), Ok(b)) = (
        existing_text.parse::<toml::Value>(),
        rendered.parse::<toml::Value>(),
    ) else {
        return false;
    };
    a == b
}

/// Write `content` to `path` atomically (same-directory temp + rename)
/// so a crash mid-write can never leave a truncated declaration behind.
fn write_atomically(path: &Path, content: &str) -> Result<(), TrustBootstrapError> {
    let io_err = |source: std::io::Error| TrustBootstrapError::Io {
        path: path.to_path_buf(),
        source,
    };
    let parent = path.parent().ok_or_else(|| {
        io_err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no parent directory",
        ))
    })?;
    std::fs::create_dir_all(parent).map_err(io_err)?;
    let tmp = path.with_extension("toml.bootstrap-tmp");
    std::fs::write(&tmp, content).map_err(io_err)?;
    std::fs::rename(&tmp, path).map_err(io_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn issuer(iss: &str, jwks: &str, auds: &[&str]) -> DeclaredIssuer {
        DeclaredIssuer {
            iss: iss.into(),
            jwks_uri: Some(jwks.into()),
            audiences: auds.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn section_with_handoff(dir: &Path) -> TrustBootstrapSection {
        TrustBootstrapSection {
            handoff_dir: Some(dir.to_path_buf()),
            handoff_wait_secs: Some(0),
            issuer: Vec::new(),
        }
    }

    fn read_allowlist(state: &Path) -> String {
        std::fs::read_to_string(state.join("security/trusted-issuers.toml")).unwrap()
    }

    #[test]
    fn test_fresh_volume_env_declaration_writes_allowlist() {
        let td = TempDir::new().unwrap();
        let section = TrustBootstrapSection::default();
        let env = Some(issuer(
            "http://ext/git",
            "http://forgejo:3000/keys",
            &["cid"],
        ));
        let report = converge_with(td.path(), &section, env, false).unwrap();
        assert!(report.wrote_allowlist);
        assert_eq!(report.issuers_total, 1);
        let text = read_allowlist(td.path());
        let parsed: TrustedIssuers = toml::from_str(&text).unwrap();
        assert_eq!(parsed.issuers[0].iss, "http://ext/git");
        assert_eq!(parsed.issuers[0].audiences, vec!["cid".to_owned()]);
    }

    #[test]
    fn test_reboot_is_a_byte_identical_noop() {
        let td = TempDir::new().unwrap();
        let section = TrustBootstrapSection::default();
        let env = Some(issuer(
            "http://ext/git",
            "http://forgejo:3000/keys",
            &["cid"],
        ));
        converge_with(td.path(), &section, env.clone(), false).unwrap();
        let first = read_allowlist(td.path());
        let report = converge_with(td.path(), &section, env, false).unwrap();
        assert!(!report.wrote_allowlist, "reboot must not rewrite");
        assert_eq!(read_allowlist(td.path()), first);
    }

    /// The auth-B1 round-2 blocker: a multi-issuer allowlist enriched on
    /// the volume must survive a re-converge — foreign entries verbatim,
    /// own entry converged on drift.
    #[test]
    fn test_merge_preserves_foreign_issuers_and_converges_own_entry() {
        let td = TempDir::new().unwrap();
        let sec = td.path().join("security");
        std::fs::create_dir_all(&sec).unwrap();
        std::fs::write(
            sec.join("trusted-issuers.toml"),
            "# operator note that must survive\n\
             [[issuer]]\n\
             # second IdP, added via the API\n\
             iss = \"http://second-idp/\"\n\
             jwks_uri = \"http://second-idp/keys\"\n\
             audiences = [\"other-aud\"]\n\
             \n\
             [[issuer]]\n\
             iss = \"http://ext/git\"\n\
             jwks_uri = \"http://forgejo:3000/keys\"\n\
             audiences = [\"STALE-client-id\"]\n",
        )
        .unwrap();
        let env = Some(issuer(
            "http://ext/git",
            "http://forgejo:3000/keys",
            &["fresh-client-id"],
        ));
        let report =
            converge_with(td.path(), &TrustBootstrapSection::default(), env, false).unwrap();
        assert!(report.wrote_allowlist);
        assert_eq!(report.issuers_total, 2);
        assert_eq!(report.foreign_preserved, 1);
        let text = read_allowlist(td.path());
        assert!(text.contains("operator note that must survive"));
        assert!(text.contains("second IdP, added via the API"));
        assert!(text.contains("fresh-client-id"));
        assert!(!text.contains("STALE-client-id"));
        assert_eq!(text.matches("http://ext/git").count(), 1);
    }

    /// The reset gesture: `TRUSTED_FORCE=1` drops foreign entries (cross-
    /// tenant volume reuse).
    #[test]
    fn test_force_rewrites_wholesale() {
        let td = TempDir::new().unwrap();
        let sec = td.path().join("security");
        std::fs::create_dir_all(&sec).unwrap();
        std::fs::write(
            sec.join("trusted-issuers.toml"),
            "[[issuer]]\niss = \"http://prior-tenant/\"\naudiences = [\"leak\"]\n",
        )
        .unwrap();
        let env = Some(issuer(
            "http://ext/git",
            "http://forgejo:3000/keys",
            &["cid"],
        ));
        let report =
            converge_with(td.path(), &TrustBootstrapSection::default(), env, true).unwrap();
        assert!(report.wrote_allowlist);
        assert_eq!(report.issuers_total, 1);
        let text = read_allowlist(td.path());
        assert!(!text.contains("prior-tenant"));
    }

    /// Fail-closed parse-back: a corrupt on-volume file that the merge
    /// cannot repair refuses the boot and leaves the file untouched.
    #[test]
    fn test_corrupt_existing_file_fails_closed_and_is_untouched() {
        let td = TempDir::new().unwrap();
        let sec = td.path().join("security");
        std::fs::create_dir_all(&sec).unwrap();
        let corrupt = "this is not toml at all [[[";
        std::fs::write(sec.join("trusted-issuers.toml"), corrupt).unwrap();
        let env = Some(issuer(
            "http://ext/git",
            "http://forgejo:3000/keys",
            &["cid"],
        ));
        let err = converge_with(td.path(), &TrustBootstrapSection::default(), env, false)
            .expect_err("must refuse");
        assert!(matches!(err, TrustBootstrapError::ParseBack { .. }));
        assert_eq!(read_allowlist(td.path()), corrupt, "file must be untouched");
    }

    /// A degenerate declaration (no audience) is refused before it can
    /// reach the volume.
    #[test]
    fn test_degenerate_declaration_refused() {
        let td = TempDir::new().unwrap();
        let env = Some(DeclaredIssuer {
            iss: "http://ext/git".into(),
            jwks_uri: None,
            audiences: vec![],
        });
        let err = converge_with(td.path(), &TrustBootstrapSection::default(), env, false)
            .expect_err("must refuse");
        assert!(matches!(
            err,
            TrustBootstrapError::DegenerateDeclaration { .. }
        ));
        assert!(!td.path().join("security/trusted-issuers.toml").exists());
    }

    #[test]
    fn test_nothing_declared_is_a_noop_even_with_stale_file() {
        let td = TempDir::new().unwrap();
        let sec = td.path().join("security");
        std::fs::create_dir_all(&sec).unwrap();
        let hand_posed = "[[issuer]]\niss = \"http://hand/\"\naudiences = [\"a\"]\n";
        std::fs::write(sec.join("trusted-issuers.toml"), hand_posed).unwrap();
        let report =
            converge_with(td.path(), &TrustBootstrapSection::default(), None, false).unwrap();
        assert!(!report.wrote_allowlist);
        assert_eq!(read_allowlist(td.path()), hand_posed);
    }

    /// Contract C end-to-end: a handoff file declares the issuer AND the
    /// binding; both files land; a second pass is write-free.
    #[test]
    fn test_handoff_declares_issuer_and_binding_idempotently() {
        let td = TempDir::new().unwrap();
        let handoff_dir = td.path().join("handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(
            handoff_dir.join("forgejo-issuer.toml"),
            "schema = \"cosmon-issuer-handoff/v1\"\n\
             [issuer]\n\
             iss = \"http://ext/git\"\n\
             jwks_uri = \"http://forgejo:3000/login/oauth/keys\"\n\
             audiences = [\"client-id-abc\"]\n\
             [binding]\n\
             noyau = \"tenant-demo-sandbox\"\n\
             nucleon_id = \"cosmon-forgejo\"\n\
             sub = \"3\"\n",
        )
        .unwrap();
        let state = td.path().join("state");
        let section = section_with_handoff(&handoff_dir);

        let report = converge_with(&state, &section, None, false).unwrap();
        assert!(report.wrote_allowlist);
        assert_eq!(report.bindings_written, vec!["cosmon-forgejo".to_owned()]);
        let text = read_allowlist(&state);
        assert!(text.contains("client-id-abc"));
        let binding =
            std::fs::read_to_string(state.join("nucleons/cosmon-forgejo/oidc-identity.toml"))
                .unwrap();
        assert!(binding.contains("sub = \"3\""));
        assert!(binding.contains("cosmon:worker:spawn"), "default scopes");

        let report2 = converge_with(&state, &section, None, false).unwrap();
        assert!(!report2.wrote_allowlist, "second boot: no rewrite");
        assert!(report2.bindings_written.is_empty());
        assert_eq!(report2.bindings_unchanged, 1);
    }

    /// `client_id` rotation (volume reuse): the handoff carries a new
    /// audience → both the allowlist entry and the binding converge.
    #[test]
    fn test_handoff_client_id_rotation_converges_binding() {
        let td = TempDir::new().unwrap();
        let handoff_dir = td.path().join("handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        let write_handoff = |cid: &str| {
            std::fs::write(
                handoff_dir.join("forgejo-issuer.toml"),
                format!(
                    "[issuer]\n\
                     iss = \"http://ext/git\"\n\
                     jwks_uri = \"http://forgejo:3000/login/oauth/keys\"\n\
                     audiences = [\"{cid}\"]\n\
                     [binding]\n\
                     noyau = \"tenant-demo-sandbox\"\n\
                     sub = \"3\"\n"
                ),
            )
            .unwrap();
        };
        let state = td.path().join("state");
        let section = section_with_handoff(&handoff_dir);
        write_handoff("old-cid");
        converge_with(&state, &section, None, false).unwrap();
        write_handoff("new-cid");
        let report = converge_with(&state, &section, None, false).unwrap();
        assert!(report.wrote_allowlist);
        assert_eq!(
            report.bindings_written,
            vec!["tenant-demo-sandbox".to_owned()]
        );
        let binding =
            std::fs::read_to_string(state.join("nucleons/tenant-demo-sandbox/oidc-identity.toml"))
                .unwrap();
        assert!(binding.contains("new-cid"));
        assert!(!binding.contains("old-cid"));
    }

    /// A malformed handoff is a hard, loud refusal — never skipped.
    #[test]
    fn test_malformed_handoff_fails_closed() {
        let td = TempDir::new().unwrap();
        let handoff_dir = td.path().join("handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(handoff_dir.join("bad.toml"), "not toml [[[").unwrap();
        let state = td.path().join("state");
        let err = converge_with(&state, &section_with_handoff(&handoff_dir), None, false)
            .expect_err("must refuse");
        assert!(matches!(err, TrustBootstrapError::InvalidHandoff { .. }));
    }

    #[test]
    fn test_unknown_handoff_schema_refused() {
        let td = TempDir::new().unwrap();
        let handoff_dir = td.path().join("handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(
            handoff_dir.join("h.toml"),
            "schema = \"cosmon-issuer-handoff/v9\"\n[issuer]\niss = \"http://x/\"\naudiences = [\"a\"]\n",
        )
        .unwrap();
        let err = converge_with(
            &td.path().join("state"),
            &section_with_handoff(&handoff_dir),
            None,
            false,
        )
        .expect_err("must refuse");
        assert!(matches!(err, TrustBootstrapError::InvalidHandoff { .. }));
    }

    /// The bounded wait: nothing declared, nothing on the volume, a
    /// 1-second budget and no handoff ever → `HandoffTimeout` (the crash-
    /// loop-until-provisioned regime).
    #[test]
    fn test_handoff_wait_expires_into_refusal() {
        let td = TempDir::new().unwrap();
        let handoff_dir = td.path().join("handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        let section = TrustBootstrapSection {
            handoff_dir: Some(handoff_dir),
            handoff_wait_secs: Some(1),
            issuer: Vec::new(),
        };
        let err = converge_with(&td.path().join("state"), &section, None, false)
            .expect_err("must time out");
        assert!(matches!(err, TrustBootstrapError::HandoffTimeout { .. }));
    }

    /// No wait when the configured handoff dir is not mounted at all —
    /// the standalone `docker run` smoke (no volumes) must boot
    /// immediately, deny-all, exactly as before.
    #[test]
    fn test_no_wait_when_handoff_dir_not_mounted() {
        let td = TempDir::new().unwrap();
        let section = TrustBootstrapSection {
            handoff_dir: Some(td.path().join("never-mounted")),
            handoff_wait_secs: Some(3600),
            issuer: Vec::new(),
        };
        let started = std::time::Instant::now();
        let report = converge_with(&td.path().join("state"), &section, None, false).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2), "must not wait");
        assert!(!report.wrote_allowlist);
    }

    /// No wait when trust already exists on the volume (reboot before
    /// the `IdP` is up must not stall or refuse).
    #[test]
    fn test_no_wait_when_allowlist_already_armed() {
        let td = TempDir::new().unwrap();
        let handoff_dir = td.path().join("handoff-not-yet");
        let state = td.path().join("state");
        let sec = state.join("security");
        std::fs::create_dir_all(&sec).unwrap();
        std::fs::write(
            sec.join("trusted-issuers.toml"),
            "[[issuer]]\niss = \"http://ext/git\"\naudiences = [\"cid\"]\n",
        )
        .unwrap();
        let section = TrustBootstrapSection {
            handoff_dir: Some(handoff_dir),
            handoff_wait_secs: Some(3600),
            issuer: Vec::new(),
        };
        let started = std::time::Instant::now();
        let report = converge_with(&state, &section, None, false).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2), "must not wait");
        assert!(!report.wrote_allowlist);
    }

    /// Static `[[trust_bootstrap.issuer]]` entries declare a complete
    /// multi-issuer allowlist (the retired `TRUSTED_ISSUERS_FILE` path).
    #[test]
    fn test_static_section_declares_multi_issuer() {
        let td = TempDir::new().unwrap();
        let section = TrustBootstrapSection {
            handoff_dir: None,
            handoff_wait_secs: None,
            issuer: vec![
                issuer("http://idp-a/", "http://idp-a/keys", &["aud-a"]),
                issuer("http://idp-b/", "http://idp-b/keys", &["aud-b"]),
            ],
        };
        let report = converge_with(td.path(), &section, None, false).unwrap();
        assert_eq!(report.issuers_total, 2);
        let parsed: TrustedIssuers = toml::from_str(&read_allowlist(td.path())).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    /// Handoff outranks env for the same iss (the `IdP`'s live truth wins
    /// over a stale static declaration).
    #[test]
    fn test_handoff_precedence_over_env_for_same_iss() {
        let td = TempDir::new().unwrap();
        let handoff_dir = td.path().join("handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(
            handoff_dir.join("h.toml"),
            "[issuer]\niss = \"http://ext/git\"\naudiences = [\"live-cid\"]\n",
        )
        .unwrap();
        let env = Some(issuer("http://ext/git", "http://old/keys", &["stale-cid"]));
        let state = td.path().join("state");
        converge_with(&state, &section_with_handoff(&handoff_dir), env, false).unwrap();
        let text = read_allowlist(&state);
        assert!(text.contains("live-cid"));
        assert!(!text.contains("stale-cid"));
    }

    /// An injection-shaped declared value (quote that would break out of
    /// the TOML string) is refused, never escaped-and-hoped.
    #[test]
    fn test_unrenderable_value_refused() {
        let td = TempDir::new().unwrap();
        let env = Some(issuer(
            "http://ext/git\"\n[[issuer]]\niss = \"evil",
            "http://x/keys",
            &["cid"],
        ));
        let err = converge_with(td.path(), &TrustBootstrapSection::default(), env, false)
            .expect_err("must refuse");
        assert!(matches!(err, TrustBootstrapError::UnrenderableValue { .. }));
    }

    #[test]
    fn test_validate_allowlist_rejects_zero_issuers() {
        let err = validate_allowlist("# empty on purpose\n").expect_err("must refuse");
        assert!(matches!(err, TrustBootstrapError::ParseBack { .. }));
    }

    /// The validate-local S2 sequence: force-declare A, merge-enrich
    /// with B, then re-declare A — the re-declaration must be a
    /// write-free no-op on its FIRST pass (rewrite only on drift), even
    /// though the merge that appended B re-shaped the file around A's
    /// block.
    #[test]
    fn test_redeclaration_after_enrichment_is_writefree_first_pass() {
        let td = TempDir::new().unwrap();
        let main = issuer("http://idp-main/", "http://idp-main/keys", &["aud"]);
        let dead = issuer("http://idp-dead/", "http://blackhole/keys", &["aud"]);
        let section = TrustBootstrapSection::default();
        converge_with(td.path(), &section, Some(main.clone()), true).unwrap();
        converge_with(td.path(), &section, Some(dead), false).unwrap();
        let before = read_allowlist(td.path());
        let report = converge_with(td.path(), &section, Some(main), false).unwrap();
        assert!(
            !report.wrote_allowlist,
            "identical re-declaration must not rewrite"
        );
        assert_eq!(report.issuers_total, 2);
        assert_eq!(report.foreign_preserved, 1);
        assert_eq!(read_allowlist(td.path()), before);
    }

    #[test]
    fn test_merge_collapses_duplicate_declared_blocks() {
        let existing = "[[issuer]]\niss = \"http://ext/git\"\naudiences = [\"a\"]\n\
                        [[issuer]]\niss = \"http://ext/git\"\naudiences = [\"b\"]\n";
        let declared = [issuer("http://ext/git", "http://f/keys", &["c"])];
        let merged = merge_allowlist(Some(existing), &declared, false);
        assert_eq!(merged.matches("[[issuer]]").count(), 1);
        assert_eq!(validate_allowlist(&merged).unwrap(), 1);
    }

    // ── two-audience provisioner (task-20260710-6ffc, G5 fan-out) ───────

    /// The load-bearing test of the two-OAuth-app provisioner: a single
    /// handoff that declares BOTH audiences (`aud=A` CLI + `aud=B` MCP)
    /// and BOTH bindings (`[binding]` for A, `[[bindings]]` for B) must
    /// seal each into its own `oidc-identity.toml`, and the two must be
    /// **audience-isolated** — a lookup on A's audience never returns B's
    /// binding and vice versa (the negative case CI's single-audience
    /// shape cannot generate by accident, kahneman-F5).
    #[test]
    fn test_handoff_seals_two_bindings_with_audience_isolation() {
        use crate::nucleon_map::HabilitationMap;
        let td = TempDir::new().unwrap();
        let handoff_dir = td.path().join("handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(
            handoff_dir.join("forgejo-issuer.toml"),
            "schema = \"cosmon-issuer-handoff/v1\"\n\
             [issuer]\n\
             iss = \"http://ext/git\"\n\
             jwks_uri = \"http://forgejo:3000/login/oauth/keys\"\n\
             audiences = [\"cid-a\", \"cid-b\"]\n\
             [binding]\n\
             noyau = \"tenant-demo-sandbox\"\n\
             nucleon_id = \"cosmon-forgejo\"\n\
             sub = \"3\"\n\
             audience = \"cid-a\"\n\
             [[bindings]]\n\
             noyau = \"tenant-demo-sandbox\"\n\
             nucleon_id = \"cosmon-forgejo-mcp\"\n\
             sub = \"3\"\n\
             audience = \"cid-b\"\n",
        )
        .unwrap();
        let state = td.path().join("state");
        let section = section_with_handoff(&handoff_dir);

        let report = converge_with(&state, &section, None, false).unwrap();
        assert!(report.wrote_allowlist);
        // Both bindings sealed — distinct directories, stable filenames.
        assert_eq!(report.bindings_written.len(), 2);
        assert!(report
            .bindings_written
            .contains(&"cosmon-forgejo".to_owned()));
        assert!(report
            .bindings_written
            .contains(&"cosmon-forgejo-mcp".to_owned()));

        // The allowlist is the CLOSED audience allowlist — exactly the two
        // provisioned client_ids, never a wildcard (buterin RS-side guard).
        let text = read_allowlist(&state);
        assert!(text.contains("cid-a"));
        assert!(text.contains("cid-b"));

        // Audience isolation, proved by the loader the adapter uses at boot.
        let map = HabilitationMap::load(&state).unwrap();
        let a = map
            .resolve_for_audience("http://ext/git", "3", "cid-a")
            .expect("aud=A binding resolves");
        let b = map
            .resolve_for_audience("http://ext/git", "3", "cid-b")
            .expect("aud=B binding resolves");
        assert_eq!(a.nucleon_id.as_str(), "cosmon-forgejo");
        assert_eq!(a.audience, "cid-a");
        assert_eq!(b.nucleon_id.as_str(), "cosmon-forgejo-mcp");
        assert_eq!(b.audience, "cid-b");
        // The negative assertion: A's audience never opens B's slot.
        assert_ne!(a.nucleon_id.as_str(), b.nucleon_id.as_str());
        // Both seals intact after the honest write.
        assert!(map.seal_intact_for_audience("http://ext/git", "3", "cid-a"));
        assert!(map.seal_intact_for_audience("http://ext/git", "3", "cid-b"));

        // Idempotent: a reboot with the same handoff rewrites nothing.
        let report2 = converge_with(&state, &section, None, false).unwrap();
        assert!(
            !report2.wrote_allowlist,
            "reboot must not rewrite allowlist"
        );
        assert!(
            report2.bindings_written.is_empty(),
            "reboot: no binding rewrite"
        );
        assert_eq!(report2.bindings_unchanged, 2);
    }

    /// Two bindings that collide on the same effective `nucleon_id` would
    /// silently overwrite each other's `oidc-identity.toml` (only one
    /// audience sealed) — the convergence refuses that boot loudly.
    #[test]
    fn test_two_bindings_same_nucleon_id_refused() {
        let td = TempDir::new().unwrap();
        let handoff_dir = td.path().join("handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(
            handoff_dir.join("forgejo-issuer.toml"),
            "[issuer]\n\
             iss = \"http://ext/git\"\n\
             audiences = [\"cid-a\", \"cid-b\"]\n\
             [binding]\n\
             noyau = \"tenant-demo-sandbox\"\n\
             nucleon_id = \"shared\"\n\
             sub = \"3\"\n\
             audience = \"cid-a\"\n\
             [[bindings]]\n\
             noyau = \"tenant-demo-sandbox\"\n\
             nucleon_id = \"shared\"\n\
             sub = \"3\"\n\
             audience = \"cid-b\"\n",
        )
        .unwrap();
        let state = td.path().join("state");
        let err = converge_with(&state, &section_with_handoff(&handoff_dir), None, false)
            .expect_err("must refuse a colliding nucleon_id");
        assert!(matches!(err, TrustBootstrapError::InvalidHandoff { .. }));
    }

    /// A plural `[[bindings]]` entry that omits `nucleon_id` defaults to
    /// its `noyau` — and if that collides with the legacy binding's
    /// directory the same fail-closed guard fires (the effective key, not
    /// the literal field, is what matters).
    #[test]
    fn test_plural_binding_defaulting_to_noyau_collision_refused() {
        let td = TempDir::new().unwrap();
        let handoff_dir = td.path().join("handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(
            handoff_dir.join("forgejo-issuer.toml"),
            "[issuer]\n\
             iss = \"http://ext/git\"\n\
             audiences = [\"cid-a\", \"cid-b\"]\n\
             [binding]\n\
             noyau = \"tenant-demo-sandbox\"\n\
             sub = \"3\"\n\
             audience = \"cid-a\"\n\
             [[bindings]]\n\
             noyau = \"tenant-demo-sandbox\"\n\
             sub = \"3\"\n\
             audience = \"cid-b\"\n",
        )
        .unwrap();
        let state = td.path().join("state");
        let err = converge_with(&state, &section_with_handoff(&handoff_dir), None, false)
            .expect_err("both default to noyau → collision");
        assert!(matches!(err, TrustBootstrapError::InvalidHandoff { .. }));
    }

    /// F1 (review task-20260710-37f8): two bindings with **distinct**
    /// `nucleon_id` directories but the **same effective
    /// `(iss, sub, audience)` triple** — the canonical trip being both
    /// omitting `audience`, so both fall back to `issuer.audiences[0]`.
    ///
    /// The `nucleon_id` guard alone accepts this (the two directories
    /// differ), each seals its own `oidc-identity.toml`, and then
    /// [`HabilitationMap::load`] collapses them to ONE — last-writer-wins
    /// over an unsorted `read_dir`, non-deterministic across reboots, both
    /// seals green. The convergence must refuse it on the real isolation
    /// key (the triple), which this test pins.
    #[test]
    fn test_two_bindings_same_effective_triple_refused() {
        let td = TempDir::new().unwrap();
        let handoff_dir = td.path().join("handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        // Distinct nucleon_id (passes guard 1); both omit `audience`, so
        // both fall back to issuer.audiences[0] = "cid-a" (trips guard 2).
        std::fs::write(
            handoff_dir.join("forgejo-issuer.toml"),
            "[issuer]\n\
             iss = \"http://ext/git\"\n\
             audiences = [\"cid-a\", \"cid-b\"]\n\
             [binding]\n\
             noyau = \"tenant-demo-sandbox\"\n\
             nucleon_id = \"cosmon-forgejo\"\n\
             sub = \"3\"\n\
             [[bindings]]\n\
             noyau = \"tenant-demo-sandbox\"\n\
             nucleon_id = \"cosmon-forgejo-mcp\"\n\
             sub = \"3\"\n",
        )
        .unwrap();
        let state = td.path().join("state");
        let err = converge_with(&state, &section_with_handoff(&handoff_dir), None, false)
            .expect_err("distinct nucleon_id, same effective triple → collision");
        assert!(matches!(err, TrustBootstrapError::InvalidHandoff { .. }));
        // Nothing was sealed — the refusal fires BEFORE any file is written,
        // so no half-converged, non-deterministic map is left on disk.
        assert!(!state.join("nucleons").join("cosmon-forgejo").exists());
        assert!(!state.join("nucleons").join("cosmon-forgejo-mcp").exists());
    }

    /// The dual of the refusal: two bindings that DIFFER on the audience
    /// (distinct nucleon_id AND distinct effective triple) still converge
    /// cleanly — the triple guard must not over-fire on the legitimate
    /// two-audience fan-out that motivated `[[bindings]]` in the first
    /// place.
    #[test]
    fn test_two_bindings_distinct_triples_converge() {
        let td = TempDir::new().unwrap();
        let handoff_dir = td.path().join("handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(
            handoff_dir.join("forgejo-issuer.toml"),
            "[issuer]\n\
             iss = \"http://ext/git\"\n\
             audiences = [\"cid-a\", \"cid-b\"]\n\
             [binding]\n\
             noyau = \"tenant-demo-sandbox\"\n\
             nucleon_id = \"cosmon-forgejo\"\n\
             sub = \"3\"\n\
             audience = \"cid-a\"\n\
             [[bindings]]\n\
             noyau = \"tenant-demo-sandbox\"\n\
             nucleon_id = \"cosmon-forgejo-mcp\"\n\
             sub = \"3\"\n\
             audience = \"cid-b\"\n",
        )
        .unwrap();
        let state = td.path().join("state");
        let report =
            converge_with(&state, &section_with_handoff(&handoff_dir), None, false).unwrap();
        assert_eq!(report.bindings_written.len(), 2);
    }
}
