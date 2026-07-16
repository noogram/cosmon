// SPDX-License-Identifier: AGPL-3.0-only

//! Portée tooling — the one-gesture federation layer (ADR-0023 G5).
//!
//! A **portée** is the *user-facing* shape of a federation relation:
//! "Dave fédère Casey sur `{speck, qcd}`". ADR-0023 (« Correspondance
//! des niveaux », 2026-06-17) splits this into two layers that this
//! module bridges:
//!
//! - **enforcement** — one `habilitation` per galaxy, a sealed
//!   `(iss, sub, aud) → noyau` capability (ADR-0023 D4: *« un badge, une
//!   galaxie »*). Each is its own [`crate::nucleon_map`] binding, written
//!   and reloaded by the audited [`crate::provisioner::Provisioner`]. The
//!   audience pins the galaxy, so a foreign identity legitimately holds N
//!   per-galaxy grants without any cross-tenant pivot.
//! - **presentation** — the operator manipulates a **portée** (the
//!   relation), never N bindings by hand. One gesture
//!   ([`PorteeProvisioner::federate`]) materialises N habilitations
//!   atomically; one read ([`PorteeProvisioner::list`]) presents them
//!   grouped as a single relation; revocation works per galaxy
//!   ([`PorteeProvisioner::revoke_galaxy`]) or for the whole relation
//!   ([`PorteeProvisioner::dissolve`]).
//!
//! This is **pure tooling over the existing core** (ADR-0023 phasage:
//! *« la vue portée-groupe … pure couche de présentation, pas un
//! changement du cœur »*): it never resolves a token, never decides
//! authz, never adds a trust primitive. It writes the same sealed
//! bindings the operator wrote by hand, plus a grouping manifest under
//! `<state_dir>/portees/<portee_id>/portee.toml`, so the relation can be
//! re-presented and revoked as a unit. There is **no**
//! `enum LocalOrFederated` and **no** `bool external` (ADR-0023 torvalds
//! guard) — a portée is just a named set of ordinary habilitations.

use std::path::PathBuf;

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::nucleon_map::HabilitationBindingSpec;
use crate::provisioner::Provisioner;

/// Default audience prefix. The audience pins the galaxy
/// (`cosmon-rpp-<galaxy>`) — the same convention the admission layer
/// strips in `infer_expected_noyau_from_aud`.
pub const DEFAULT_AUDIENCE_PREFIX: &str = "cosmon-rpp-";

/// The foreign identity a portée federates with. The pair
/// `(issuer, sub)` is the principal; the galaxy is carried separately
/// (the audience), so one partner spans N galaxies.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PartnerIdentity {
    /// JWT `iss` claim — the peer instance's `IdP`, byte-for-byte equal to
    /// its JWKS issuer. Must be an absolute `http(s)://` URL.
    pub issuer: String,
    /// JWT `sub` claim — the partner principal as their `IdP` signs it.
    pub sub: String,
}

/// One materialised galaxy grant inside a portée — the join between the
/// user-facing galaxy name and the enforcement-side habilitation.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PorteeMember {
    /// Galaxy the partner can open (= the `noyau`, today mono-galaxy).
    pub galaxy: String,
    /// Audience that pins this galaxy (`cosmon-rpp-<galaxy>`).
    pub audience: String,
    /// Directory name under `nucleons/` of the backing habilitation.
    pub habilitation_id: String,
}

/// On-disk grouping manifest — the *presentation* record that turns N
/// habilitations back into one relation. It is **not** a root-of-trust:
/// the bindings under `nucleons/` are the sealed truth; this manifest
/// only records which of them belong together so the operator sees one
/// "Casey : {speck, qcd}" instead of two opaque pins. Persisted at
/// `<state_dir>/portees/<portee_id>/portee.toml`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PorteeManifest {
    /// Stable relation id (the directory name under `portees/`).
    pub portee_id: String,
    /// The foreign identity this relation grants to.
    pub partner: PartnerIdentity,
    /// Scopes granted on every galaxy of the relation (shared across the
    /// portée — a portée is a single posture toward one partner).
    #[serde(default)]
    pub scopes: Vec<String>,
    /// The galaxy grants, in stable insertion order.
    #[serde(default)]
    pub members: Vec<PorteeMember>,
    /// ISO-8601 creation timestamp (operator clock; informational).
    #[serde(default)]
    pub created_at: String,
}

/// The operator's one gesture: *« fédère `<partner>` sur `<galaxies>` »*.
#[derive(Clone, Debug)]
pub struct PorteeSpec {
    /// Relation id. Defaults to the sanitised partner `sub` when `None`.
    pub portee_id: Option<String>,
    /// The foreign identity to federate with.
    pub partner: PartnerIdentity,
    /// Galaxies the partner may open. Non-empty; deduplicated on apply.
    pub galaxies: Vec<String>,
    /// Scopes granted on each galaxy. Empty ⇒ JWT-scopes-only admission.
    pub scopes: Vec<String>,
    /// Materialise each galaxy's `<galaxies_root>/<galaxy>/` tree if
    /// absent (mirrors `Provisioner::provision`'s `create_noyau`).
    pub create_noyau: bool,
    /// ISO-8601 timestamp stamped into the sealed bindings + manifest.
    pub created_at: Option<String>,
}

/// Outcome of [`PorteeProvisioner::federate`].
#[derive(Clone, Debug, Serialize)]
pub struct PorteeOutcome {
    /// The relation id (derived or supplied).
    pub portee_id: String,
    /// `true` ⇒ the portée manifest did not previously exist.
    pub created: bool,
    /// The full member set after the gesture (additive union).
    pub members: Vec<PorteeMember>,
    /// Galaxies whose `<galaxies_root>/<galaxy>/` tree this call created.
    pub galaxies_created: Vec<String>,
}

/// Read view of a portée for operator introspection
/// (`GET /v1/admin/federations`). Identical shape to the manifest, kept
/// distinct so the wire view can diverge later without touching the
/// on-disk record.
#[derive(Clone, Debug, Serialize)]
pub struct PorteeView {
    /// Relation id.
    pub portee_id: String,
    /// The foreign identity.
    pub partner: PartnerIdentity,
    /// Shared scopes.
    pub scopes: Vec<String>,
    /// Galaxy grants.
    pub members: Vec<PorteeMember>,
    /// Creation timestamp.
    pub created_at: String,
}

impl From<PorteeManifest> for PorteeView {
    fn from(m: PorteeManifest) -> Self {
        Self {
            portee_id: m.portee_id,
            partner: m.partner,
            scopes: m.scopes,
            members: m.members,
            created_at: m.created_at,
        }
    }
}

/// Derive the audience that pins a galaxy: `cosmon-rpp-<galaxy>`.
#[must_use]
pub fn audience_for_galaxy(galaxy: &str) -> String {
    format!("{DEFAULT_AUDIENCE_PREFIX}{galaxy}")
}

/// Derive the backing habilitation id for one galaxy of a portée:
/// `<portee_id>--<galaxy>`. The double dash keeps the two segments
/// readable and avoids collision with a single-segment local noyau id.
#[must_use]
pub fn habilitation_id_for(portee_id: &str, galaxy: &str) -> String {
    format!("{portee_id}--{galaxy}")
}

/// A token (galaxy / portée id) is well-formed iff it is non-empty after
/// trimming and carries no whitespace or path separator (it becomes a
/// directory name and an audience suffix).
fn validate_token(kind: &'static str, value: &str) -> Result<String, ApiError> {
    let t = value.trim();
    if t.is_empty()
        || t.chars().any(char::is_whitespace)
        || t.contains('/')
        || t.contains('\\')
        || t == "."
        || t == ".."
    {
        return Err(ApiError::with_status(
            StatusCode::BAD_REQUEST,
            "malformed_portee",
        ));
    }
    let _ = kind;
    Ok(t.to_owned())
}

/// The presentation-layer writer over the enforcement-layer
/// [`Provisioner`]. Cheap to wrap in an `Arc`; serialises portée writes
/// against each other (the underlying provisioner serialises the binding
/// writes independently).
#[derive(Debug)]
pub struct PorteeProvisioner {
    state_dir: PathBuf,
    provisioner: std::sync::Arc<Provisioner>,
    write_lock: tokio::sync::Mutex<()>,
}

impl PorteeProvisioner {
    /// Build over the live binding provisioner and the state dir (where
    /// `portees/` and `nucleons/` both live).
    #[must_use]
    pub fn new(state_dir: PathBuf, provisioner: std::sync::Arc<Provisioner>) -> Self {
        Self {
            state_dir,
            provisioner,
            write_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// An inert tooling instance over an inert provisioner — for
    /// `AppState` literals in tests that never exercise the federation
    /// surface. Never call [`Self::federate`] on it (it would write under
    /// `/nonexistent`). Mirrors [`Provisioner::inert`].
    #[doc(hidden)]
    #[must_use]
    pub fn inert() -> Self {
        Self::new(
            PathBuf::from("/nonexistent"),
            std::sync::Arc::new(Provisioner::inert()),
        )
    }

    /// Directory holding a portée manifest.
    fn portee_dir(&self, portee_id: &str) -> PathBuf {
        self.state_dir.join("portees").join(portee_id)
    }

    /// Read a portée manifest, or `None` when absent / unreadable.
    fn read_manifest(&self, portee_id: &str) -> Option<PorteeManifest> {
        let path = self.portee_dir(portee_id).join("portee.toml");
        let text = std::fs::read_to_string(path).ok()?;
        toml::from_str(&text).ok()
    }

    /// **The one gesture.** Materialise (or extend, additively) a portée:
    /// provision one habilitation per galaxy, then write the grouping
    /// manifest. Atomic: if any galaxy fails to provision, every
    /// habilitation provisioned *by this call* is rolled back and no
    /// manifest is written — the relation either lands whole or not at
    /// all.
    ///
    /// Idempotent and additive: re-federating with an overlapping galaxy
    /// set is a per-galaxy no-op for the bindings already present; new
    /// galaxies are added; the manifest becomes the union. Removing a
    /// galaxy is the explicit inverse ([`Self::revoke_galaxy`]).
    ///
    /// # Errors
    ///
    /// - `400 malformed_portee` — empty galaxy set, malformed id/galaxy.
    /// - any [`Provisioner::provision`] error (e.g. `400 malformed_binding`
    ///   for a non-URL issuer) — propagated after rollback.
    pub async fn federate(&self, spec: &PorteeSpec) -> Result<PorteeOutcome, ApiError> {
        let _guard = self.write_lock.lock().await;

        // 1. Resolve + validate the relation id and the galaxy set.
        let portee_id = match &spec.portee_id {
            Some(id) => validate_token("portee_id", id)?,
            None => validate_token("portee_id", &spec.partner.sub)?,
        };
        if spec.galaxies.is_empty() {
            return Err(ApiError::with_status(
                StatusCode::BAD_REQUEST,
                "malformed_portee",
            ));
        }
        // Deduplicate galaxies, preserving first-seen order.
        let mut galaxies: Vec<String> = Vec::new();
        for g in &spec.galaxies {
            let g = validate_token("galaxy", g)?;
            if !galaxies.contains(&g) {
                galaxies.push(g);
            }
        }

        // 2. Load any existing manifest (additive union of members).
        let existing = self.read_manifest(&portee_id);
        let created = existing.is_none();
        let mut members: Vec<PorteeMember> = existing
            .as_ref()
            .map(|m| m.members.clone())
            .unwrap_or_default();

        // 3. Provision one habilitation per galaxy, tracking what THIS
        //    call newly provisioned so we can roll back on failure.
        let mut rollback: Vec<String> = Vec::new();
        let mut galaxies_created: Vec<String> = Vec::new();
        for galaxy in &galaxies {
            let audience = audience_for_galaxy(galaxy);
            let habilitation_id = habilitation_id_for(&portee_id, galaxy);
            let binding = HabilitationBindingSpec {
                noyau: galaxy.clone(),
                sub: spec.partner.sub.clone(),
                issuer: spec.partner.issuer.clone(),
                audience: audience.clone(),
                nucleon_id: Some(habilitation_id.clone()),
                phase: None,
                scopes: spec.scopes.clone(),
                sealed_at: spec.created_at.clone(),
            };
            match self
                .provisioner
                .provision(&binding, spec.create_noyau)
                .await
            {
                Ok(out) => {
                    if out.created {
                        rollback.push(habilitation_id.clone());
                    }
                    if out.noyau_created {
                        galaxies_created.push(galaxy.clone());
                    }
                    if !members.iter().any(|m| m.galaxy == *galaxy) {
                        members.push(PorteeMember {
                            galaxy: galaxy.clone(),
                            audience,
                            habilitation_id,
                        });
                    }
                }
                Err(e) => {
                    // Roll back every habilitation THIS call created, so a
                    // partially-applied gesture never lingers.
                    for hab in rollback.iter().rev() {
                        let _ = self.provisioner.revoke(hab).await;
                    }
                    return Err(e);
                }
            }
        }

        // 4. Write the grouping manifest atomically.
        let manifest = PorteeManifest {
            portee_id: portee_id.clone(),
            partner: spec.partner.clone(),
            scopes: spec.scopes.clone(),
            members: members.clone(),
            created_at: spec
                .created_at
                .clone()
                .or_else(|| existing.as_ref().map(|m| m.created_at.clone()))
                .unwrap_or_default(),
        };
        self.write_manifest(&manifest).map_err(|e| {
            tracing::error!(event = "portee.federate", error = %e, "manifest write failed");
            // The bindings are live and correct; only the grouping record
            // failed. Surface it rather than rolling back working grants.
            ApiError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "portee_manifest_io_error",
            )
        })?;

        Ok(PorteeOutcome {
            portee_id,
            created,
            members,
            galaxies_created,
        })
    }

    /// List every portée (grouped relations) in stable id order.
    #[must_use]
    pub fn list(&self) -> Vec<PorteeView> {
        let root = self.state_dir.join("portees");
        let mut out: Vec<PorteeView> = Vec::new();
        let Ok(entries) = std::fs::read_dir(&root) else {
            return out;
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            if let Some(id) = dir.file_name().and_then(|n| n.to_str()) {
                if let Some(m) = self.read_manifest(id) {
                    out.push(m.into());
                }
            }
        }
        out
    }

    /// Fetch one portée by id.
    #[must_use]
    pub fn get(&self, portee_id: &str) -> Option<PorteeView> {
        self.read_manifest(portee_id).map(PorteeView::from)
    }

    /// **Dissolve** a portée: revoke every backing habilitation, then
    /// remove the manifest. The mirror of [`Self::federate`] — the
    /// relation goes away whole, and the security *increases* as it does
    /// (each removed pin falls back to deny-by-default; ADR-0023 D6).
    ///
    /// # Errors
    ///
    /// - `404 portee_not_found` — no such manifest.
    pub async fn dissolve(&self, portee_id: &str) -> Result<usize, ApiError> {
        let _guard = self.write_lock.lock().await;
        let Some(manifest) = self.read_manifest(portee_id) else {
            return Err(ApiError::with_status(
                StatusCode::NOT_FOUND,
                "portee_not_found",
            ));
        };
        let mut revoked = 0usize;
        for member in &manifest.members {
            // Best-effort: a member already gone (host-side rm) is fine —
            // dissolution is convergent toward deny-by-default.
            if self
                .provisioner
                .revoke(&member.habilitation_id)
                .await
                .is_ok()
            {
                revoked += 1;
            }
        }
        let dir = self.portee_dir(portee_id);
        std::fs::remove_dir_all(&dir).map_err(|e| {
            tracing::error!(event = "portee.dissolve", error = %e, "manifest dir removal failed");
            ApiError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "portee_manifest_io_error",
            )
        })?;
        Ok(revoked)
    }

    /// Revoke **one galaxy** from a portée: revoke that galaxy's
    /// habilitation and drop it from the manifest, leaving the rest of
    /// the relation intact (ADR-0023: *« révocation par galaxie OU de
    /// toute la portée »*). Removing the last galaxy leaves an empty
    /// relation (use [`Self::dissolve`] to remove the relation itself).
    ///
    /// # Errors
    ///
    /// - `404 portee_not_found` — no such manifest.
    /// - `404 galaxy_not_in_portee` — the galaxy is not a member.
    pub async fn revoke_galaxy(
        &self,
        portee_id: &str,
        galaxy: &str,
    ) -> Result<PorteeView, ApiError> {
        let _guard = self.write_lock.lock().await;
        let Some(mut manifest) = self.read_manifest(portee_id) else {
            return Err(ApiError::with_status(
                StatusCode::NOT_FOUND,
                "portee_not_found",
            ));
        };
        let Some(pos) = manifest.members.iter().position(|m| m.galaxy == galaxy) else {
            return Err(ApiError::with_status(
                StatusCode::NOT_FOUND,
                "galaxy_not_in_portee",
            ));
        };
        let member = manifest.members.remove(pos);
        // Best-effort revoke of the binding; the manifest update is the
        // source of truth for the relation's shape.
        let _ = self.provisioner.revoke(&member.habilitation_id).await;
        self.write_manifest(&manifest).map_err(|e| {
            tracing::error!(event = "portee.revoke_galaxy", error = %e, "manifest write failed");
            ApiError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "portee_manifest_io_error",
            )
        })?;
        Ok(manifest.into())
    }

    /// Atomic manifest write (tmp + rename in the same dir).
    fn write_manifest(&self, manifest: &PorteeManifest) -> std::io::Result<()> {
        use std::io::Write as _;
        let dir = self.portee_dir(&manifest.portee_id);
        std::fs::create_dir_all(&dir)?;
        let body = toml::to_string_pretty(manifest)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let path = dir.join("portee.toml");
        let tmp = path.with_extension("toml.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_init::ImageInit;
    use crate::nucleon_map::{HabilitationMap, SharedHabilitationMap};
    use std::path::Path;
    use std::sync::Arc;

    fn provisioner_in(td: &Path) -> Arc<Provisioner> {
        let map = SharedHabilitationMap::new(HabilitationMap::default());
        let image_init = ImageInit {
            inbox_root: td.join("inbox"),
            galaxies_root: td.join("galaxies"),
            cs_path: td.join("nonexistent-cs"),
            claude_home: td.join("home"),
            formulas_seed_dir: None,
        };
        Arc::new(Provisioner::new(
            td.to_path_buf(),
            td.join("galaxies"),
            map,
            image_init,
        ))
    }

    fn spec(portee_id: &str, sub: &str, galaxies: &[&str]) -> PorteeSpec {
        PorteeSpec {
            portee_id: Some(portee_id.to_owned()),
            partner: PartnerIdentity {
                issuer: "https://casey.instance.peer".into(),
                sub: sub.into(),
            },
            galaxies: galaxies.iter().map(|s| (*s).to_owned()).collect(),
            scopes: vec!["cosmon:molecule:read".into()],
            create_noyau: true,
            created_at: Some("2026-06-17T00:00:00Z".into()),
        }
    }

    #[tokio::test]
    async fn one_gesture_materialises_n_habilitations_grouped() {
        let td = tempfile::tempdir().unwrap();
        let prov = provisioner_in(td.path());
        let pp = PorteeProvisioner::new(td.path().to_path_buf(), prov.clone());

        let out = pp
            .federate(&spec("casey", "casey", &["speck", "qcd"]))
            .await
            .unwrap();
        assert!(out.created);
        assert_eq!(out.members.len(), 2, "one gesture → N habilitations");

        // Enforcement: two real bindings exist, each pinned to its galaxy
        // by audience — one foreign identity, two galaxies (ADR-0023 D4).
        let live = prov.map().load();
        assert_eq!(
            live.resolve_for_audience("https://casey.instance.peer", "casey", "cosmon-rpp-speck")
                .unwrap()
                .noyau
                .as_str(),
            "speck"
        );
        assert_eq!(
            live.resolve_for_audience("https://casey.instance.peer", "casey", "cosmon-rpp-qcd")
                .unwrap()
                .noyau
                .as_str(),
            "qcd"
        );

        // Presentation: one relation groups the two grants.
        let views = pp.list();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].portee_id, "casey");
        assert_eq!(views[0].members.len(), 2);
    }

    #[tokio::test]
    async fn federate_is_additive_and_idempotent() {
        let td = tempfile::tempdir().unwrap();
        let prov = provisioner_in(td.path());
        let pp = PorteeProvisioner::new(td.path().to_path_buf(), prov);

        pp.federate(&spec("casey", "casey", &["speck"]))
            .await
            .unwrap();
        // Re-federate with an extra galaxy: additive union, not a reset.
        let out = pp
            .federate(&spec("casey", "casey", &["speck", "qcd"]))
            .await
            .unwrap();
        assert!(!out.created, "existing manifest ⇒ not created");
        assert_eq!(out.members.len(), 2);
        let galaxies: Vec<&str> = out.members.iter().map(|m| m.galaxy.as_str()).collect();
        assert!(galaxies.contains(&"speck") && galaxies.contains(&"qcd"));
    }

    #[tokio::test]
    async fn revoke_one_galaxy_keeps_the_rest() {
        let td = tempfile::tempdir().unwrap();
        let prov = provisioner_in(td.path());
        let pp = PorteeProvisioner::new(td.path().to_path_buf(), prov.clone());
        pp.federate(&spec("casey", "casey", &["speck", "qcd"]))
            .await
            .unwrap();

        let view = pp.revoke_galaxy("casey", "speck").await.unwrap();
        assert_eq!(view.members.len(), 1);
        assert_eq!(view.members[0].galaxy, "qcd");
        // The speck binding is gone; qcd survives.
        let live = prov.map().load();
        assert!(live
            .resolve_for_audience("https://casey.instance.peer", "casey", "cosmon-rpp-speck")
            .is_none());
        assert!(live
            .resolve_for_audience("https://casey.instance.peer", "casey", "cosmon-rpp-qcd")
            .is_some());
    }

    #[tokio::test]
    async fn dissolve_revokes_all_and_removes_manifest() {
        let td = tempfile::tempdir().unwrap();
        let prov = provisioner_in(td.path());
        let pp = PorteeProvisioner::new(td.path().to_path_buf(), prov.clone());
        pp.federate(&spec("casey", "casey", &["speck", "qcd"]))
            .await
            .unwrap();

        let revoked = pp.dissolve("casey").await.unwrap();
        assert_eq!(revoked, 2);
        assert!(pp.get("casey").is_none());
        let live = prov.map().load();
        assert!(live
            .resolve_for_audience("https://casey.instance.peer", "casey", "cosmon-rpp-speck")
            .is_none());
    }

    #[tokio::test]
    async fn empty_galaxy_set_is_400() {
        let td = tempfile::tempdir().unwrap();
        let prov = provisioner_in(td.path());
        let pp = PorteeProvisioner::new(td.path().to_path_buf(), prov);
        let err = pp.federate(&spec("casey", "casey", &[])).await.unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert_eq!(err.label, "malformed_portee");
    }

    #[tokio::test]
    async fn malformed_issuer_rolls_back_partial_gesture() {
        let td = tempfile::tempdir().unwrap();
        let prov = provisioner_in(td.path());
        let pp = PorteeProvisioner::new(td.path().to_path_buf(), prov.clone());
        let mut s = spec("casey", "casey", &["speck", "qcd"]);
        s.partner.issuer = "not-a-url".into();
        let err = pp.federate(&s).await.unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert_eq!(err.label, "malformed_binding");
        // Nothing lingers: no manifest, no bindings.
        assert!(pp.get("casey").is_none());
        assert_eq!(prov.map().load().binding_count(), 0);
    }

    #[tokio::test]
    async fn dissolve_absent_is_404() {
        let td = tempfile::tempdir().unwrap();
        let prov = provisioner_in(td.path());
        let pp = PorteeProvisioner::new(td.path().to_path_buf(), prov);
        let err = pp.dissolve("ghost").await.unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.label, "portee_not_found");
    }
}
