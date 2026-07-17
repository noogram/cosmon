// SPDX-License-Identifier: AGPL-3.0-only

//! Sealed `sub → nucleon_id → noyau` mapping — clause (a) of the §8j
//! HTTPS+JWT instantiation (ADR-080 §3.1).
//!
//! Each `oidc-identity.toml` file under
//! `<state_dir>/nucleons/<nucleon_id>/` is BLAKE3-sealed at load. The
//! recorded seal is compared on every `resolve()` call so retroactive
//! edits are detected via [`crate::RppRejectReason::SealBroken`].
//!
//! The mapping is read-only here; provisioning is an explicit
//! operator gesture (`cs nucleon bind ...`, out of crate).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::{ArcSwap, Guard};
use serde::{Deserialize, Serialize};

/// A `noyau` is the multi-tenant axis (organisation / community) per
/// ADR-063. The `noyau` value also determines the subprocess `cwd`:
/// `/srv/cosmon/<noyau>/`.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
pub struct Noyau(pub String);

impl Noyau {
    /// Construct from any string-like value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the noyau as a `&str` for path / log composition.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Noyau {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A nucleon identifier (mirrors the cosmon-matrix-tick newtype but
/// kept local so the RPP crate has no transitive dependency on the
/// Matrix bridge).
#[derive(Clone, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
pub struct HabilitationId(pub String);

impl HabilitationId {
    /// Construct from any string-like value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the id as a `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for HabilitationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// On-disk shape of an `oidc-identity.toml` file. Multiple files may
/// share a `nucleon_id` (one per Orbitale, ADR-063); each file binds
/// exactly one `(iss, sub, audience)` triple.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OidcIdentity {
    /// Resolved `nucleon_id` (the directory name).
    pub nucleon_id: String,
    /// Cognitive-substrate label per ADR-063
    /// (`Biological` | `LlmFrontier`).
    pub phase: String,
    /// Tenant axis — the `noyau` this `sub` is scoped to (cross-tenant
    /// pivot is rejected).
    pub noyau: String,
    /// `IdP` / `IdP`-instance details.
    pub oidc: OidcClaims,
    /// Optional binding-granted scopes (T23).
    /// The upstream `IdP` (Forgejo `OAuth2`) cannot mint custom scopes
    /// like `cosmon:molecule:*`; the binding closes that gap by
    /// granting the scopes implicitly for the bound `(iss, sub)`.
    /// Absent on legacy bindings — admission falls back to the JWT's
    /// scopes alone (backwards-compat).
    #[serde(default)]
    pub scopes: Option<ScopesGrant>,
    /// Optional `[drain_bounds]` overrides (B1/B2/B3 moussage bounds).
    /// Absent → server defaults apply; a tenant drain is never
    /// unbounded.
    #[serde(default)]
    pub drain_bounds: Option<DrainBoundsSpec>,
}

/// Binding-granted scope envelope (T23).
///
/// Carried by `oidc-identity.toml` under the optional `[scopes]`
/// section:
///
/// ```toml
/// [scopes]
/// allowed = ["cosmon:molecule:read", "cosmon:molecule:write"]
/// ```
///
/// The list extends what the JWT itself carries — an admin nucleon
/// can be granted `cosmon:molecule:write` even when the upstream
/// `IdP` only issues `openid`. The grant is per-`(iss, sub)` and
/// per-`noyau`: cross-noyau pivot is still rejected by the audience
/// pin (`CrossTenantPivot`), so binding-granted scopes never widen
/// the tenant isolation invariant (ADR-080 §8j).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ScopesGrant {
    /// Scopes implicitly granted to this `(iss, sub)` pair, on top of
    /// whatever scopes the JWT carries.
    #[serde(default)]
    pub allowed: Vec<String>,
}

/// Server-side drain bounds carried by the binding (B1 moussage
/// resident). The bound must live in a system strictly stronger than
/// the client.
///
/// Optional `[drain_bounds]` section of `oidc-identity.toml`:
///
/// ```toml
/// [drain_bounds]
/// budget = 64          # B3 — max runtime actions per drain (decreasing)
/// max_depth = 6        # B1 — max DAG depth (longest chain, molecules)
/// max_molecules = 128  # B2 — max molecules in the fleet while draining
/// ```
///
/// Every field is optional; an absent field (or an absent section)
/// falls back to the server defaults below. The binding is
/// operator-written and BLAKE3-sealed — the client can *read* the
/// effective bounds (`GET /v1/quota`) but can never write them; the
/// §8p surface carries no route that touches this file. Extension of
/// the §8j(c) leaky-bucket placement: same boot-time read, same
/// inviolability, one more dimension (the drain budget).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct DrainBoundsSpec {
    /// B3 override — max runtime actions per drain.
    #[serde(default)]
    pub budget: Option<u64>,
    /// B1 override — max DAG depth.
    #[serde(default)]
    pub max_depth: Option<u32>,
    /// B2 override — max molecules.
    #[serde(default)]
    pub max_molecules: Option<u64>,
}

/// B3 default — max runtime actions per tenant drain. Conservative:
/// enough for a root + a few dozen children with retries, far below
/// anything that looks like unbounded moussage.
pub const DEFAULT_DRAIN_BUDGET: u64 = 128;
/// B1 default — max DAG depth for a tenant drain.
pub const DEFAULT_DRAIN_MAX_DEPTH: u32 = 8;
/// B2 default — max molecules in a tenant fleet while draining.
pub const DEFAULT_DRAIN_MAX_MOLECULES: u64 = 256;

/// Effective, always-concrete drain bounds for one binding. Unlike
/// [`DrainBoundsSpec`] (the on-disk override shape) every field here
/// is resolved: a tenant drain is NEVER unbounded — totality (B3) is
/// not opt-in (godel Q3: B3 is *obligatoire*).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrainBounds {
    /// B3 — max runtime actions per drain (the decreasing budget).
    pub budget: u64,
    /// B1 — max DAG depth.
    pub max_depth: u32,
    /// B2 — max molecules.
    pub max_molecules: u64,
}

impl Default for DrainBounds {
    fn default() -> Self {
        Self {
            budget: DEFAULT_DRAIN_BUDGET,
            max_depth: DEFAULT_DRAIN_MAX_DEPTH,
            max_molecules: DEFAULT_DRAIN_MAX_MOLECULES,
        }
    }
}

impl DrainBounds {
    /// Resolve the effective bounds from an optional binding override.
    #[must_use]
    pub fn from_spec(spec: Option<&DrainBoundsSpec>) -> Self {
        let d = Self::default();
        match spec {
            None => d,
            Some(s) => Self {
                budget: s.budget.unwrap_or(d.budget),
                max_depth: s.max_depth.unwrap_or(d.max_depth),
                max_molecules: s.max_molecules.unwrap_or(d.max_molecules),
            },
        }
    }
}

/// Per-`IdP` claim envelope binding a JWT `(iss, sub, aud)` triple to a
/// nucleon. Clause (a) consults this on every request.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OidcClaims {
    /// JWT `iss` claim (e.g. `"https://accounts.google.com"`).
    pub issuer: String,
    /// JWT `sub` claim — the principal identifier as the `IdP` signs it.
    pub sub: String,
    /// JWT `aud` claim — pinned to this RPP instance.
    pub audience: String,
    /// ISO-8601 timestamp of provisioning. Informational; not consulted.
    #[serde(default)]
    pub sealed_at: String,
}

/// In-memory view of the nucleon map, keyed by `(iss, sub, aud)`.
///
/// The third key dimension — `aud` — is the per-galaxy capability pin
/// (ADR-0023 D4: *« habilitation = 1 galaxie ; un badge, une galaxie »*).
/// A single principal `(iss, sub)` may hold **N** bindings, one per
/// audience (= one per galaxy); each is its own habilitation directory.
/// This is what lets the federation tooling materialise a portée as N
/// per-galaxy grants for ONE foreign identity (G5) without any
/// `LocalOrFederated` branching: the audience was already a pinned claim,
/// it is simply promoted into the key. A local single-galaxy tenant is
/// unchanged — it has exactly one `(iss, sub, aud)` row.
///
/// The no-cross-tenant-pivot invariant is preserved structurally: a token
/// carrying audience A can only ever resolve the binding keyed on A, so it
/// can never reach galaxy B's noyau (whose binding is keyed on B's
/// audience). Built by [`HabilitationMap::load`] from an on-disk directory
/// tree, or via the [`HabilitationMapBuilder`] in tests.
#[derive(Clone, Debug, Default)]
pub struct HabilitationMap {
    by_key: BTreeMap<(String, String, String), Resolved>,
    seals: BTreeMap<(String, String, String), String>,
    live_seals: BTreeMap<(String, String, String), String>,
}

/// Secret-free projection of one loaded binding, for operator
/// introspection (`GET /v1/admin/habilitations`).
/// Carries the binding envelope the operator wrote — NEVER the BLAKE3
/// seal, NEVER any admin credential.
#[derive(Clone, Debug, Serialize)]
pub struct BindingSummary {
    /// JWT `iss` claim the binding admits.
    pub issuer: String,
    /// JWT `sub` claim the binding admits.
    pub sub: String,
    /// Directory name under `nucleons/` (the `habilitation_id`).
    pub habilitation_id: String,
    /// Tenant axis this `(iss, sub)` is scoped to.
    pub noyau: String,
    /// Pinned audience.
    pub audience: String,
    /// Binding-granted scopes (T23), verbatim on-disk order.
    pub allowed_scopes: Vec<String>,
}

/// Resolved binding for one `(iss, sub)` pair.
#[derive(Clone, Debug)]
pub struct Resolved {
    /// The bound nucleon.
    pub nucleon_id: HabilitationId,
    /// Tenant scope — used to detect cross-tenant pivot.
    pub noyau: Noyau,
    /// Pinned audience (must match the JWT's `aud`).
    pub audience: String,
    /// Scopes the binding grants implicitly (T23). Stable order: the
    /// list mirrors the on-disk `[scopes].allowed` array verbatim.
    /// Empty when the binding does not carry a `[scopes]` section —
    /// legacy bindings fall through to the JWT scope set alone.
    pub allowed_scopes: Vec<String>,
    /// Effective drain bounds (B1/B2/B3) — binding overrides resolved
    /// against the server defaults. Always concrete: a tenant drain is
    /// never unbounded.
    pub drain_bounds: DrainBounds,
    /// Federation provenance — `Some` only when this binding was
    /// admitted via a verified detached [`crate::scope_badge::ScopeBadge`]
    /// (ADR-0023 MVP-B) rather than a local sealed pin; `None` for every
    /// local binding loaded from disk.
    ///
    /// This is the **entire** structural footprint of the federated case
    /// in the resolution core (ADR-0023 §MVP garde-fou): an additive
    /// `Option<…>`. There is deliberately no `enum LocalOrFederated` and
    /// no `bool external` — authorization stays host-side and
    /// deny-by-default whether the identity is local or foreign.
    pub federated: Option<crate::scope_badge::FederatedProvenance>,
}

impl HabilitationMap {
    /// Resolve a principal `(iss, sub)` to *a* sealed binding, ignoring
    /// audience. Returns the lexicographically-first binding (by audience)
    /// when the principal holds several per-galaxy grants; `None` for an
    /// unknown principal.
    ///
    /// This is the *principal-level* lookup: it answers "does this
    /// identity have any grant at all?" — used by informational callers
    /// (`/v1/quota`, `/v1/auth/me`) and to distinguish
    /// [`crate::RppRejectReason::UnknownSub`] (no grant) from
    /// [`crate::RppRejectReason::CrossTenantPivot`] (a grant exists, but
    /// not for the presented audience). For the admission hot path use
    /// [`Self::resolve_for_audience`], which pins the galaxy.
    #[must_use]
    pub fn resolve(&self, iss: &str, sub: &str) -> Option<&Resolved> {
        self.by_key
            .range((iss.to_owned(), sub.to_owned(), String::new())..)
            .find(|((i, s, _), _)| i == iss && s == sub)
            .map(|(_, r)| r)
    }

    /// Resolve a full `(iss, sub, aud)` triple to its sealed binding —
    /// the per-galaxy capability lookup. The audience pins exactly one
    /// galaxy (ADR-0023 D4), so this is the authoritative admission-path
    /// resolver: a token can only ever open the galaxy whose audience it
    /// carries. Returns `None` when no binding pins this exact triple.
    #[must_use]
    pub fn resolve_for_audience(&self, iss: &str, sub: &str, aud: &str) -> Option<&Resolved> {
        self.by_key
            .get(&(iss.to_owned(), sub.to_owned(), aud.to_owned()))
    }

    /// Total number of `(iss, sub) → noyau` bindings loaded. Used by
    /// `cosmon-rpp-adapter` to surface the binding count at boot
    /// (silent-empty-HabilitationMap bugs are otherwise invisible until
    /// the first request rejects with `unknown_sub`).
    #[must_use]
    pub fn binding_count(&self) -> usize {
        self.by_key.len()
    }

    /// Distinct `noyau` values across every binding, in stable
    /// first-occurrence order (the `(iss, sub)` map is a `BTreeMap`, so
    /// the order is deterministic across boots).
    ///
    /// This is the source of truth for the boot-time per-noyau state
    /// materialization ([`crate::image_init::ImageInit::run`]): the
    /// binding layer is already plural, so the set of noyaux to
    /// materialise is exactly the set of noyaux bound here. N nucléons
    /// may share a noyau — they collapse to one materialised galaxy
    /// tree (`/srv/cosmon/<noyau>/`).
    #[must_use]
    pub fn noyaux(&self) -> Vec<Noyau> {
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for resolved in self.by_key.values() {
            if seen.insert(resolved.noyau.as_str().to_owned()) {
                out.push(resolved.noyau.clone());
            }
        }
        out
    }

    /// Distinct `(noyau, binding_count)` pairs for a given `sub`, in
    /// stable first-occurrence order. Powers `GET /v1/noyaux` — a
    /// discovery endpoint that lets a multi-noyau operator enumerate the
    /// tenants their `sub` is bound to without first guessing a `noyau`
    /// slug.
    ///
    /// The filter matches on the `sub` value alone (across every pinned
    /// issuer) since a multi-IdP operator may legitimately appear under
    /// distinct `(iss, sub)` keys that all collapse to the same human
    /// principal. `binding_count` is the number of `(iss, sub) → noyau`
    /// rows backing the noyau for this sub.
    #[must_use]
    pub fn noyaux_for_sub(&self, sub: &str) -> Vec<(Noyau, usize)> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        for ((_, s, _), resolved) in &self.by_key {
            if s != sub {
                continue;
            }
            let key = resolved.noyau.as_str().to_owned();
            let entry = counts.entry(key.clone()).or_insert(0);
            if *entry == 0 {
                order.push(key);
            }
            *entry += 1;
        }
        order
            .into_iter()
            .map(|n| {
                let count = counts.get(&n).copied().unwrap_or(0);
                (Noyau::new(n), count)
            })
            .collect()
    }

    /// Scopes granted by the `(iss, sub)` binding, or an empty slice
    /// when the binding has no `[scopes]` section (or is absent
    /// entirely). The accessor is read-only and side-effect-free; it
    /// is consulted by the route handlers *after* admission so the
    /// scope check can union JWT scopes with binding-granted scopes
    /// before deciding Allow/Absent (T23).
    #[must_use]
    pub fn allowed_scopes_for(&self, iss: &str, sub: &str) -> &[String] {
        self.resolve(iss, sub)
            .map_or(&[], |r| r.allowed_scopes.as_slice())
    }

    /// Binding-granted scopes for an exact `(iss, sub, aud)` triple — the
    /// per-galaxy scope grant. Use this when the caller knows the
    /// presented audience (the admission-resolved galaxy) so that a
    /// principal federated on two galaxies with different scope sets gets
    /// the right slice. Falls back to an empty slice for an unknown
    /// triple (deny-by-default). Mirror of [`Self::resolve_for_audience`]
    /// for the scope dimension.
    #[must_use]
    pub fn allowed_scopes_for_audience(&self, iss: &str, sub: &str, aud: &str) -> &[String] {
        self.resolve_for_audience(iss, sub, aud)
            .map_or(&[], |r| r.allowed_scopes.as_slice())
    }

    /// Owned, secret-free projection of every loaded binding, in stable
    /// `(iss, sub)` order. Powers `GET /v1/admin/habilitations`
    /// (operator introspection). It NEVER carries
    /// the BLAKE3 seal nor any admin credential — only the binding
    /// envelope the operator wrote.
    #[must_use]
    pub fn summaries(&self) -> Vec<BindingSummary> {
        self.by_key
            .iter()
            .map(|((iss, sub, _aud), r)| BindingSummary {
                issuer: iss.clone(),
                sub: sub.clone(),
                habilitation_id: r.nucleon_id.as_str().to_owned(),
                noyau: r.noyau.as_str().to_owned(),
                audience: r.audience.clone(),
                allowed_scopes: r.allowed_scopes.clone(),
            })
            .collect()
    }

    /// Verify the BLAKE3 seal on `(iss, sub)`. Callers MUST consult
    /// this before trusting a [`Self::resolve`] return value in any
    /// state-mutating path.
    #[must_use]
    pub fn seal_intact(&self, iss: &str, sub: &str) -> bool {
        // Principal-level seal: every per-galaxy binding for this
        // `(iss, sub)` must be intact. With a single binding (the local
        // tenant case) this is identical to the old behaviour; for a
        // federated principal it is conservative — a single tampered
        // galaxy grant fails the whole principal.
        let mut any = false;
        for ((i, s, aud), recorded) in &self.seals {
            if i != iss || s != sub {
                continue;
            }
            any = true;
            match self.live_seals.get(&(i.clone(), s.clone(), aud.clone())) {
                Some(live) if live == recorded => {}
                _ => return false,
            }
        }
        // No recorded seal at all ⇒ builder path (tests opt out).
        if any {
            return true;
        }
        !self.live_seals.keys().any(|(i, s, _)| i == iss && s == sub)
    }

    /// Verify the BLAKE3 seal on an exact `(iss, sub, aud)` binding — the
    /// per-galaxy seal check used by the admission hot path once the
    /// audience has pinned the galaxy. Returns `true` on the builder path
    /// (no recorded seal) so tests opt out exactly as with
    /// [`Self::seal_intact`].
    #[must_use]
    pub fn seal_intact_for_audience(&self, iss: &str, sub: &str, aud: &str) -> bool {
        let key = (iss.to_owned(), sub.to_owned(), aud.to_owned());
        match (self.seals.get(&key), self.live_seals.get(&key)) {
            (Some(recorded), Some(live)) => recorded == live,
            (None, None) => true, // builder path — tests opt out
            _ => false,
        }
    }

    /// Load every `<state_dir>/nucleons/<nucleon_id>/oidc-identity.toml`
    /// file. Malformed files are logged and skipped.
    ///
    /// # Errors
    ///
    /// Returns an error if the parent directory cannot be enumerated;
    /// individual file failures are tolerated (skipped + logged) so a
    /// single bad provisioning record does not blackhole the map.
    pub fn load(state_dir: &Path) -> std::io::Result<Self> {
        let mut out = Self::default();
        let root = state_dir.join("nucleons");
        if !root.exists() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(&root)?.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let dir = entry.path();
            // Multiple `oidc-identity*.toml` files allowed (one per
            // Orbitale).
            for ident_file in std::fs::read_dir(&dir)?.flatten() {
                let path = ident_file.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !name.starts_with("oidc-identity") || !is_toml(name) {
                    continue;
                }
                let text = match std::fs::read_to_string(&path) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "skipping unreadable oidc-identity file");
                        continue;
                    }
                };
                let parsed: OidcIdentity = match toml::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "skipping malformed oidc-identity file");
                        continue;
                    }
                };
                let seal = blake3::hash(text.as_bytes()).to_hex().to_string();
                let key = (
                    parsed.oidc.issuer.clone(),
                    parsed.oidc.sub.clone(),
                    parsed.oidc.audience.clone(),
                );
                out.seals.insert(key.clone(), seal.clone());
                out.live_seals.insert(key.clone(), seal);
                let allowed_scopes = parsed
                    .scopes
                    .as_ref()
                    .map(|g| g.allowed.clone())
                    .unwrap_or_default();
                out.by_key.insert(
                    key,
                    Resolved {
                        nucleon_id: HabilitationId::new(parsed.nucleon_id.clone()),
                        noyau: Noyau::new(parsed.noyau.clone()),
                        audience: parsed.oidc.audience.clone(),
                        allowed_scopes,
                        drain_bounds: DrainBounds::from_spec(parsed.drain_bounds.as_ref()),
                        federated: None,
                    },
                );
            }
        }
        Ok(out)
    }

    /// Recompute every live seal from the on-disk content. Called
    /// once per refresh window (V1+ — cached at boot for V0).
    pub fn refresh_live_seals(&mut self, state_dir: &Path) {
        for ((iss, sub, aud), resolved) in &self.by_key.clone() {
            let path = candidate_paths(state_dir, &resolved.nucleon_id);
            let mut found_live = None;
            for p in path {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    if let Ok(parsed) = toml::from_str::<OidcIdentity>(&text) {
                        if &parsed.oidc.issuer == iss
                            && &parsed.oidc.sub == sub
                            && &parsed.oidc.audience == aud
                        {
                            let seal = blake3::hash(text.as_bytes()).to_hex().to_string();
                            found_live = Some(seal);
                            break;
                        }
                    }
                }
            }
            let key = (iss.clone(), sub.clone(), aud.clone());
            match found_live {
                Some(seal) => {
                    self.live_seals.insert(key, seal);
                }
                None => {
                    self.live_seals.remove(&key);
                }
            }
        }
    }

    /// Test/util builder.
    #[must_use]
    pub fn builder() -> HabilitationMapBuilder {
        HabilitationMapBuilder::default()
    }
}

/// Live, atomically-swappable handle to the [`HabilitationMap`].
///
/// The adapter loads the sealed bindings once at boot and reads the map
/// on every request, but it reloads the map only on an explicit operator
/// gesture (`SIGHUP`, see [`crate::reload`]). `arc-swap` is the right
/// tool for this read-mostly/reload-rarely shape: reads are lock-free
/// (no `RwLock` contention on the admission hot path) and a reload is a
/// single atomic pointer store. A request that already holds a
/// [`Guard`] keeps reading its consistent snapshot while a new map is
/// published behind it — so reloading a binding never blocks a request
/// and, crucially, never requires a process restart that would tear down
/// the in-flight tmux workers.
///
/// The handle is cheaply [`Clone`] (one `Arc` bump): boot clones it into
/// the SIGHUP listener task and moves the original into [`crate::AppState`].
#[derive(Clone, Debug)]
pub struct SharedHabilitationMap(Arc<ArcSwap<HabilitationMap>>);

impl SharedHabilitationMap {
    /// Wrap an initial map (typically [`HabilitationMap::load`] at boot).
    #[must_use]
    pub fn new(map: HabilitationMap) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(map)))
    }

    /// Load a consistent, lock-free snapshot of the live map. Hold the
    /// returned guard for the lifetime of any borrow taken from it
    /// (`resolve`, `allowed_scopes_for` return references into the
    /// snapshot); for owned-return accessors (`binding_count`,
    /// `noyaux`, `noyaux_for_sub`) the guard may be a temporary.
    #[must_use]
    pub fn load(&self) -> Guard<Arc<HabilitationMap>> {
        self.0.load()
    }

    /// Atomically publish a new map. Readers holding an existing guard
    /// keep their old snapshot; subsequent [`Self::load`] calls observe
    /// the new one. The swap is a single store with no reader blocking.
    pub fn store(&self, map: HabilitationMap) {
        self.0.store(Arc::new(map));
    }
}

fn is_toml(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
}

fn candidate_paths(state_dir: &Path, nucleon_id: &HabilitationId) -> Vec<PathBuf> {
    let mut out = vec![];
    let dir = state_dir.join("nucleons").join(nucleon_id.as_str());
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("oidc-identity") && is_toml(name) {
                    out.push(path);
                }
            }
        }
    }
    out
}

/// Test-only builder for [`HabilitationMap`].
#[derive(Clone, Debug, Default)]
pub struct HabilitationMapBuilder {
    entries: Vec<(String, String, Resolved)>,
}

impl HabilitationMapBuilder {
    /// Insert a binding without on-disk seal machinery. The resulting
    /// binding carries no granted scopes; use
    /// [`Self::insert_with_scopes`] to populate `[scopes].allowed`
    /// directly.
    #[must_use]
    pub fn insert(
        mut self,
        iss: impl Into<String>,
        sub: impl Into<String>,
        nucleon_id: HabilitationId,
        noyau: Noyau,
        audience: impl Into<String>,
    ) -> Self {
        self.entries.push((
            iss.into(),
            sub.into(),
            Resolved {
                nucleon_id,
                noyau,
                audience: audience.into(),
                allowed_scopes: Vec::new(),
                drain_bounds: DrainBounds::default(),
                federated: None,
            },
        ));
        self
    }

    /// Insert a binding with explicit `[scopes].allowed` grants
    /// (T23). Used by tests that exercise the
    /// admin-binding scope-union path.
    #[must_use]
    pub fn insert_with_scopes(
        mut self,
        iss: impl Into<String>,
        sub: impl Into<String>,
        nucleon_id: HabilitationId,
        noyau: Noyau,
        audience: impl Into<String>,
        allowed_scopes: Vec<String>,
    ) -> Self {
        self.entries.push((
            iss.into(),
            sub.into(),
            Resolved {
                nucleon_id,
                noyau,
                audience: audience.into(),
                allowed_scopes,
                drain_bounds: DrainBounds::default(),
                federated: None,
            },
        ));
        self
    }

    /// Finalise the map.
    #[must_use]
    pub fn build(self) -> HabilitationMap {
        let mut map = HabilitationMap::default();
        for (iss, sub, resolved) in self.entries {
            let aud = resolved.audience.clone();
            map.by_key.insert((iss, sub, aud), resolved);
        }
        map
    }
}

// ─── Operator-side binding renderer (Pierre hardening P2) ───────────────
//
// `task-20260605-e26a`. The (iss, sub) → noyau binding is the §8j
// root-of-trust: whoever can write it mints a tenant axis and grants its
// scopes. It is therefore OPERATOR-ONLY and host-side — never a
// tenant-JWT-reachable API (see the security audit in smithy
// docs/ops/2026-06-05-pierre-hardening-3-propositions.md). Today the
// `oidc-identity.toml` is hand-rendered by a bash heredoc
// (`provision-noyau.sh`), whose typo surface is the documented cause of
// `issuer_not_pinned` drift (a stray trailing slash desyncs the three
// copies of `iss`). This module is the audited, validated renderer that
// replaces the heredoc: it builds the SAME [`OidcIdentity`] the loader
// parses (zero schema drift — the round-trip is asserted in tests) and
// refuses to emit a structurally-invalid binding.

/// Why a [`HabilitationBindingSpec`] was refused. Surfaced verbatim to the
/// operator so the fix is mechanical.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenderError {
    /// A required field was empty (after trimming).
    EmptyField(&'static str),
    /// `issuer` / `oidc` is not an absolute `http(s)://` URL — the most
    /// common provisioning mistake, and the one that silently desyncs
    /// the `iss` pin.
    NotAbsoluteUrl {
        /// The offending field name.
        field: &'static str,
        /// The value as supplied.
        value: String,
    },
    /// A scope string was empty or carried whitespace — the binding
    /// allowlist is matched verbatim, so a stray space never grants.
    MalformedScope(String),
    /// `toml` serialization failed (should be unreachable for the fixed
    /// schema; surfaced rather than panicked).
    Serialize(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::EmptyField(name) => write!(f, "field `{name}` must not be empty"),
            RenderError::NotAbsoluteUrl { field, value } => write!(
                f,
                "field `{field}` must be an absolute http(s):// URL, got `{value}`"
            ),
            RenderError::MalformedScope(s) => {
                write!(f, "scope `{s}` is empty or contains whitespace")
            }
            RenderError::Serialize(e) => write!(f, "toml serialization failed: {e}"),
        }
    }
}

impl std::error::Error for RenderError {}

/// The four authorization params (+ optional metadata) an operator
/// supplies to mint a nucleon binding. Mirrors `provision-noyau.sh`'s
/// flags so the script can shell out to the audited renderer instead of
/// rendering TOML by hand.
#[derive(Clone, Debug)]
pub struct HabilitationBindingSpec {
    /// Tenant axis (galaxy slot). State materialises at
    /// `<galaxies_root>/<noyau>/`.
    pub noyau: String,
    /// JWT `sub` claim the binding admits.
    pub sub: String,
    /// JWT `iss` claim — byte-for-byte equal to the `IdP` `--issuer` and
    /// the minted JWT (wiring contract A).
    pub issuer: String,
    /// JWT `aud` claim pinned to this deployment.
    pub audience: String,
    /// Directory name under `nucleons/`. Defaults to `noyau` when `None`.
    pub nucleon_id: Option<String>,
    /// Cognitive-substrate label (ADR-063). Defaults to `Biological`.
    pub phase: Option<String>,
    /// Binding-granted scopes. Empty → no `[scopes]` section (legacy
    /// JWT-scopes-only admission).
    pub scopes: Vec<String>,
    /// ISO-8601 provisioning timestamp. Informational; pass the operator
    /// clock (the renderer is pure and does not read the wall clock).
    pub sealed_at: Option<String>,
}

/// Default cognitive-substrate label when the operator omits `--phase`.
pub const DEFAULT_BINDING_PHASE: &str = "Biological";

fn require_non_empty(name: &'static str, value: &str) -> Result<String, RenderError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(RenderError::EmptyField(name));
    }
    Ok(trimmed.to_owned())
}

fn require_absolute_url(field: &'static str, value: &str) -> Result<String, RenderError> {
    let v = require_non_empty(field, value)?;
    if !(v.starts_with("http://") || v.starts_with("https://")) {
        return Err(RenderError::NotAbsoluteUrl {
            field,
            value: v.clone(),
        });
    }
    Ok(v)
}

/// Build a validated [`OidcIdentity`] from the spec. Pure; the only
/// failure modes are the [`RenderError`] variants. Reused by
/// [`render_oidc_identity_toml`] and unit-testable without TOML.
///
/// # Errors
///
/// Returns [`RenderError`] when a required field is empty, the
/// `issuer`/`audience` URL is not absolute, or a scope is malformed.
pub fn build_binding(spec: &HabilitationBindingSpec) -> Result<OidcIdentity, RenderError> {
    let noyau = require_non_empty("noyau", &spec.noyau)?;
    let sub = require_non_empty("sub", &spec.sub)?;
    let issuer = require_absolute_url("issuer", &spec.issuer)?;
    let audience = require_non_empty("audience", &spec.audience)?;
    let nucleon_id = match &spec.nucleon_id {
        Some(id) => require_non_empty("nucleon_id", id)?,
        None => noyau.clone(),
    };
    let phase = match &spec.phase {
        Some(p) => require_non_empty("phase", p)?,
        None => DEFAULT_BINDING_PHASE.to_owned(),
    };
    let mut allowed = Vec::with_capacity(spec.scopes.len());
    for scope in &spec.scopes {
        if scope.is_empty() || scope.chars().any(char::is_whitespace) {
            return Err(RenderError::MalformedScope(scope.clone()));
        }
        allowed.push(scope.clone());
    }
    let scopes = if allowed.is_empty() {
        None
    } else {
        Some(ScopesGrant { allowed })
    };
    Ok(OidcIdentity {
        // Drain bounds are not part of the operator-rendered spec yet:
        // the renderer covers the identity/scopes envelope; the
        // operator adds `[drain_bounds]` by hand when the defaults do
        // not fit. Absent → server defaults (never unbounded).
        drain_bounds: None,
        nucleon_id,
        phase,
        noyau,
        oidc: OidcClaims {
            issuer,
            sub,
            audience,
            sealed_at: spec.sealed_at.clone().unwrap_or_default(),
        },
        scopes,
    })
}

/// Render a validated `oidc-identity.toml` body from the four-tuple.
/// The output parses back to the same binding via [`HabilitationMap::load`]
/// — the renderer and the loader share the [`OidcIdentity`] schema, so
/// there is no second source of truth to drift.
///
/// # Errors
///
/// Propagates [`build_binding`]'s validation failures, plus a
/// [`RenderError::Serialize`] if TOML emission fails.
pub fn render_oidc_identity_toml(spec: &HabilitationBindingSpec) -> Result<String, RenderError> {
    let binding = build_binding(spec)?;
    let body =
        toml::to_string_pretty(&binding).map_err(|e| RenderError::Serialize(e.to_string()))?;
    // A leading provenance comment mirrors the heredoc's header so an
    // operator reading the file on disk knows what rendered it.
    Ok(format!(
        "# Nucleon binding — (iss, sub) → noyau. Rendered by the audited\n\
         # cosmon-rpp-adapter `nucleon render` path (Pierre hardening P2,\n\
         # task-20260605-e26a). issuer + audience must match the IdP and the\n\
         # JWT byte-for-byte (wiring contract A). Cross-noyau pivot stays\n\
         # structurally impossible (ADR-080 §8j clauses (a)+(e)).\n\
         {body}"
    ))
}

// ─────────────────────────────────────────────────────────────────────────
// ADR-0022 D4 (2026-06-16) — `nucléon` → `habilitation` rename, Phase A.
//
// The canonical type names are now `Habilitation*`. The `Nucleon*` aliases
// below are the **backward-compat surface** for any out-of-workspace or
// not-yet-migrated consumer. They are wire/disk-neutral: every alias points
// at the renamed type, and no serialized field name or on-disk path
// (`nucleon_id`, `.cosmon/state/nucleons/<id>/oidc-identity.toml`) is touched
// in this phase — those carry the §8j root-of-trust binding and migrate later
// behind read-compat shims (see Phase B in the migration plan). Remove these
// aliases in Phase C once every consumer has switched and the deprecation
// window has elapsed.
// ─────────────────────────────────────────────────────────────────────────

/// Deprecated alias for [`HabilitationId`]. Renamed per ADR-0022 D4.
#[deprecated(note = "ADR-0022 D4: renamed to `HabilitationId`; removed in Phase C")]
pub type NucleonId = HabilitationId;

/// Deprecated alias for [`HabilitationMap`]. Renamed per ADR-0022 D4.
#[deprecated(note = "ADR-0022 D4: renamed to `HabilitationMap`; removed in Phase C")]
pub type NucleonMap = HabilitationMap;

/// Deprecated alias for [`SharedHabilitationMap`]. Renamed per ADR-0022 D4.
#[deprecated(note = "ADR-0022 D4: renamed to `SharedHabilitationMap`; removed in Phase C")]
pub type SharedNucleonMap = SharedHabilitationMap;

/// Deprecated alias for [`HabilitationMapBuilder`]. Renamed per ADR-0022 D4.
#[deprecated(note = "ADR-0022 D4: renamed to `HabilitationMapBuilder`; removed in Phase C")]
pub type NucleonMapBuilder = HabilitationMapBuilder;

/// Deprecated alias for [`HabilitationBindingSpec`]. Renamed per ADR-0022 D4.
#[deprecated(note = "ADR-0022 D4: renamed to `HabilitationBindingSpec`; removed in Phase C")]
pub type NucleonBindingSpec = HabilitationBindingSpec;

#[cfg(test)]
mod tests {
    use super::*;

    // ── drain bounds (B1/B2/B3 moussage, task-20260610-e5f6) ────────────

    #[test]
    fn drain_bounds_default_is_never_unbounded() {
        // godel Q3: B3 is obligatory — an absent [drain_bounds]
        // section resolves to concrete server defaults, never ∞.
        let b = DrainBounds::from_spec(None);
        assert_eq!(b.budget, DEFAULT_DRAIN_BUDGET);
        assert_eq!(b.max_depth, DEFAULT_DRAIN_MAX_DEPTH);
        assert_eq!(b.max_molecules, DEFAULT_DRAIN_MAX_MOLECULES);
    }

    #[test]
    fn drain_bounds_partial_override_keeps_other_defaults() {
        let spec = DrainBoundsSpec {
            budget: Some(12),
            max_depth: None,
            max_molecules: Some(40),
        };
        let b = DrainBounds::from_spec(Some(&spec));
        assert_eq!(b.budget, 12);
        assert_eq!(b.max_depth, DEFAULT_DRAIN_MAX_DEPTH);
        assert_eq!(b.max_molecules, 40);
    }

    #[test]
    fn oidc_identity_parses_drain_bounds_section() {
        let toml_text = r#"
nucleon_id = "nuc-a"
phase = "Biological"
noyau = "tenant-demo"

[oidc]
issuer = "https://idp"
sub = "sub-123"
audience = "cosmon-rpp-tenant"

[drain_bounds]
budget = 9
max_depth = 3
"#;
        let parsed: OidcIdentity = toml::from_str(toml_text).expect("parse");
        let b = DrainBounds::from_spec(parsed.drain_bounds.as_ref());
        assert_eq!(b.budget, 9);
        assert_eq!(b.max_depth, 3);
        assert_eq!(b.max_molecules, DEFAULT_DRAIN_MAX_MOLECULES);
    }

    #[test]
    fn legacy_binding_without_drain_bounds_still_parses() {
        let toml_text = r#"
nucleon_id = "nuc-a"
phase = "Biological"
noyau = "tenant-demo"

[oidc]
issuer = "https://idp"
sub = "sub-123"
audience = "cosmon-rpp-tenant"
"#;
        let parsed: OidcIdentity = toml::from_str(toml_text).expect("parse legacy");
        assert!(parsed.drain_bounds.is_none());
        assert_eq!(
            DrainBounds::from_spec(parsed.drain_bounds.as_ref()),
            DrainBounds::default()
        );
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let map = HabilitationMap::default();
        assert!(map.resolve("https://idp", "sub").is_none());
    }

    #[test]
    fn builder_path_treats_seal_as_intact() {
        let map = HabilitationMap::builder()
            .insert(
                "https://idp",
                "sub-123",
                HabilitationId::new("nuc-a"),
                Noyau::new("tenant-demo"),
                "cosmon-rpp-tenant-demo",
            )
            .build();
        assert!(map.seal_intact("https://idp", "sub-123"));
        let r = map.resolve("https://idp", "sub-123").unwrap();
        assert_eq!(r.nucleon_id.as_str(), "nuc-a");
        assert_eq!(r.noyau.as_str(), "tenant-demo");
        assert_eq!(r.audience, "cosmon-rpp-tenant-demo");
    }

    #[test]
    fn load_returns_empty_when_dir_missing() {
        let td = tempfile::TempDir::new().unwrap();
        let map = HabilitationMap::load(td.path()).unwrap();
        assert!(map.resolve("any", "any").is_none());
    }

    #[test]
    fn load_seals_present_files() {
        let td = tempfile::TempDir::new().unwrap();
        let dir = td.path().join("nucleons/nuc-a");
        std::fs::create_dir_all(&dir).unwrap();
        let body = r#"
nucleon_id = "nuc-a"
phase = "Biological"
noyau = "tenant-demo"

[oidc]
issuer = "https://idp"
sub = "sub-123"
audience = "cosmon-rpp-tenant-demo"
sealed_at = "2026-04-27T14:00:00Z"
"#;
        std::fs::write(dir.join("oidc-identity.toml"), body).unwrap();
        let map = HabilitationMap::load(td.path()).unwrap();
        let r = map.resolve("https://idp", "sub-123").unwrap();
        assert_eq!(r.nucleon_id.as_str(), "nuc-a");
        assert!(map.seal_intact("https://idp", "sub-123"));
    }

    #[test]
    fn allowed_scopes_default_to_empty_when_section_absent() {
        let td = tempfile::TempDir::new().unwrap();
        let dir = td.path().join("nucleons/nuc-legacy");
        std::fs::create_dir_all(&dir).unwrap();
        // Legacy binding — no [scopes] section at all.
        let body = r#"
nucleon_id = "nuc-legacy"
phase = "Biological"
noyau = "tenant-demo"

[oidc]
issuer = "https://idp"
sub = "sub-legacy"
audience = "cosmon-rpp-tenant-demo"
"#;
        std::fs::write(dir.join("oidc-identity.toml"), body).unwrap();
        let map = HabilitationMap::load(td.path()).unwrap();
        assert!(map
            .allowed_scopes_for("https://idp", "sub-legacy")
            .is_empty());
    }

    #[test]
    fn allowed_scopes_loaded_from_disk_when_section_present() {
        let td = tempfile::TempDir::new().unwrap();
        let dir = td.path().join("nucleons/you-democorp");
        std::fs::create_dir_all(&dir).unwrap();
        let body = r#"
nucleon_id = "you-democorp"
phase = "Biological"
noyau = "democorp"

[oidc]
issuer = "https://forgejo.cosmon-state.svc.cluster.local"
sub = "you"
audience = "cosmon-rpp-democorp"

[scopes]
allowed = ["cosmon:molecule:read", "cosmon:molecule:write"]
"#;
        std::fs::write(dir.join("oidc-identity.toml"), body).unwrap();
        let map = HabilitationMap::load(td.path()).unwrap();
        let scopes =
            map.allowed_scopes_for("https://forgejo.cosmon-state.svc.cluster.local", "you");
        assert_eq!(scopes.len(), 2);
        assert!(scopes.iter().any(|s| s == "cosmon:molecule:read"));
        assert!(scopes.iter().any(|s| s == "cosmon:molecule:write"));
    }

    #[test]
    fn allowed_scopes_for_unknown_binding_is_empty_slice() {
        let map = HabilitationMap::default();
        assert!(map.allowed_scopes_for("nope", "nope").is_empty());
    }

    #[test]
    fn builder_with_scopes_round_trips() {
        let map = HabilitationMap::builder()
            .insert_with_scopes(
                "https://idp",
                "admin-1",
                HabilitationId::new("you-democorp"),
                Noyau::new("democorp"),
                "cosmon-rpp-democorp",
                vec!["cosmon:molecule:write".into()],
            )
            .build();
        let scopes = map.allowed_scopes_for("https://idp", "admin-1");
        assert_eq!(scopes, &["cosmon:molecule:write".to_owned()]);
    }

    #[test]
    fn noyaux_dedupes_shared_tenants() {
        // Three bindings, two distinct noyaux: two nucléons share
        // `tenant-demo`, one is in `democorp`. Materialization is per-noyau,
        // so `noyaux()` must collapse the shared tenant to one entry.
        let map = HabilitationMap::builder()
            .insert(
                "https://idp",
                "sub-a",
                HabilitationId::new("nuc-a"),
                Noyau::new("tenant-demo"),
                "aud",
            )
            .insert(
                "https://idp",
                "sub-b",
                HabilitationId::new("nuc-b"),
                Noyau::new("tenant-demo"),
                "aud",
            )
            .insert(
                "https://idp",
                "sub-c",
                HabilitationId::new("nuc-c"),
                Noyau::new("democorp"),
                "aud",
            )
            .build();
        let noyaux = map.noyaux();
        assert_eq!(noyaux.len(), 2);
        assert!(noyaux.iter().any(|n| n.as_str() == "tenant-demo"));
        assert!(noyaux.iter().any(|n| n.as_str() == "democorp"));
    }

    #[test]
    fn noyaux_empty_when_no_bindings() {
        assert!(HabilitationMap::default().noyaux().is_empty());
    }

    #[test]
    fn noyaux_for_sub_returns_distinct_noyaux_with_counts() {
        // Three bindings for `you`:
        //   - tenant-demo-sandbox under issuer A
        //   - tenant-demo-sandbox under issuer B (same noyau, two IdPs)
        //   - operator-sandbox under issuer A
        // Plus an unrelated sub `someone-else` that must NOT leak.
        let map = HabilitationMap::builder()
            .insert(
                "https://idp-a",
                "you",
                HabilitationId::new("nuc-a"),
                Noyau::new("tenant-demo-sandbox"),
                "aud",
            )
            .insert(
                "https://idp-b",
                "you",
                HabilitationId::new("nuc-b"),
                Noyau::new("tenant-demo-sandbox"),
                "aud",
            )
            .insert(
                "https://idp-a",
                "you-second",
                HabilitationId::new("nuc-c"),
                Noyau::new("operator-sandbox"),
                "aud",
            )
            .insert(
                "https://idp-a",
                "someone-else",
                HabilitationId::new("nuc-x"),
                Noyau::new("hidden-sandbox"),
                "aud",
            )
            .build();
        let rows = map.noyaux_for_sub("you");
        assert_eq!(rows.len(), 1, "single noyau collapses across two issuers");
        assert_eq!(rows[0].0.as_str(), "tenant-demo-sandbox");
        assert_eq!(rows[0].1, 2, "two (iss, sub) bindings → count 2");

        // Distinct sub maps to a distinct noyau.
        let rows = map.noyaux_for_sub("you-second");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0.as_str(), "operator-sandbox");
        assert_eq!(rows[0].1, 1);
    }

    #[test]
    fn noyaux_for_sub_collapses_two_noyaux_for_same_principal() {
        // Same sub appears under two issuers, each pointing to a
        // different noyau — both must show up with binding_count = 1.
        let map = HabilitationMap::builder()
            .insert(
                "https://idp-a",
                "you",
                HabilitationId::new("nuc-a"),
                Noyau::new("tenant-demo-sandbox"),
                "aud",
            )
            .insert(
                "https://idp-b",
                "you",
                HabilitationId::new("nuc-b"),
                Noyau::new("operator-sandbox"),
                "aud",
            )
            .build();
        let rows = map.noyaux_for_sub("you");
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .any(|(n, c)| n.as_str() == "tenant-demo-sandbox" && *c == 1));
        assert!(rows
            .iter()
            .any(|(n, c)| n.as_str() == "operator-sandbox" && *c == 1));
    }

    #[test]
    fn noyaux_for_sub_empty_when_no_match() {
        let map = HabilitationMap::builder()
            .insert(
                "https://idp",
                "you",
                HabilitationId::new("nuc-a"),
                Noyau::new("tenant-demo-sandbox"),
                "aud",
            )
            .build();
        assert!(map.noyaux_for_sub("unbound-principal").is_empty());
    }

    #[test]
    fn seal_breaks_after_disk_edit() {
        let td = tempfile::TempDir::new().unwrap();
        let dir = td.path().join("nucleons/nuc-a");
        std::fs::create_dir_all(&dir).unwrap();
        let body = r#"
nucleon_id = "nuc-a"
phase = "Biological"
noyau = "tenant-demo"

[oidc]
issuer = "https://idp"
sub = "sub-123"
audience = "cosmon-rpp-tenant-demo"
"#;
        let path = dir.join("oidc-identity.toml");
        std::fs::write(&path, body).unwrap();
        let mut map = HabilitationMap::load(td.path()).unwrap();
        // Edit on disk — seal MUST diverge.
        std::fs::write(
            &path,
            body.replace("noyau = \"tenant-demo\"", "noyau = \"noog\""),
        )
        .unwrap();
        map.refresh_live_seals(td.path());
        assert!(!map.seal_intact("https://idp", "sub-123"));
    }

    // ── Operator-side renderer (Pierre hardening P2) ────────────────────

    fn sample_spec() -> HabilitationBindingSpec {
        HabilitationBindingSpec {
            noyau: "tenant-demo-research".into(),
            sub: "research-operator".into(),
            issuer: "http://oidc-mock:8444".into(),
            audience: "cosmon-rpp-tenant-demo".into(),
            nucleon_id: None,
            phase: None,
            scopes: vec![
                "cosmon:molecule:read".into(),
                "cosmon:molecule:write".into(),
            ],
            sealed_at: Some("2026-06-05T00:00:00Z".into()),
        }
    }

    #[test]
    fn render_round_trips_through_loader() {
        // The load-bearing zero-drift assertion: the renderer's output is
        // parsed back by the SAME HabilitationMap::load the adapter uses at boot.
        let spec = sample_spec();
        let body = render_oidc_identity_toml(&spec).unwrap();

        let td = tempfile::TempDir::new().unwrap();
        let dir = td.path().join("nucleons").join("tenant-demo-research");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oidc-identity.toml"), &body).unwrap();

        let map = HabilitationMap::load(td.path()).unwrap();
        let resolved = map
            .resolve("http://oidc-mock:8444", "research-operator")
            .expect("binding must resolve");
        assert_eq!(resolved.noyau.as_str(), "tenant-demo-research");
        assert_eq!(resolved.nucleon_id.as_str(), "tenant-demo-research");
        assert_eq!(resolved.audience, "cosmon-rpp-tenant-demo");
        assert_eq!(
            resolved.allowed_scopes,
            vec![
                "cosmon:molecule:read".to_owned(),
                "cosmon:molecule:write".to_owned()
            ]
        );
        assert!(map.seal_intact("http://oidc-mock:8444", "research-operator"));
    }

    #[test]
    fn render_defaults_nucleon_id_to_noyau_and_phase_to_biological() {
        let mut spec = sample_spec();
        spec.nucleon_id = None;
        spec.phase = None;
        let binding = build_binding(&spec).unwrap();
        assert_eq!(binding.nucleon_id, "tenant-demo-research");
        assert_eq!(binding.phase, "Biological");
    }

    #[test]
    fn render_omits_scopes_section_when_empty() {
        let mut spec = sample_spec();
        spec.scopes = vec![];
        let binding = build_binding(&spec).unwrap();
        assert!(binding.scopes.is_none());
        let body = render_oidc_identity_toml(&spec).unwrap();
        assert!(!body.contains("[scopes]"));
    }

    #[test]
    fn render_rejects_empty_required_fields() {
        let mut spec = sample_spec();
        spec.sub = "   ".into();
        assert_eq!(
            build_binding(&spec).unwrap_err(),
            RenderError::EmptyField("sub")
        );
    }

    #[test]
    fn render_rejects_non_absolute_issuer() {
        let mut spec = sample_spec();
        spec.issuer = "oidc-mock:8444".into();
        match build_binding(&spec).unwrap_err() {
            RenderError::NotAbsoluteUrl { field, .. } => assert_eq!(field, "issuer"),
            other => panic!("expected NotAbsoluteUrl, got {other:?}"),
        }
    }

    #[test]
    fn render_rejects_scope_with_whitespace() {
        let mut spec = sample_spec();
        spec.scopes = vec!["cosmon:molecule:read ".into()];
        match build_binding(&spec).unwrap_err() {
            RenderError::MalformedScope(s) => assert_eq!(s, "cosmon:molecule:read "),
            other => panic!("expected MalformedScope, got {other:?}"),
        }
    }

    #[test]
    fn render_trims_and_pins_issuer_byte_for_byte() {
        // A trailing-slash / whitespace mishap is the documented cause of
        // issuer_not_pinned; the renderer trims surrounding whitespace but
        // preserves the URL exactly so iss stays byte-for-byte.
        let mut spec = sample_spec();
        spec.issuer = "  https://idp.example.com/oidc  ".into();
        let binding = build_binding(&spec).unwrap();
        assert_eq!(binding.oidc.issuer, "https://idp.example.com/oidc");
    }
}
