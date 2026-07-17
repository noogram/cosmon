// SPDX-License-Identifier: AGPL-3.0-only

//! Mindguard — fail-closed gates inscribed cosmon-ward to refuse claim
//! transitions whose evidence channel has not been touched by an
//! independent witness in the time window that precedes the claim.
//!
//! Born 2026-05-27 after the third Kahneman trap in eight days
//! (sysctl proxy → make-all proxy → git-push proxy) — see
//! the chronicle *« L'auto-pilote qui claim sans regarder »*.
//!
//! # Axiome janis (inscribed cosmon-ward)
//!
//! > Aucune claim d'état — *complete*, *verified*, *deployed*, *prêt* —
//! > n'est valide si l'observable qui la définit n'a pas été touchée,
//! > par un témoin indépendant du canal qui produit la claim, dans la
//! > fenêtre de temps qui précède la claim.
//!
//! # Layout
//!
//! - [`surface_visual`] — the v0 gate: refuses `cs complete <MOL>` when
//!   `<MOL>` touched the visual surface (`*.html`, `*.css`, `*.js`,
//!   `wiki/`, `lumen/web/`) without a sibling `verify-surface` molecule
//!   landing GREEN inside `T_max`.
//!
//! - [`ledger`] — append-only override audit at
//!   `~/.cosmon/audit/mindguard-overrides.jsonl`. Every
//!   `--override-mindguard-down` invocation lands here with its
//!   justification before the override takes effect.
//!
//! - [`config`] — TOML loader for `~/.config/cosmon/mindguard-surface.toml`.
//!   Sensible defaults if the file is absent; the binary ships with
//!   `templates/mindguard-surface.toml`.

pub mod config;
pub mod ledger;
pub mod surface_visual;

use std::fmt;

/// Errors emitted by mindguard gates.
///
/// Two distinct semantics: [`Refused`](MindguardError::Refused) means the
/// gate fired (the evidence was checked and missing); `Unavailable`
/// means the gate machinery itself could not run (state store unreachable,
/// git diff failed, config corrupt). Both are fail-closed by default —
/// only an explicit `--override-mindguard-down` with a justification
/// (logged to the ledger) is allowed to bypass `Unavailable`.
#[derive(Debug)]
pub enum MindguardError {
    /// The gate ran and refused: required evidence is missing.
    ///
    /// The string carries the user-facing reason plus a remediation hint
    /// (typically the `cs nucleate verify-surface …`
    /// command to repair the gap).
    Refused(String),

    /// The gate machinery itself failed (state store, git, config, IO).
    ///
    /// Operator may pass `--override-mindguard-down --justification "…"`
    /// to proceed; the override lands in
    /// `~/.cosmon/audit/mindguard-overrides.jsonl` before the underlying
    /// operation runs.
    Unavailable(String),
}

impl fmt::Display for MindguardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(msg) => write!(f, "mindguard refused: {msg}"),
            Self::Unavailable(msg) => write!(f, "mindguard unavailable: {msg}"),
        }
    }
}

impl std::error::Error for MindguardError {}

impl MindguardError {
    /// Whether `--override-mindguard-down` may bypass this error.
    ///
    /// Only [`Unavailable`](Self::Unavailable) is overridable. A
    /// [`Refused`](Self::Refused) error means the gate fired
    /// intentionally — the remedy is to land the missing evidence
    /// (e.g. run `cs nucleate verify-surface --var
    /// target=<MOL>`), not to disable the gate.
    #[must_use]
    #[allow(dead_code)]
    pub fn is_overridable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}
