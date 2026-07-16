// SPDX-License-Identifier: AGPL-3.0-only

//! Non-disruptive `HabilitationMap` reload — the "reload" half of the Pierre
//! P2 supported nucleon-creation path.
//!
//! # The problem this closes
//!
//! The adapter loads the sealed `(iss, sub) → nucleon → noyau` bindings
//! exactly once, at boot ([`crate::HabilitationMap::load`]), into an
//! immutable `Arc`. Staging a *new* binding — `cs nucleon bind …` writes
//! a fresh `oidc-identity.toml` under `<state_dir>/nucleons/<id>/` — was
//! therefore invisible until the adapter restarted. And a restart of the
//! container tears down every in-flight tmux worker (the smithy
//! `provision-noyau.sh` restart caveat): the new binding could not be
//! activated without dropping work already in motion.
//!
//! # The primitive
//!
//! This module gives the adapter a **reload that drops no worker**. On
//! `SIGHUP` (the standard "re-read your config" gesture — nginx, sshd,
//! …), the adapter:
//!
//! 1. re-reads `<state_dir>/nucleons/` into a fresh [`HabilitationMap`];
//! 2. computes the **delta** of noyaux that appeared since the live map;
//! 3. runs [`ImageInit::run`] over *only* those new noyaux, materialising
//!    their galaxy tree (`cs init`, `git init`, …) — idempotent and
//!    best-effort, exactly as at boot;
//! 4. **atomically swaps** the live map in
//!    ([`SharedHabilitationMap::store`]).
//!
//! The ordering is load → delta-init → swap on purpose: a freshly bound
//! noyau's `cwd` is materialised *before* its binding becomes
//! resolvable, so no admitted request can resolve to a noyau whose
//! galaxy tree does not yet exist. The swap itself is a single atomic
//! pointer store: in-flight requests keep reading their snapshot, the
//! `axum` server never stops accepting connections, and the tmux workers
//! — separate processes the adapter never owned — are untouched.
//!
//! A reload that fails to read the directory leaves the live map intact
//! (best-effort, never fatal), mirroring the boot-time discipline of
//! [`crate::image_init`].
//!
//! # Why SIGHUP and not an inotify watcher
//!
//! SIGHUP is an explicit operator gesture, which matches cosmon's
//! "propose mechanisms of verification, do not impose them" discipline
//! (`docs/architectural-invariants.md` §8b): the operator stages the
//! binding file, then *decides* to activate it. An always-on inotify
//! watcher would react to every partial write under `nucleons/` and is
//! left as a future extension — [`reload`] is the reusable core it would
//! call, so adding it later is additive.

use std::collections::BTreeSet;
use std::path::Path;

use crate::image_init::{ImageInit, ImageInitReport};
use crate::jwt::{JwksStore, SharedJwksStore};
use crate::nucleon_map::{HabilitationMap, Noyau, SharedHabilitationMap};

/// Outcome of one [`reload`] pass, for structured ops logging and for
/// assertions in tests.
#[derive(Debug)]
pub struct ReloadOutcome {
    /// Binding count of the live map *before* the reload.
    pub bindings_before: usize,
    /// Binding count of the map that was published. Equals
    /// `bindings_before` when the reload failed to load (map unchanged).
    pub bindings_after: usize,
    /// Noyaux present in the new map but absent from the previous one —
    /// the delta that [`ImageInit::run`] was invoked over.
    pub new_noyaux: Vec<Noyau>,
    /// `Some` only when the delta was non-empty (an `image_init` pass
    /// actually ran); `None` when there was nothing new to materialise.
    pub init_report: Option<ImageInitReport>,
    /// `Some` with a reason when the on-disk reload could not be read; in
    /// that case the live map was **not** swapped.
    pub error: Option<String>,
}

impl ReloadOutcome {
    /// `true` when the reload read the directory cleanly (the map was
    /// published) *and* every materialization step of the delta pass
    /// succeeded. A reload with an empty delta and no read error is
    /// `true`.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
            && self
                .init_report
                .as_ref()
                .is_none_or(ImageInitReport::all_ok)
    }

    /// Emit the structured `tracing` surface for this reload.
    pub fn log(&self) {
        if let Some(err) = &self.error {
            tracing::error!(
                event = "reload.nucleons",
                outcome = "failed",
                error = %err,
                "nucleon map reload failed — keeping the live map",
            );
            return;
        }
        tracing::info!(
            event = "reload.nucleons",
            outcome = "done",
            bindings_before = self.bindings_before,
            bindings_after = self.bindings_after,
            new_noyaux = self.new_noyaux.len(),
            "nucleon map reloaded without restart",
        );
        if let Some(report) = &self.init_report {
            report.log();
        }
    }
}

/// Re-read the on-disk nucleon bindings, materialise the delta of new
/// noyaux, and atomically publish the new map into `shared`.
///
/// Best-effort and non-fatal: an unreadable `nucleons/` directory leaves
/// the live map in place and is reported via [`ReloadOutcome::error`].
/// Pure orchestration — the actual materialization authority stays in
/// [`ImageInit`] (which shells out to `cs init` / `git`), exactly as at
/// boot.
#[must_use]
pub fn reload(
    shared: &SharedHabilitationMap,
    state_dir: &Path,
    image_init: &ImageInit,
) -> ReloadOutcome {
    // Snapshot the live map's noyaux so we can diff against the fresh
    // load. Held only long enough to read; the guard is dropped before
    // the swap.
    let before_noyaux: BTreeSet<String> = {
        let live = shared.load();
        live.noyaux()
            .into_iter()
            .map(|n| n.as_str().to_owned())
            .collect()
    };
    let bindings_before = shared.load().binding_count();

    let fresh = match HabilitationMap::load(state_dir) {
        Ok(map) => map,
        Err(e) => {
            return ReloadOutcome {
                bindings_before,
                bindings_after: bindings_before,
                new_noyaux: Vec::new(),
                init_report: None,
                error: Some(format!("read {}/nucleons: {e}", state_dir.display())),
            };
        }
    };

    let bindings_after = fresh.binding_count();
    let new_noyaux: Vec<Noyau> = fresh
        .noyaux()
        .into_iter()
        .filter(|n| !before_noyaux.contains(n.as_str()))
        .collect();

    // Materialise the delta BEFORE publishing the new map, so a binding
    // is never resolvable before its galaxy tree exists.
    let init_report = if new_noyaux.is_empty() {
        None
    } else {
        Some(image_init.run(&new_noyaux))
    };

    shared.store(fresh);

    ReloadOutcome {
        bindings_before,
        bindings_after,
        new_noyaux,
        init_report,
        error: None,
    }
}

/// Outcome of one [`reload_jwks`] pass — the authn-door counterpart of
/// [`ReloadOutcome`]. Carries the per-issuer key counts before and after
/// so a federated-peer onboarding (a new issuer appears) or revocation (an
/// issuer disappears) is visible in the ops log.
#[derive(Debug)]
pub struct JwksReloadOutcome {
    /// Distinct issuer count of the live store *before* the reload.
    pub issuers_before: usize,
    /// Distinct issuer count of the store that was published. Equals
    /// `issuers_before` when the reload failed (store unchanged).
    pub issuers_after: usize,
    /// Total pinned keys after the reload.
    pub keys_after: usize,
    /// `Some` with a reason when the on-disk reload could not be read; in
    /// that case the live store was **not** swapped.
    pub error: Option<String>,
}

impl JwksReloadOutcome {
    /// `true` when the reload read the JWKS dir cleanly and published a
    /// fresh store.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }

    /// Emit the structured `tracing` surface for this reload.
    pub fn log(&self) {
        if let Some(err) = &self.error {
            tracing::error!(
                event = "reload.jwks",
                outcome = "failed",
                error = %err,
                "jwks reload failed — keeping the live store",
            );
            return;
        }
        tracing::info!(
            event = "reload.jwks",
            outcome = "done",
            issuers_before = self.issuers_before,
            issuers_after = self.issuers_after,
            keys_after = self.keys_after,
            "jwks store reloaded without restart",
        );
    }
}

/// Re-read the on-disk JWKS (the **authn door**) and atomically publish
/// the fresh store into `shared`. The symmetric counterpart of [`reload`]
/// (the **authz door**): adding a federated peer issuer's `<iss>.json`
/// under `<state_dir>/security/jwks/` — or removing it to revoke — takes
/// effect on `SIGHUP` with no reboot (ADR-0023 MVP-A, D6).
///
/// Best-effort and non-fatal: an unreadable `security/jwks/` directory
/// leaves the live store in place and is reported via
/// [`JwksReloadOutcome::error`]. Note the asymmetry with [`reload`]: there
/// is no `ImageInit` delta pass — a JWKS only authenticates, it never
/// materialises tenant state (JWKS authn ≠ pin authz, ADR-0023).
#[must_use]
pub fn reload_jwks(shared: &SharedJwksStore, state_dir: &Path) -> JwksReloadOutcome {
    let issuers_before = shared.load().key_counts_by_issuer().len();

    let fresh = match JwksStore::load(state_dir) {
        Ok(store) => store,
        Err(e) => {
            return JwksReloadOutcome {
                issuers_before,
                issuers_after: issuers_before,
                keys_after: 0,
                error: Some(format!("read {}/security/jwks: {e}", state_dir.display())),
            };
        }
    };

    let counts = fresh.key_counts_by_issuer();
    let issuers_after = counts.len();
    let keys_after = counts.iter().map(|(_, n)| *n).sum();

    shared.store(fresh);

    JwksReloadOutcome {
        issuers_before,
        issuers_after,
        keys_after,
        error: None,
    }
}

/// Long-lived `SIGHUP` listener: on every hangup, re-read BOTH the binding
/// map (authz, [`reload`]) AND the JWKS store (authn, [`reload_jwks`]) and
/// log each outcome. Intended to be `tokio::spawn`ed at boot with clones
/// of the [`SharedHabilitationMap`] and [`SharedJwksStore`] handles (the
/// originals move into [`crate::AppState`]).
///
/// One signal refreshes both doors so onboarding or revoking a federated
/// peer — which is a JWKS line (authn) *and* a pin (authz) — is a single
/// `kill -HUP <pid>` (ADR-0023 MVP-A).
///
/// The loop returns only if the signal stream cannot be installed —
/// in which case runtime reload is unavailable and the operator falls
/// back to a restart, logged loudly so the degradation is visible.
#[cfg(unix)]
pub async fn sighup_reload_listener(
    shared: SharedHabilitationMap,
    jwks: SharedJwksStore,
    state_dir: std::path::PathBuf,
    image_init: ImageInit,
) {
    use tokio::signal::unix::{signal, SignalKind};

    let mut hup = match signal(SignalKind::hangup()) {
        Ok(stream) => stream,
        Err(e) => {
            tracing::error!(
                event = "reload.sighup",
                error = %e,
                "failed to install SIGHUP handler — runtime reload disabled, \
                 staging a new binding or JWKS will require a restart",
            );
            return;
        }
    };
    tracing::info!(
        event = "reload.sighup",
        "SIGHUP reload armed — `kill -HUP <pid>` stages new bindings AND JWKS without a restart",
    );
    loop {
        hup.recv().await;
        tracing::info!(
            event = "reload.sighup",
            "SIGHUP received — reloading nucleon bindings and JWKS",
        );
        reload(&shared, &state_dir, &image_init).log();
        reload_jwks(&jwks, &state_dir).log();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write an `oidc-identity.toml` for `(nucleon_id, noyau, iss, sub)`
    /// under `<state_dir>/nucleons/<nucleon_id>/`.
    fn write_binding(state_dir: &Path, nucleon_id: &str, noyau: &str, iss: &str, sub: &str) {
        let dir = state_dir.join("nucleons").join(nucleon_id);
        std::fs::create_dir_all(&dir).unwrap();
        let body = format!(
            "nucleon_id = \"{nucleon_id}\"\n\
             phase = \"Biological\"\n\
             noyau = \"{noyau}\"\n\n\
             [oidc]\n\
             issuer = \"{iss}\"\n\
             sub = \"{sub}\"\n\
             audience = \"cosmon-rpp-{noyau}\"\n",
        );
        std::fs::write(dir.join("oidc-identity.toml"), body).unwrap();
    }

    /// An `ImageInit` pointed entirely inside `td` so the delta pass
    /// scribbles only in the temp dir. `cs` resolves to a non-existent
    /// path: the per-noyau `cs init` step then `Failed`s gracefully
    /// (best-effort), which is fine — the test asserts the *swap* and the
    /// *delta computation*, not the shell-out success.
    fn image_init_in(td: &Path) -> ImageInit {
        ImageInit {
            inbox_root: td.join("inbox"),
            galaxies_root: td.join("galaxies"),
            cs_path: td.join("nonexistent-cs"),
            claude_home: td.join("home"),
            formulas_seed_dir: None,
        }
    }

    #[test]
    fn reload_picks_up_a_newly_staged_binding() {
        let td = tempfile::tempdir().unwrap();
        let state_dir = td.path().to_path_buf();
        write_binding(&state_dir, "nuc-a", "tenant-demo", "https://idp", "sub-a");

        let shared = SharedHabilitationMap::new(HabilitationMap::load(&state_dir).unwrap());
        assert_eq!(shared.load().binding_count(), 1);
        assert!(shared.load().resolve("https://idp", "sub-b").is_none());

        // Operator stages a second binding in a new noyau, then SIGHUPs.
        write_binding(&state_dir, "nuc-b", "democorp", "https://idp", "sub-b");
        let outcome = reload(&shared, &state_dir, &image_init_in(td.path()));

        assert!(outcome.error.is_none());
        assert_eq!(outcome.bindings_before, 1);
        assert_eq!(outcome.bindings_after, 2);
        // Only the genuinely-new noyau is in the delta.
        assert_eq!(outcome.new_noyaux.len(), 1);
        assert_eq!(outcome.new_noyaux[0].as_str(), "democorp");
        // The live map now resolves the new binding without a restart.
        assert!(shared.load().resolve("https://idp", "sub-b").is_some());
        assert_eq!(shared.load().binding_count(), 2);
    }

    #[test]
    fn reload_with_no_change_is_a_noop_swap_with_empty_delta() {
        let td = tempfile::tempdir().unwrap();
        let state_dir = td.path().to_path_buf();
        write_binding(&state_dir, "nuc-a", "tenant-demo", "https://idp", "sub-a");

        let shared = SharedHabilitationMap::new(HabilitationMap::load(&state_dir).unwrap());
        let outcome = reload(&shared, &state_dir, &image_init_in(td.path()));

        assert!(outcome.error.is_none());
        assert!(outcome.new_noyaux.is_empty());
        assert!(
            outcome.init_report.is_none(),
            "no delta → no image_init pass"
        );
        assert_eq!(outcome.bindings_after, 1);
        assert!(shared.load().resolve("https://idp", "sub-a").is_some());
    }

    #[test]
    fn reload_adds_binding_to_existing_noyau_without_remateralizing() {
        // A second binding lands in the SAME noyau: the binding count
        // grows but the noyau delta is empty (it was already
        // materialised), so no image_init pass runs.
        let td = tempfile::tempdir().unwrap();
        let state_dir = td.path().to_path_buf();
        write_binding(&state_dir, "nuc-a", "tenant-demo", "https://idp", "sub-a");
        let shared = SharedHabilitationMap::new(HabilitationMap::load(&state_dir).unwrap());

        write_binding(&state_dir, "nuc-a2", "tenant-demo", "https://idp", "sub-a2");
        let outcome = reload(&shared, &state_dir, &image_init_in(td.path()));

        assert!(outcome.error.is_none());
        assert_eq!(outcome.bindings_after, 2);
        assert!(
            outcome.new_noyaux.is_empty(),
            "same noyau → no re-materialization"
        );
        assert!(outcome.init_report.is_none());
        assert!(shared.load().resolve("https://idp", "sub-a2").is_some());
    }

    #[test]
    fn reload_failure_keeps_the_live_map() {
        // Point at a state_dir whose `nucleons/` cannot be enumerated
        // (it is a file, not a directory) → read error, live map intact.
        let td = tempfile::tempdir().unwrap();
        let good = td.path().join("good");
        write_binding(&good, "nuc-a", "tenant-demo", "https://idp", "sub-a");
        let shared = SharedHabilitationMap::new(HabilitationMap::load(&good).unwrap());
        assert_eq!(shared.load().binding_count(), 1);

        let bad = td.path().join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        // Create `nucleons` as a *file* so read_dir on it fails.
        std::fs::write(bad.join("nucleons"), "not a dir").unwrap();

        let outcome = reload(&shared, &bad, &image_init_in(td.path()));
        assert!(outcome.error.is_some(), "read error surfaced");
        // The live map is untouched.
        assert_eq!(shared.load().binding_count(), 1);
        assert!(shared.load().resolve("https://idp", "sub-a").is_some());
    }

    // ── JWKS authn-door reload (ADR-0023 MVP-A) ────────────────────────

    #[test]
    fn reload_jwks_missing_dir_is_ok_and_empty() {
        // No `security/jwks/` yet → a clean, empty store (deny-all authn),
        // never an error. Symmetric with the binding map's empty load.
        let td = tempfile::tempdir().unwrap();
        let shared = SharedJwksStore::new(JwksStore::default());
        let outcome = reload_jwks(&shared, td.path());
        assert!(outcome.is_ok());
        assert_eq!(outcome.issuers_after, 0);
        assert_eq!(outcome.keys_after, 0);
    }

    #[test]
    fn reload_jwks_read_error_keeps_live_store() {
        // `security/jwks` is a FILE, not a dir → enumeration fails; the
        // live store must be left intact (best-effort, never fatal).
        let td = tempfile::tempdir().unwrap();
        let sec = td.path().join("security");
        std::fs::create_dir_all(&sec).unwrap();
        std::fs::write(sec.join("jwks"), "not a dir").unwrap();

        let shared = SharedJwksStore::new(JwksStore::default());
        let outcome = reload_jwks(&shared, td.path());
        assert!(outcome.error.is_some(), "read error surfaced");
        assert!(!outcome.is_ok());
    }

    #[test]
    fn outcome_is_ok_reflects_error_and_delta() {
        let ok = ReloadOutcome {
            bindings_before: 1,
            bindings_after: 1,
            new_noyaux: Vec::new(),
            init_report: None,
            error: None,
        };
        assert!(ok.is_ok());

        let failed = ReloadOutcome {
            bindings_before: 1,
            bindings_after: 1,
            new_noyaux: Vec::new(),
            init_report: None,
            error: Some("boom".to_owned()),
        };
        assert!(!failed.is_ok());
    }
}
