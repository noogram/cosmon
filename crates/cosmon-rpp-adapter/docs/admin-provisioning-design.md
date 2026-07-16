# Design — Admin provisioning route (create a *habilitation* via API, no SSH)

> **Type.** Design / ADR-proposal feeding a future cosmon ADR.
> **Origine.** smithy molécule `task-20260616-1acd` (trou B1, rôle architect).
> Réf. smithy **ADR-0022 §2.2 + §6** (rationalisation de l'ontologie cosmon,
> Accepted 2026-06-16). **smithy PLANIFIE — ce document est le design ; l'impl
> atterrit ici (branche `feat/task-20260616-1acd-admin-provisioning`) en B2.**
> **Status.** Proposed — 2026-06-16. Design + signatures d'API. Pas de code
> d'implémentation dans ce livrable (rôle architect).

---

## 0. Vocabulaire (ADR-0022, Accepted 2026-06-16)

- **doc = `habilitation`**, jamais « nucléon ». Le **code** dit encore
  `nucleon_id` (champ sérialisé) / chemin disque `nucleons/` — dette Phase B/C
  pas encore résorbée (`docs/MIGRATION-nucleon-to-habilitation.md`).
- **Noms de type — dépendance de branche (vérifié 2026-06-16).** Le rename Phase A
  (`Habilitation*` canon, alias `Nucleon*` `#[deprecated]`) vit sur la branche
  `feat/task-20260616-a677-nucleon-habilitation` qui **n'est PAS encore mergée sur
  `main`**. Sur `main` (base de cette branche B1), les noms canon sont **encore
  `Nucleon*`** (`NucleonMap`, `NucleonBindingSpec`, `SharedNucleonMap`,
  `build_binding`, `render_oidc_identity_toml` — vérifiés `nucleon_map.rs:224,470,652,705,758`).
  **Règle pour B2 :** utiliser les noms canon **en vigueur au moment de l'impl** —
  `Nucleon*` si B2 part de `main` avant le merge a677 ; `Habilitation*` si a677 a
  mergé. Les signatures §4 emploient `Habilitation*` par anticipation (cible canon
  ADR-0022) ; substituer `Nucleon*` mécaniquement si la base ne contient pas
  encore le rename. **Le design est invariant au choix de nom** — il porte sur le
  binding, pas sur son orthographe Rust.
- **4 niveaux** : `molécule` · `galaxie` · `portée` = vocabulaire **tenant** ;
  `noyau` / `habilitation` = vocabulaire d'**implémentation**, invisible au tenant.

---

## 1. Contexte — le trou, exactement

Aujourd'hui (vérifié dans `nucleon_map.rs`) :

- Une **habilitation** = le binding scellé `(iss, sub) → noyau` (+ `aud`, +
  scopes T23, + drain_bounds), matérialisé par un fichier
  `<state_dir>/nucleons/<id>/oidc-identity.toml`.
- **AUCUNE route API ne crée une habilitation** (ADR-0022 §2.2). Création =
  l'opérateur écrit le `.toml` **host-side** (heredoc `provision-noyau.sh` ou le
  renderer audité `cs-rpp-adapter nucleon render`, hardening P2), puis recharge
  (`SIGHUP` → `HabilitationMap::load` → `SharedHabilitationMap::store`, atomique
  via `arc-swap`).
- Le commentaire `nucleon_map.rs:591-604` pose l'invariant : *« the (iss, sub) →
  noyau binding is the §8j root-of-trust: whoever can write it mints a tenant
  axis and grants its scopes. It is therefore OPERATOR-ONLY and host-side —
  never a tenant-JWT-reachable API. »*

**Ce que la réunion opérateur veut** (CMB 2026-06-16) : *« je crée un badge via
l'API, et je dis à Jordan de s'authentifier avec »* — sans SSH host-side.

**La tension à résoudre** : « créer le binding via une API » vs « le binding ne
doit JAMAIS être atteignable par un JWT tenant, c'est la racine de confiance ».
Le design ci-dessous montre que ces deux phrases ne sont **pas** contradictoires
— à condition que l'auth de la route admin **ne dérive pas** de la chaîne OIDC
que le binding existe précisément pour backstopper.

---

## 2. Décision de design (load-bearing) — l'auth admin est un *sceau opérateur host-side*, PAS un JWT OIDC

> **L'autorité d'écrire la racine de confiance ne peut pas dériver de la racine
> de confiance elle-même, ni de l'IdP que cette racine existe pour
> backstopper.**

### 2.1 Le piège à éviter (rejeté explicitement)

Approche tentante mais **fausse pour ce cas** : *« créer un scope
`cosmon:admin:provision`, le sceller dans l'habilitation de l'opérateur, et
garder la route admin derrière ce scope (union JWT + binding, comme T23). »*

**Pourquoi c'est faux ici** — c'est la posture (a) par la porte de derrière :

1. **Circularité / bootstrap.** Le scope admin vivrait dans une habilitation.
   Or écrire des habilitations est *précisément* ce que la route protège. La
   première habilitation devrait quand même être écrite host-side → on n'a pas
   supprimé le geste host-side, on l'a juste déplacé d'un cran. Pire : une fois
   la première écrite, toute habilitation portant le scope admin peut en minter
   d'autres → **escalade de privilège auto-entretenue**.
2. **Compromission de l'IdP = écriture du binding.** Si le scope admin est
   atteignable via un JWT (même « binding-granted »), alors un IdP compromis ou
   sur-permissif qui mint le bon `(iss,sub)` peut pivoter vers l'écriture de la
   racine de confiance. C'est **exactement** le single-point-of-trust que la
   **posture (b)** (ADR-0022 §D1) refuse. Deux murs deviennent un.
3. **DoD violé.** « jamais écrit par un JWT tenant » — un scope OIDC *est* porté
   par un JWT. Même « binding-granted », il transite par la validation JWT
   tenant (`JwtVerifier::validate` + `effective_scope_decision`).

### 2.2 La forme retenue

L'auth de la route admin est un **credential opérateur scellé host-side**,
**disjoint** de la chaîne OIDC tenant :

- Un secret à haute entropie, **injecté au boot du conteneur** (variable
  d'env / fichier-secret monté), **jamais mintable par l'IdP**, **jamais présent
  dans un token tenant**.
- La route admin exige ce credential dans un **header dédié**
  (`X-Cosmon-Admin-Token`), comparé en **temps constant** au sceau host-side.
- Le credential vit dans le **même domaine de confiance que le `.toml`
  host-side** : qui peut poser le secret de boot = qui pouvait déjà SSH écrire le
  `.toml`. On ne crée **aucune** nouvelle autorité ; on remplace le **canal**
  (SSH → HTTP authentifié), pas la **racine**.

**Précédent dans le code.** Les routes bootstrap `POST /v1/auth/claude/*` sont
déjà `scope = -` (aucun check OAuth2) et `adapter-only` : elles tournent *avant*
que des credentials existent. La route admin suit le **même patron** (scope `-`,
auth par un mécanisme disjoint de l'OIDC), mais durcie par un sceau au lieu d'un
flux PKCE ouvert.

### 2.3 Ce qui NE change PAS (invariants préservés — exigence DoD)

| Invariant | Statut |
|---|---|
| binding `(iss,sub) → noyau` **deny-by-default** | **préservé** — un `(iss,sub)` inconnu reste rejeté ; la route ne fait qu'**ajouter** des lignes admises |
| sceau **BLAKE3** au load + détection d'édition rétroactive (`seal_intact`) | **préservé** — `HabilitationMap::load` recalcule le sceau ; la route écrit puis reload, comme l'opérateur |
| binding = **racine de confiance host-side §8j**, jamais écrit par un JWT tenant | **préservé** — l'écrivain est l'adapter-en-tant-qu'agent-opérateur, gated par le sceau host-side ; le tenant n'atteint jamais la route |
| reload atomique `arc-swap` (`SharedHabilitationMap::store`), pas de restart | **réutilisé** — la route reload in-process, supprime même le besoin de SIGHUP |
| renderer audité (`build_binding` / `render_oidc_identity_toml`, zéro drift de schéma) | **réutilisé tel quel** — la route est *le renderer-au-dessus-de-HTTP*, pas un second chemin d'écriture |
| posture (b), réversibilité vers (a) | **préservée** — quand un IdP fédéré mûr portera la portée, l'habilitation rétrécit ; ce design ne la supprime pas |

> **L'image (registre Feynman).** Avant : le concierge (opérateur) descend à la
> cave par l'escalier de service (SSH) et grave une nouvelle clé sur l'établi
> scellé. Maintenant : on installe un **monte-charge fermé à clé** (la route
> admin) qui descend au même établi. La clé du monte-charge (le sceau
> opérateur) est posée à l'installation de l'immeuble (boot du conteneur), pas
> distribuée aux locataires. Les locataires (JWT tenant) n'ont **pas** le bouton
> du monte-charge ; ils n'ont que la clé de leur appartement. L'établi, la
> gravure, le coffre scellé : **inchangés**. Seul l'escalier devient un
> monte-charge.

---

## 3. Surface API

Trois objets dans le DoD : **galaxie / noyau / habilitation** + *lier
identité(s) ↔ noyau(x)*. Rappel ontologie : `galaxie` ≅ `noyau` (deux registres,
un objet — ADR-0022 §P2) ; le **noyau** se matérialise en répertoire
`/srv/cosmon/<noyau>/`. Une habilitation binde **1→1** (`(iss,sub) → un noyau`) ;
*lier N identités ou N noyaux* = **N habilitations** (pas un binding multivalent
— ADR-0022 §2.1).

### 3.1 Routes

```
POST   /v1/admin/habilitations          — provisionne UNE habilitation
                                           (crée le noyau si absent, écrit le
                                           binding, reload). Idempotent par
                                           (iss,sub).
GET    /v1/admin/habilitations          — liste les habilitations provisionnées
                                           (introspection opérateur ; jamais le
                                           secret, jamais le sceau brut).
DELETE /v1/admin/habilitations/{id}     — révoque (supprime le binding + reload).
                                           (optionnel B2 ; révocation = retirer
                                           une porte, symétrique de la création.)
```

Toutes : principal **operator**, exposition **adapter-only**, scope **`-`**
(auth = sceau, pas OAuth2). **Aucune** n'a de `#[verb]` (adapter-native).

> **Note §8p.** `operator-only` *exposure* est interdit sur la surface gelée
> (`api_surface_freeze.rs`). La bonne classification est donc
> **`adapter-only`** (route adapter-native, sans verbe `cs` jumeau), principal
> **`operator`**. À **valider mécaniquement** contre le parser
> `cosmon-surface-canon` au moment de l'impl (B2) : si le fold refuse le couple
> (principal=operator, exposure=adapter-only), retomber sur le patron exact des
> lignes `auth/claude` (principal=tenant, exposure=adapter-only) et porter la
> distinction opérateur uniquement dans l'extracteur d'auth. Le **sceau de
> sécurité ne dépend pas** de la colonne `principal` — il dépend de
> l'extracteur (§4.2).

### 3.2 `POST /v1/admin/habilitations` — corps & réponse

**Request body** (`ProvisionHabilitationBody`) — miroir 1:1 de
`HabilitationBindingSpec` (le spec que `build_binding` valide déjà) :

```jsonc
{
  "noyau": "jordan-research",            // requis. /srv/cosmon/<noyau>/
  "habilitation_id": "jordan-research",  // optionnel ; défaut = noyau
  "oidc": {
    "issuer":   "http://oidc-mock:8444", // requis, URL absolue http(s)
    "sub":      "jordan",                // requis, distinct par binding
    "audience": "cosmon-rpp-jordan"      // requis, = aud du JWT, byte-for-byte
  },
  "scopes": ["cosmon:molecule:read",     // optionnel (T23). Validé : non-vide,
             "cosmon:molecule:write",    //   sans espace.
             "cosmon:auth:claude:write"],
  "create_noyau": true                   // optionnel, défaut true. mkdir +
                                         //   (option) `cs init` si absent.
}
```

> `phase` (défaut `Biological`) et `drain_bounds` : **non exposés** au boundary
> dans la v1 (l'opérateur les édite host-side si besoin, comme aujourd'hui le
> renderer met `drain_bounds: None`). Garde la surface minimale (tolnay) ;
> ajoutables plus tard sans rupture (champs optionnels).

**Réponse `201 Created`** (`ProvisionedHabilitation`) :

```jsonc
{
  "request_id": "req-…",
  "habilitation_id": "jordan-research",
  "noyau": "jordan-research",
  "binding_path": "<state_dir>/nucleons/jordan-research/oidc-identity.toml",
  "seal": "blake3:…",          // hash du fichier rendu (vérif, pas le secret)
  "reloaded": true,            // map rechargée in-process (pas de SIGHUP requis)
  "noyau_created": true        // true si le répertoire a été créé par cet appel
}
```

**Idempotence.** Re-POST du **même** `(iss,sub)` vers le **même** `noyau` avec
les mêmes scopes → `200 OK` (pas `201`), `binding_path` inchangé, `seal`
identique. Re-POST du même `(iss,sub)` vers un noyau **différent** → `409
Conflict` (`cross_noyau_rebind_refused`) : un `(iss,sub)` binde un seul noyau
(préserve `CrossTenantPivot` structurel). Changer le noyau d'une identité =
DELETE puis POST (geste explicite, jamais silencieux).

### 3.3 Codes d'erreur (mappés sur `ApiError`/`RppRejectReason`)

| HTTP | label | Cause |
|---|---|---|
| `401` | `admin_token_missing` | header `X-Cosmon-Admin-Token` absent |
| `401` | `admin_token_invalid` | sceau ne correspond pas (comparaison temps constant) |
| `403` | `admin_disabled` | aucun sceau admin configuré au boot → route **fermée** (fail-closed) |
| `400` | `malformed_binding` | `RenderError` (champ vide, issuer non-URL, scope avec espace) |
| `409` | `cross_noyau_rebind_refused` | `(iss,sub)` déjà bindé à un autre noyau |
| `422` | `unknown_scope` | scope hors des 11 scopes canon (validation stricte optionnelle) |
| `500` | `provision_io_error` | échec d'écriture du `.toml` ou de `mkdir` du noyau |
| `503` | `reload_failed` | le `.toml` écrit ne recharge pas (sceau/parse) → **rollback** du fichier |

---

## 4. Signatures Rust (le contrat pour B2)

> Noms canon `Habilitation*` (Phase A). Réutilise le renderer existant
> (`build_binding`, `render_oidc_identity_toml`, `HabilitationBindingSpec`,
> `RenderError`) — **ne pas réimplémenter le rendu du `.toml`**.

### 4.1 Module nouveau : `src/routes/admin.rs`

```rust
//! Operator-sealed admin provisioning routes (ADR-0022 §2.2/§6, task-…-1acd).
//! Auth is a HOST-SIDE SEAL, disjoint from the tenant OIDC chain — see
//! `AdminSeal`. These routes are the audited renderer over HTTP: they call the
//! SAME `build_binding`/`render_oidc_identity_toml` the operator used by hand,
//! write the binding into `<state_dir>/nucleons/<id>/`, and reload the map
//! in-process. They never widen the deny-by-default binding semantics; they
//! only ADD admitted `(iss,sub) → noyau` lines.

use std::sync::Arc;
use axum::{extract::{State, Path}, http::HeaderMap, response::Json, Json as JsonBody};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::ApiError;
use crate::nucleon_map::{HabilitationBindingSpec, render_oidc_identity_toml, RenderError};

#[derive(Debug, Deserialize)]
pub struct ProvisionHabilitationBody {
    pub noyau: String,
    #[serde(default)]
    pub habilitation_id: Option<String>,
    pub oidc: OidcBody,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default = "default_true")]
    pub create_noyau: bool,
}

#[derive(Debug, Deserialize)]
pub struct OidcBody {
    pub issuer: String,
    pub sub: String,
    pub audience: String,
}

#[derive(Debug, Serialize)]
pub struct ProvisionedHabilitation {
    pub request_id: String,
    pub habilitation_id: String,
    pub noyau: String,
    pub binding_path: String,
    pub seal: String,
    pub reloaded: bool,
    pub noyau_created: bool,
}

/// POST /v1/admin/habilitations
pub async fn provision_habilitation(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Result<JsonBody<ProvisionHabilitationBody>, axum::extract::rejection::JsonRejection>,
) -> Result<(axum::http::StatusCode, Json<ProvisionedHabilitation>), ApiError> {
    // 1. SEALED OPERATOR AUTH — disjoint from OIDC. Fail-closed.
    state.admin_seal.require(&headers)?;          // §4.2 — 401/403, constant-time

    // 2. Parse + validate body → HabilitationBindingSpec.
    let JsonBody(b) = body.map_err(|_| state.reject(/* MalformedJson */))?;
    let spec = HabilitationBindingSpec { /* from b, habilitation_id default = noyau */ };

    // 3. Provision (idempotent, locked) — §4.3.
    let out = state.provisioner.provision(&spec, b.create_noyau)?;

    // 4. 201 (created) or 200 (idempotent no-op).
    Ok((out.status_code(), Json(out.into_response(/* request_id */))))
}

/// GET /v1/admin/habilitations  — introspection (never returns the seal secret).
pub async fn list_habilitations(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> { /* require seal, then map.snapshot() */ }

/// DELETE /v1/admin/habilitations/{id}  — revoke (optional, B2).
pub async fn revoke_habilitation(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> { /* require seal, rm + reload */ }
```

### 4.2 Le sceau opérateur — `src/admin_seal.rs` (nouveau)

```rust
//! Host-sealed operator credential for the admin provisioning surface.
//! NEVER an OIDC JWT, NEVER mintable by the IdP, NEVER in a tenant token.
//! Provisioned at container boot (env / mounted secret). Fail-closed:
//! absent seal ⇒ the admin surface is CLOSED (403 admin_disabled).

pub struct AdminSeal {
    /// BLAKE3 of the configured admin token (we store the hash, not the token).
    expected: Option<blake3::Hash>,
}

impl AdminSeal {
    /// Built at boot from `COSMON_ADMIN_TOKEN` (env) or a mounted secret file.
    /// `None` ⇒ admin surface disabled (fail-closed).
    pub fn from_env(/* config */) -> Self { /* … */ }

    /// Guard: extract `X-Cosmon-Admin-Token`, compare CONSTANT-TIME against the
    /// sealed hash. Returns the typed rejection on any mismatch.
    pub fn require(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        let Some(expected) = self.expected.as_ref() else {
            return Err(/* 403 admin_disabled */);
        };
        let presented = headers.get("x-cosmon-admin-token")
            .and_then(|v| v.to_str().ok())
            .ok_or(/* 401 admin_token_missing */)?;
        // constant-time: hash the presented token, compare hashes.
        let got = blake3::hash(presented.as_bytes());
        if got == *expected { Ok(()) } else { Err(/* 401 admin_token_invalid */) }
    }
}
```

> **Pourquoi BLAKE3 du token et comparaison de hash** : on ne garde jamais le
> secret en clair en mémoire process plus que nécessaire, et la comparaison de
> deux digests de taille fixe est intrinsèquement temps-constant (pas de
> court-circuit sur la longueur). `blake3::Hash` implémente `==` en temps
> constant. (Alternative acceptable : `subtle::ConstantTimeEq` sur les bytes
> bruts — au choix de B2.)

### 4.3 Le service de provisioning — sérialisé, atomique, rollback

```rust
//! Single writer to the binding store. Serializes concurrent provision calls
//! (a Mutex), so two POSTs never race on write+reload. Reuses the audited
//! renderer; reload is in-process via SharedHabilitationMap::store.

pub struct Provisioner {
    state_dir: PathBuf,
    galaxies_root: PathBuf,
    map: SharedHabilitationMap,
    write_lock: tokio::sync::Mutex<()>,   // serialize writes + reload
}

pub struct ProvisionOutcome {
    pub created: bool,          // 201 vs 200 (idempotent)
    pub noyau_created: bool,
    pub binding_path: PathBuf,
    pub seal: String,
}

impl Provisioner {
    pub fn provision(&self, spec: &HabilitationBindingSpec, create_noyau: bool)
        -> Result<ProvisionOutcome, ApiError>
    {
        // let _g = self.write_lock.lock();   // (async: .await)
        // 1. Render+validate (reuse): render_oidc_identity_toml(spec)? → toml text.
        // 2. Idempotence / conflict check against current map.load():
        //      - same (iss,sub)→same noyau, same scopes ⇒ created=false (200).
        //      - same (iss,sub)→different noyau         ⇒ 409 cross_noyau_rebind_refused.
        // 3. create_noyau: if galaxies_root/<noyau> absent ⇒ mkdir (+ optional `cs init`).
        // 4. Write toml atomically (write tmp + fsync + rename) to
        //      state_dir/nucleons/<habilitation_id>/oidc-identity.toml.
        // 5. Reload: HabilitationMap::load(&state_dir)? then map.store(new).
        //      On reload error ⇒ ROLLBACK the file write, return 503 reload_failed.
        // 6. Audit event (audit.rs): HabilitationProvisioned { id, noyau, iss, sub }.
        //      NEVER log the admin token.
    }
}
```

### 4.4 Câblage dans `AppState` + `router()`

```rust
// AppState (lib.rs) gains:
pub admin_seal: Arc<AdminSeal>,
pub provisioner: Arc<Provisioner>,

// router() (lib.rs) gains:
.route("/v1/admin/habilitations", post(routes::admin::provision_habilitation)
                                  .get(routes::admin::list_habilitations))
.route("/v1/admin/habilitations/{id}", delete(routes::admin::revoke_habilitation))
```

> Ces routes sont montées **avant** le `RequestBodyLimitLayer`/CORS comme les
> autres ; elles n'ajoutent **pas** de middleware d'auth global — l'auth est
> dans le handler (`admin_seal.require`), patron identique aux routes existantes
> qui valident en tête de handler.

---

## 5. Cas limites & modes de défaillance (revue architecte)

| # | Cas | Comportement attendu |
|---|---|---|
| E1 | Aucun sceau admin au boot | route **fermée** (403 `admin_disabled`). Fail-closed : par défaut, pas d'admin surface — il faut un geste explicite de configuration au boot. |
| E2 | JWT tenant valide présenté à `/v1/admin/*` | **rejeté** : la route ne lit pas l'`Authorization: Bearer` ; sans `X-Cosmon-Admin-Token` ⇒ 401. Un JWT tenant n'ouvre **jamais** la porte (DoD). |
| E3 | Deux POST concurrents même `(iss,sub)` | sérialisés par `write_lock` ; le 2e voit l'état du 1er ⇒ idempotent (200) ou 409. Pas de `.toml` corrompu, pas de double-reload incohérent. |
| E4 | `.toml` écrit mais reload échoue (sceau/parse) | **rollback** du fichier, 503 `reload_failed`. La map en vigueur reste l'ancienne (arc-swap : les requêtes en vol gardent leur snapshot). Aucun état à demi-écrit. |
| E5 | `issuer` avec slash final divergent | `RenderError`/400 — le renderer audité **est** la défense anti-`issuer_not_pinned` drift (P2). La route hérite de cette validation gratuitement. |
| E6 | `(iss,sub)` re-bind vers autre noyau | 409 `cross_noyau_rebind_refused`. Préserve `CrossTenantPivot` : un sub = un noyau. Rebind = DELETE+POST explicite. |
| E7 | Édition rétroactive du `.toml` hors route | inchangé : `seal_intact` détecte le tamper au prochain `refresh_live_seals`. La route ne dégrade pas cette protection. |
| E8 | Création noyau : `/srv/cosmon/<noyau>/` existe déjà | `noyau_created=false`, pas d'écrasement ; on ne touche pas un workspace existant (mémoire : *« regarder la cible avant d'écraser »*). |
| E9 | `habilitation_id` collisionne un dir existant d'un autre `(iss,sub)` | rejet 409 (le dir peut porter plusieurs `oidc-identity*.toml`, mais le binding `(iss,sub)` doit rester unique) — à clarifier en B2 ; défaut conservateur = refuser. |
| E10 | Token admin dans les logs / l'audit | **interdit** : jamais loggé, jamais dans l'événement d'audit, jamais dans la réponse. Seul le `seal` (hash du *fichier rendu*, pas du token) est retourné. |
| E11 | Rotation du sceau admin | redéploiement (nouveau secret de boot). Hors scope route ; relève de la def runtime (Tenant-Demo, §7). |

---

## 6. Entrée au catalogue d'API (canon)

Le catalogue `docs/specs/cosmon-rpp-api-reference.md` (smithy) est **généré**
depuis `crates/cosmon-rpp-adapter/data/surface_events.txt` via
`cargo xtask gen-api-ref` (`just verify-api-ref` côté smithy). **B2 appendra**
ces lignes (append-only, jamais réordonner) :

```
POST /v1/admin/habilitations | task-20260616-1acd | 2026-06-16 | operator | - | adapter-only | Provision a habilitation binding (operator-sealed, host-side root-of-trust)
GET /v1/admin/habilitations | task-20260616-1acd | 2026-06-16 | operator | - | adapter-only | List provisioned habilitations (operator introspection)
DELETE /v1/admin/habilitations/{id} | task-20260616-1acd | 2026-06-16 | operator | - | adapter-only | Revoke a provisioned habilitation binding
```

> **À vérifier au fold (B2)** : que `cosmon-surface-canon` accepte
> `principal=operator` + `exposure=adapter-only` + `scope=-`. Si le parser exige
> qu'`operator` ⇒ `operator-only` (interdit sur la surface), utiliser le patron
> exact des lignes `auth/claude` : `principal=tenant`, `exposure=adapter-only`,
> `scope=-`, la distinction opérateur restant portée **uniquement** par
> l'extracteur `AdminSeal` (la sécurité ne dépend pas de la colonne `principal`).
> Lancer `cargo build` (le fold échoue sur ligne ambiguë) **puis**
> `cargo xtask gen-api-ref` **puis** `just verify-api-ref` côté smithy.

---

## 7. Hand-off Tenant-Demo (def runtime / contrat ownership)

smithy/cosmon est **autonome sur l'IMAGE** ; la **def runtime du compose** est
le contrat Tenant-Demo (à leur transmettre à chaque changement). Ce design
**introduit une nouvelle entrée de def runtime** :

- **Un secret de boot `COSMON_ADMIN_TOKEN`** (ou un fichier-secret monté) doit
  être injecté dans le conteneur de l'adapter. **Sans lui, la surface admin est
  fermée** (fail-closed, E1) — comportement sûr par défaut, donc déploiement
  non-régressif tant que le secret n'est pas posé.
- **Propriété du secret** : généré et détenu host-side (opérateur / Tenant-Demo),
  jamais commité, jamais dans l'image. Même domaine de confiance que les
  `.toml` host-side aujourd'hui.
- **Volume d'état** : la route écrit dans `<state_dir>/nucleons/…` — déjà
  monté **écriture** (l'adapter y écrit l'état tenant). Vérifier que le mount
  n'est pas `read_only` pour ce sous-arbre (cf. durcissement A2
  `read_only:true` + tmpfs — `nucleons/` doit rester writable).

> **À envoyer à Tenant-Demo** (outbox) : tableau « nouvelle var d'env
> `COSMON_ADMIN_TOKEN` (secret, host-owned) + confirmation que
> `<state_dir>/nucleons/` est writable ». Sans action Tenant-Demo, la route reste
> simplement fermée — aucune casse.

---

## 8. Plan de test bout-en-bout (DoD : « testée bout-en-bout »)

Harnais E2E (style `e2e/init-equivalence/`, `e2e/readonly-boot/` côté smithy ;
tests d'intégration côté crate) :

1. **T1 — provisioning nominal.** Boot avec `COSMON_ADMIN_TOKEN=…`. `POST
   /v1/admin/habilitations` (sceau correct) crée le noyau `jordan-research`,
   écrit le `.toml`, `201`, `reloaded:true`. Assert : fichier présent, sceau
   BLAKE3 cohérent, `map.resolve(iss,sub)` non-vide.
2. **T2 — le badge fonctionne ensuite.** Avec un JWT `(iss,sub,aud)` matchant le
   binding fraîchement créé, une route tenant (`GET /v1/molecules`) **passe**
   (admission OK). *« je crée un badge via l'API, Jordan s'authentifie avec »* —
   bout-en-bout.
3. **T3 — deny-by-default tient.** Un JWT `(iss,sub)` **non** provisionné est
   **rejeté** (admission refuse) — la route admin n'a pas affaibli le
   deny-by-default.
4. **T4 — le tenant n'atteint pas l'admin.** `POST /v1/admin/habilitations` avec
   un `Authorization: Bearer <jwt-tenant valide>` mais **sans**
   `X-Cosmon-Admin-Token` ⇒ **401**. Avec un mauvais token ⇒ **401**. (DoD :
   « jamais écrit par un JWT tenant ».)
5. **T5 — fail-closed.** Boot **sans** `COSMON_ADMIN_TOKEN` ⇒ `POST
   /v1/admin/*` ⇒ **403 `admin_disabled`**.
6. **T6 — idempotence & conflit.** Re-POST identique ⇒ `200`. Re-POST même
   `(iss,sub)` vers autre noyau ⇒ `409`.
7. **T7 — rollback reload.** Injecter un `.toml` non rechargeable (mock
   `load` qui échoue) ⇒ `503`, fichier rollbacké, map inchangée.
8. **T8 — équivalence renderer.** Le `.toml` produit par la route == le `.toml`
   produit par `cs-rpp-adapter nucleon render` pour le même spec (zéro drift de
   schéma — c'est le même `build_binding`).

---

## 9. Ce que ce design NE fait PAS (frontières)

- **Ne supprime pas** le binding host-side ni le chemin renderer CLI : la route
  est un **second canal** vers le même établi, pas un remplacement. L'opérateur
  peut toujours écrire le `.toml` à la main (court-terme, CMB 2026-06-16).
- **N'implémente pas `portée`** (l'étage scope-IdP de l'ADR-0022 §5) : futur, et
  surtout pas un arbre de fichiers. Hors scope B1.
- **Ne renomme pas** `nucleon_id` / `nucleons/` (champ wire + chemin disque) :
  dette Phase B/C (`MIGRATION-nucleon-to-habilitation.md`). Ce design **n'ajoute
  pas** de dette : il consomme les noms canon `Habilitation*` côté types et
  laisse le wire/disk tel quel.
- **Ne bascule pas vers la posture (a)** : l'IdP ne mint toujours pas la portée ;
  l'habilitation reste la police que l'IdP ne sait pas exprimer.

---

## Addendum (2026-06-17) — Portée tooling : le geste fédératif (ADR-0023 G5)

Le non-objectif B1 §9 *« n'implémente pas `portée` — futur »* est désormais
réalisé : `crates/cosmon-rpp-adapter/src/portee.rs` + la surface
`/v1/admin/federations`. C'est la **couche présentation** d'ADR-0023
(« Correspondance des niveaux », 2026-06-17), posée **par-dessus** l'établi B1 —
elle n'ajoute aucune primitive de confiance.

**Deux couches, un pont.**
- **enforcement** — une `habilitation` = `(iss, sub, aud) → noyau`, capability
  d'**une seule galaxie** (D4 *« un badge, une galaxie »*). L'`audience` épingle
  la galaxie ; une identité étrangère détient donc **N habilitations**, une par
  galaxie, sans pivot cross-tenant (un token ne porte que l'audience de *sa*
  galaxie). Substrat : la clé du `HabilitationMap` est passée de `(iss, sub)` à
  `(iss, sub, aud)` — additif, le tenant local mono-galaxie est inchangé.
- **présentation** — l'opérateur manipule une **`portée`** (la relation), jamais
  N bindings à la main. **Un geste** matérialise N habilitations atomiquement et
  les regroupe ; un manifeste `<state_dir>/portees/<id>/portee.toml` enregistre
  *quels* bindings vont ensemble (record de regroupement, **pas** une
  racine-de-confiance — les `nucleons/` scellés restent la vérité).

**Garde-fou (torvalds, ADR-0023) :** pas d'`enum LocalOrFederated`, pas de
`bool external`. Une portée est un *ensemble nommé d'habilitations ordinaires*.

### Le geste — `POST /v1/admin/federations`

Sceau opérateur identique aux routes habilitation (`X-Cosmon-Admin-Token`,
disjoint de l'OIDC tenant). `201` relation neuve, `200` extension additive.

```bash
curl -sS -X POST https://<instance>/v1/admin/federations \
  -H "X-Cosmon-Admin-Token: $COSMON_ADMIN_TOKEN" \
  -H 'content-type: application/json' \
  -d '{
    "portee_id": "casey",
    "partner": { "issuer": "https://casey.instance.peer", "sub": "casey" },
    "galaxies": ["speck", "qcd"],
    "scopes": ["cosmon:molecule:read", "cosmon:molecule:write"]
  }'
# → matérialise 2 habilitations : casey--speck (aud cosmon-rpp-speck → noyau
#   speck) et casey--qcd ; les regroupe en une relation « casey : {speck, qcd} »
```

Atomique : si une galaxie échoue (ex. issuer non-URL → `400 malformed_binding`),
toute habilitation créée *par ce geste* est révoquée — la relation atterrit
entière ou pas du tout.

### Lire, révoquer

```bash
# Vue groupée (la relation, pas N bindings)
curl -sS https://<instance>/v1/admin/federations -H "X-Cosmon-Admin-Token: $T"

# Révoquer UNE galaxie (le reste de la relation survit)
curl -sS -X DELETE https://<instance>/v1/admin/federations/casey/galaxies/speck \
  -H "X-Cosmon-Admin-Token: $T"

# Dissoudre TOUTE la relation (révoque chaque habilitation + retire le manifeste)
curl -sS -X DELETE https://<instance>/v1/admin/federations/casey \
  -H "X-Cosmon-Admin-Token: $T"
```

Réversibilité native (D6) : défaire = retirer des pins ; la sécurité *augmente*
en se défaisant (retombe en `deny-by-default`).

### Frontières (ce que G5 ne fait pas)

- **Ne change pas le cœur** : aucune résolution de token nouvelle, aucune
  décision authz, aucune route tenant. La granularité (`habilitation = 1 galaxie`
  / `portée = groupe`) est **figée ADR-0023** — l'outillage la matérialise, ne la
  rédébat pas.
- **Pas de verbe `cs`** : surface opérateur, `adapter-only` (comme les routes
  habilitation) — appel via `curl`/outillage Tenant-Demo, jamais le CLI tenant.
- **Audience = convention** `cosmon-rpp-<galaxy>` (même règle que
  `infer_expected_noyau_from_aud`). Le manifeste n'est pas scellé : il regroupe,
  il n'autorise pas.

---

## Sources (lecture directe, ce repo)

- `crates/cosmon-rpp-adapter/src/nucleon_map.rs` — `OidcIdentity`, `OidcClaims`,
  `ScopesGrant`, `Resolved`, `HabilitationMap::load` (sceau BLAKE3),
  `SharedHabilitationMap` (arc-swap/SIGHUP), `build_binding` /
  `render_oidc_identity_toml` (renderer P2, `:591-604` root-of-trust comment).
- `crates/cosmon-rpp-adapter/src/lib.rs` — `router()`, `AppState`.
- `crates/cosmon-rpp-adapter/src/routes/{noyaux,molecules,auth_me}.rs` — patron
  de handler, `extract_bearer`, `authorise_scope`, `effective_scope_decision`.
- `crates/cosmon-rpp-adapter/src/auth/scopes.rs` — les 11 scopes (aucun admin).
- `crates/cosmon-rpp-adapter/src/admission.rs` — `OPERATOR_ONLY_VERBS`.
- `crates/cosmon-rpp-adapter/data/surface_events.txt` — canon append-only, format
  7 champs, `operator-only` interdit sur la surface gelée.
- `crates/cosmon-rpp-adapter/docs/MIGRATION-nucleon-to-habilitation.md` — Phase A
  landée (`Habilitation*` canon), wire/disk Phase B/C en attente.
- smithy `docs/adr/0022-rationalisation-ontologie-cosmon.md` §2.2, §5, §6, §D1.
- smithy `docs/specs/cosmon-rpp-api-reference.md` — catalogue généré.
