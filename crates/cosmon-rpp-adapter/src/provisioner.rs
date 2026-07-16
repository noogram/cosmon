// SPDX-License-Identifier: AGPL-3.0-only

//! Single-writer provisioning service for the `(iss, sub) → noyau`
//! binding store (B2 — impl of B1 design
//! `docs/admin-provisioning-design.md` §4.3).
//!
//! The provisioner is the audited renderer *over HTTP*: it calls the
//! SAME [`build_binding`] / [`render_oidc_identity_toml`] the operator
//! uses by hand (zero schema drift — there is no second writer of the
//! `.toml`), writes the binding atomically into
//! `<state_dir>/nucleons/<habilitation_id>/`, and **reloads the live map
//! in-process** via [`SharedHabilitationMap::store`]. It never widens the
//! deny-by-default binding semantics — it only ADDS admitted
//! `(iss, sub) → noyau` lines.
//!
//! Concurrency: a [`tokio::sync::Mutex`] serialises writes so two
//! POSTs never race on write+reload (design E3). Failure modes are
//! atomic: a binding either lands and resolves, or the file is rolled
//! back and the live map is untouched (design E4 — arc-swap means
//! in-flight requests keep their snapshot).
//!
//! # The reload-without-reboot primitive (B2 headline)
//!
//! Before this path, staging a binding was invisible until the adapter
//! restarted — and a restart tears down every in-flight tmux worker.
//! [`Provisioner::provision`] folds the reload INTO the write
//! (`reloaded: true` in the response), and [`Provisioner::reload`]
//! exposes a standalone re-read for bindings staged by another channel
//! (host-side `.toml` edit). Both publish the new map with a single
//! atomic pointer store — no `SIGHUP`, no reboot, no dropped worker.

use std::path::{Path, PathBuf};

use axum::http::StatusCode;

use crate::error::ApiError;
use crate::image_init::ImageInit;
use crate::nucleon_map::{
    build_binding, render_oidc_identity_toml, HabilitationBindingSpec, HabilitationMap, Noyau,
    RenderError, SharedHabilitationMap,
};
use crate::reload::{self, ReloadOutcome};

/// Outcome of a successful [`Provisioner::provision`] call.
#[derive(Debug)]
pub struct ProvisionOutcome {
    /// `true` ⇒ the `(iss, sub)` binding did not previously resolve
    /// (HTTP `201`); `false` ⇒ it already existed and this call was an
    /// idempotent no-op or an in-place update (HTTP `200`).
    pub created: bool,
    /// `true` ⇒ this call materialised `<galaxies_root>/<noyau>/`.
    pub noyau_created: bool,
    /// Directory name under `nucleons/`.
    pub habilitation_id: String,
    /// Tenant axis the binding is scoped to.
    pub noyau: String,
    /// Absolute path to the written `oidc-identity.toml`.
    pub binding_path: PathBuf,
    /// BLAKE3 hash of the rendered file body (verification, NOT the
    /// admin token).
    pub seal: String,
}

impl ProvisionOutcome {
    /// HTTP status: `201 Created` for a fresh binding, `200 OK` for an
    /// idempotent re-provision.
    #[must_use]
    pub fn status_code(&self) -> StatusCode {
        if self.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        }
    }
}

/// Single writer to the binding store. Cheap to clone-wrap in an `Arc`;
/// the write lock lives inside.
#[derive(Debug)]
pub struct Provisioner {
    state_dir: PathBuf,
    galaxies_root: PathBuf,
    map: SharedHabilitationMap,
    image_init: ImageInit,
    write_lock: tokio::sync::Mutex<()>,
}

impl Provisioner {
    /// Build the provisioner over the live map handle and the boot
    /// `ImageInit` (reused to materialise a freshly-bound noyau's galaxy
    /// tree, exactly as at boot).
    #[must_use]
    pub fn new(
        state_dir: PathBuf,
        galaxies_root: PathBuf,
        map: SharedHabilitationMap,
        image_init: ImageInit,
    ) -> Self {
        Self {
            state_dir,
            galaxies_root,
            map,
            image_init,
            write_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// An inert provisioner whose paths point nowhere and whose map is
    /// empty — for `AppState` constructions in tests and surfaces that
    /// never exercise the admin path. Never call [`Self::provision`] on
    /// it (it would write under `/nonexistent`); it exists only so a
    /// test `AppState` literal stays a one-liner. Not `#[cfg(test)]`
    /// because integration tests compile against the public lib API.
    #[doc(hidden)]
    #[must_use]
    pub fn inert() -> Self {
        let nowhere = PathBuf::from("/nonexistent");
        Self::new(
            nowhere.clone(),
            nowhere.clone(),
            SharedHabilitationMap::new(HabilitationMap::default()),
            ImageInit {
                inbox_root: nowhere.join("inbox"),
                galaxies_root: nowhere.join("galaxies"),
                cs_path: nowhere.join("cs"),
                claude_home: nowhere.join("home"),
                formulas_seed_dir: None,
            },
        )
    }

    /// Borrow the live binding-map handle. The portée tooling
    /// ([`crate::portee::PorteeProvisioner`]) reads it to confirm the N
    /// per-galaxy habilitations it materialised resolve; callers MUST NOT
    /// mutate through it (writes go through [`Self::provision`] /
    /// [`Self::revoke`] so the single-writer discipline holds).
    #[must_use]
    pub fn map(&self) -> &crate::nucleon_map::SharedHabilitationMap {
        &self.map
    }

    /// Re-read every on-disk binding, materialise the delta of new
    /// noyaux, and atomically publish the new map. The standalone
    /// "reload à chaud" trigger behind `POST /v1/admin/reload` — picks
    /// up a binding staged by ANY channel (host-side `.toml` edit, a
    /// prior failed publish) without a reboot. Idempotent: a reload with
    /// no on-disk change is a no-op swap with an empty delta.
    ///
    /// Serialised against [`Self::provision`] so a reload never races a
    /// concurrent write.
    pub async fn reload(&self) -> ReloadOutcome {
        let _guard = self.write_lock.lock().await;
        reload::reload(&self.map, &self.state_dir, &self.image_init)
    }

    /// Provision one habilitation: validate, write the binding
    /// atomically, materialise the noyau if requested, reload in-process,
    /// and verify the binding now resolves before publishing.
    ///
    /// Idempotent by `(iss, sub)`: a re-POST of an identical binding is a
    /// `200` no-op; a re-POST of the same `(iss, sub)` to a different
    /// noyau is a `409` (cross-tenant pivot stays structurally
    /// impossible).
    ///
    /// # Errors
    ///
    /// - `400 malformed_binding` — [`RenderError`] (empty field, non-URL
    ///   issuer, whitespace scope).
    /// - `409 cross_noyau_rebind_refused` — `(iss, sub)` already bound to
    ///   a different noyau.
    /// - `409 habilitation_id_collision` — the target dir already holds a
    ///   binding for a different `(iss, sub)`.
    /// - `500 provision_io_error` — write / mkdir failure.
    /// - `503 reload_failed` — the written binding does not reload
    ///   (rolled back; live map untouched).
    pub async fn provision(
        &self,
        spec: &HabilitationBindingSpec,
        create_noyau: bool,
    ) -> Result<ProvisionOutcome, ApiError> {
        let _guard = self.write_lock.lock().await;

        // 1. Validate + render (REUSE the audited renderer — zero drift).
        let identity = build_binding(spec).map_err(render_to_api)?;
        let toml_body = render_oidc_identity_toml(spec).map_err(render_to_api)?;
        let iss = identity.oidc.issuer.clone();
        let sub = identity.oidc.sub.clone();
        let aud = identity.oidc.audience.clone();
        let noyau = identity.noyau.clone();
        let habilitation_id = identity.nucleon_id.clone();

        // 2. Conflict check against the live map, keyed by the FULL
        //    `(iss, sub, aud)` triple. The audience pins the galaxy
        //    (ADR-0023 D4), so one principal may legitimately hold N
        //    per-galaxy grants (this is what lets the federation tooling
        //    materialise a portée as N habilitations for one foreign
        //    identity). The guard fires only when the *same* audience pin
        //    would be re-pointed at a different noyau — that genuine
        //    cross-tenant rebind stays refused, preserving the
        //    no-cross-tenant-pivot invariant.
        let existed = {
            let live = self.map.load();
            match live.resolve_for_audience(&iss, &sub, &aud) {
                Some(resolved) if resolved.noyau.as_str() != noyau => {
                    return Err(ApiError::with_status(
                        StatusCode::CONFLICT,
                        "cross_noyau_rebind_refused",
                    ));
                }
                Some(_) => true,
                None => false,
            }
        };

        // 3. Resolve target path + habilitation_id collision check.
        let dir = self.state_dir.join("nucleons").join(&habilitation_id);
        let path = dir.join("oidc-identity.toml");
        let prior_bytes = match std::fs::read_to_string(&path) {
            Ok(text) => {
                // The dir may already hold a binding. Refuse to clobber a
                // DIFFERENT (iss, sub) under the same habilitation_id
                // (design E9, conservative default).
                if let Ok(prev) = toml::from_str::<crate::nucleon_map::OidcIdentity>(&text) {
                    if prev.oidc.issuer != iss || prev.oidc.sub != sub {
                        return Err(ApiError::with_status(
                            StatusCode::CONFLICT,
                            "habilitation_id_collision",
                        ));
                    }
                }
                // Identical bytes AND already resolving ⇒ true no-op.
                if text == toml_body && existed {
                    return Ok(ProvisionOutcome {
                        created: false,
                        noyau_created: false,
                        habilitation_id,
                        noyau,
                        binding_path: path,
                        seal: blake3::hash(toml_body.as_bytes()).to_hex().to_string(),
                    });
                }
                Some(text)
            }
            Err(_) => None,
        };

        // 4. Materialise the noyau if requested and absent (mkdir + the
        //    same `cs init` pass as boot). When create_noyau is false we
        //    leave the tree to the operator; the binding still lands.
        let noyau_dir = self.galaxies_root.join(&noyau);
        let noyau_created = if create_noyau && !noyau_dir.exists() {
            self.image_init.run(&[Noyau::new(noyau.clone())]).log();
            true
        } else {
            false
        };

        // 5. Write the binding atomically (tmp + rename in the same dir).
        write_atomic(&dir, &path, &toml_body).map_err(|e| {
            tracing::error!(event = "admin.provision", error = %e, "binding write failed");
            ApiError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "provision_io_error")
        })?;

        // 6. Reload + VERIFY before publishing: load a fresh map, confirm
        //    the new (iss, sub) resolves to the intended noyau, only then
        //    swap it in. On any failure, roll the file back and keep the
        //    live map — no half-written state reaches a reader.
        match HabilitationMap::load(&self.state_dir) {
            Ok(fresh)
                if fresh
                    .resolve_for_audience(&iss, &sub, &aud)
                    .is_some_and(|r| r.noyau.as_str() == noyau) =>
            {
                self.map.store(fresh);
            }
            _ => {
                rollback(&path, prior_bytes.as_deref());
                return Err(ApiError::with_status(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "reload_failed",
                ));
            }
        }

        Ok(ProvisionOutcome {
            created: !existed,
            noyau_created,
            habilitation_id,
            noyau,
            binding_path: path,
            seal: blake3::hash(toml_body.as_bytes()).to_hex().to_string(),
        })
    }

    /// Revoke a provisioned habilitation by directory id: remove the
    /// binding dir, then reload so the `(iss, sub)` no longer resolves.
    /// Symmetric with [`Self::provision`] (design §3.1 — "retirer une
    /// porte").
    ///
    /// # Errors
    ///
    /// - `404 habilitation_not_found` — no such dir under `nucleons/`.
    /// - `500 provision_io_error` — removal failed.
    pub async fn revoke(&self, habilitation_id: &str) -> Result<ReloadOutcome, ApiError> {
        let _guard = self.write_lock.lock().await;
        let dir = self.state_dir.join("nucleons").join(habilitation_id);
        if !dir.is_dir() {
            return Err(ApiError::with_status(
                StatusCode::NOT_FOUND,
                "habilitation_not_found",
            ));
        }
        std::fs::remove_dir_all(&dir).map_err(|e| {
            tracing::error!(event = "admin.revoke", error = %e, "binding dir removal failed");
            ApiError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "provision_io_error")
        })?;
        Ok(reload::reload(&self.map, &self.state_dir, &self.image_init))
    }
}

/// Map a [`RenderError`] onto the stable `400 malformed_binding`. The
/// detailed cause stays in the structured log; the wire body carries
/// only the label (anonymity discipline, mirrors `error.rs`). Taken by
/// value so it drops cleanly into `.map_err(render_to_api)`.
#[allow(clippy::needless_pass_by_value)]
fn render_to_api(e: RenderError) -> ApiError {
    tracing::warn!(event = "admin.provision", error = %e, "rejected malformed binding");
    ApiError::with_status(StatusCode::BAD_REQUEST, "malformed_binding")
}

/// Atomic write: ensure `dir`, write to a sibling temp file, fsync, then
/// rename over `path`. Rename within a directory is atomic on POSIX, so
/// a concurrent loader never observes a half-written `.toml`.
fn write_atomic(dir: &Path, path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("toml.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Undo a binding write: restore the prior bytes if there were any, else
/// remove the file. Best-effort — a rollback failure is logged but does
/// not mask the original error the caller is already returning.
fn rollback(path: &Path, prior: Option<&str>) {
    let result = match prior {
        Some(bytes) => std::fs::write(path, bytes),
        None => std::fs::remove_file(path),
    };
    if let Err(e) = result {
        tracing::error!(
            event = "admin.provision.rollback",
            path = %path.display(),
            error = %e,
            "binding rollback failed — operator should inspect the nucleons dir",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_init_in(td: &Path) -> ImageInit {
        ImageInit {
            inbox_root: td.join("inbox"),
            galaxies_root: td.join("galaxies"),
            cs_path: td.join("nonexistent-cs"),
            claude_home: td.join("home"),
            formulas_seed_dir: None,
        }
    }

    fn provisioner_in(td: &Path) -> Provisioner {
        let map = SharedHabilitationMap::new(HabilitationMap::default());
        Provisioner::new(
            td.to_path_buf(),
            td.join("galaxies"),
            map,
            image_init_in(td),
        )
    }

    fn sample_spec(noyau: &str, sub: &str) -> HabilitationBindingSpec {
        HabilitationBindingSpec {
            noyau: noyau.into(),
            sub: sub.into(),
            issuer: "http://oidc-mock:8444".into(),
            audience: format!("cosmon-rpp-{noyau}"),
            nucleon_id: None,
            phase: None,
            scopes: vec!["cosmon:molecule:read".into()],
            sealed_at: Some("2026-06-16T00:00:00Z".into()),
        }
    }

    #[tokio::test]
    async fn provision_creates_and_resolves_without_restart() {
        let td = tempfile::tempdir().unwrap();
        let p = provisioner_in(td.path());
        let spec = sample_spec("jordan-research", "jordan");

        let out = p.provision(&spec, true).await.unwrap();
        assert!(out.created);
        assert_eq!(out.noyau, "jordan-research");
        assert_eq!(out.status_code(), StatusCode::CREATED);
        // The live map resolves the new binding immediately — no reboot.
        let live = p.map.load();
        assert!(live.resolve("http://oidc-mock:8444", "jordan").is_some());
        // File present on disk.
        assert!(out.binding_path.is_file());
    }

    #[tokio::test]
    async fn reprovision_identical_is_idempotent_200() {
        let td = tempfile::tempdir().unwrap();
        let p = provisioner_in(td.path());
        let spec = sample_spec("jordan-research", "jordan");

        let first = p.provision(&spec, true).await.unwrap();
        assert!(first.created);
        let second = p.provision(&spec, true).await.unwrap();
        assert!(!second.created, "re-POST identical ⇒ 200 no-op");
        assert_eq!(second.status_code(), StatusCode::OK);
        assert_eq!(first.seal, second.seal);
    }

    #[tokio::test]
    async fn rebind_same_audience_to_different_noyau_is_409() {
        // The cross-tenant-pivot guard now fires on the FULL (iss, sub,
        // aud) pin: re-pointing the SAME audience at a different noyau is
        // a genuine rebind and stays refused. `sample_spec` derives the
        // audience from the noyau, so force both specs onto one audience
        // to exercise the guard.
        let td = tempfile::tempdir().unwrap();
        let p = provisioner_in(td.path());
        let mut a = sample_spec("noyau-a", "jordan");
        a.audience = "cosmon-rpp-shared".into();
        p.provision(&a, true).await.unwrap();

        let mut other = sample_spec("noyau-b", "jordan");
        other.audience = "cosmon-rpp-shared".into();
        other.nucleon_id = Some("noyau-b".into());
        let err = p.provision(&other, true).await.unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(err.label, "cross_noyau_rebind_refused");
    }

    #[tokio::test]
    async fn one_identity_two_galaxies_both_provision() {
        // ADR-0023 D4 — capability per galaxy: one foreign identity may
        // hold N per-galaxy grants (distinct audiences), which is exactly
        // what the federation tooling (G5) materialises as a portée. The
        // audience pins the galaxy, so the two bindings coexist and each
        // resolves on its own audience without cross-tenant pivot.
        let td = tempfile::tempdir().unwrap();
        let p = provisioner_in(td.path());

        // Casey federated on `speck`, then on a second galaxy `qcd`.
        let mut speck = sample_spec("speck", "casey");
        speck.nucleon_id = Some("casey-on-speck".into());
        let mut qcd = sample_spec("qcd", "casey");
        qcd.nucleon_id = Some("casey-on-qcd".into());

        assert!(p.provision(&speck, true).await.unwrap().created);
        assert!(p.provision(&qcd, true).await.unwrap().created);

        let live = p.map.load();
        // Same (iss, sub), two galaxies, disambiguated by audience.
        assert_eq!(
            live.resolve_for_audience("http://oidc-mock:8444", "casey", "cosmon-rpp-speck")
                .unwrap()
                .noyau
                .as_str(),
            "speck",
        );
        assert_eq!(
            live.resolve_for_audience("http://oidc-mock:8444", "casey", "cosmon-rpp-qcd")
                .unwrap()
                .noyau
                .as_str(),
            "qcd",
        );
    }

    #[tokio::test]
    async fn habilitation_id_collision_is_409() {
        let td = tempfile::tempdir().unwrap();
        let p = provisioner_in(td.path());
        // First binding under habilitation_id "shared".
        let mut a = sample_spec("shared", "tenant_auditor");
        a.nucleon_id = Some("shared".into());
        p.provision(&a, true).await.unwrap();
        // Second binding, DIFFERENT sub, but same habilitation_id dir.
        let mut b = sample_spec("shared", "bob");
        b.nucleon_id = Some("shared".into());
        let err = p.provision(&b, true).await.unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(err.label, "habilitation_id_collision");
    }

    #[tokio::test]
    async fn malformed_binding_is_400() {
        let td = tempfile::tempdir().unwrap();
        let p = provisioner_in(td.path());
        let mut spec = sample_spec("n", "s");
        spec.issuer = "not-a-url".into();
        let err = p.provision(&spec, true).await.unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert_eq!(err.label, "malformed_binding");
    }

    #[tokio::test]
    async fn revoke_removes_and_404s_when_absent() {
        let td = tempfile::tempdir().unwrap();
        let p = provisioner_in(td.path());
        p.provision(&sample_spec("jordan-research", "jordan"), true)
            .await
            .unwrap();
        assert!(p
            .map
            .load()
            .resolve("http://oidc-mock:8444", "jordan")
            .is_some());

        let out = p.revoke("jordan-research").await.unwrap();
        assert!(out.error.is_none());
        assert!(p
            .map
            .load()
            .resolve("http://oidc-mock:8444", "jordan")
            .is_none());

        let err = p.revoke("does-not-exist").await.unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.label, "habilitation_not_found");
    }

    #[tokio::test]
    async fn standalone_reload_picks_up_host_side_edit() {
        let td = tempfile::tempdir().unwrap();
        let p = provisioner_in(td.path());
        // Operator stages a binding host-side (no API write), then reloads.
        let dir = td.path().join("nucleons").join("staged");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("oidc-identity.toml"),
            "nucleon_id = \"staged\"\nphase = \"Biological\"\nnoyau = \"staged\"\n\n\
             [oidc]\nissuer = \"https://idp\"\nsub = \"host-staged\"\naudience = \"aud\"\n",
        )
        .unwrap();
        assert!(p.map.load().resolve("https://idp", "host-staged").is_none());

        let out = p.reload().await;
        assert!(out.error.is_none());
        assert!(p.map.load().resolve("https://idp", "host-staged").is_some());
    }
}
