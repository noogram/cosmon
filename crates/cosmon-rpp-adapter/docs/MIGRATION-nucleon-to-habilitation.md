# Migration `nucléon` → `habilitation` (ADR-0022 D4)

**Status:** Phase A landed (this branch). Phases B/C planned, not yet executed.
**Origin:** smithy ADR-0022 §Décision D4 + §6 (Accepted 2026-06-16).
**Discipline:** smithy *planifie* ; cosmon-ward *implémente*. This doc is the
cosmon-side execution plan for the rename.

---

## Why phased, not big-bang

A single atomic rename of every `nucleon*` token across the workspace (87 Rust
files, ~1000 occurrences) is **unsafe** because three distinct surfaces carry
different risk:

| Surface | Examples | Risk |
|---|---|---|
| **Rust type names** (PascalCase) | `NucleonMap`, `NucleonId`, `SharedNucleonMap` | **none** — newtypes serialize transparently or are never serialized; renaming the type does not touch any wire/disk byte |
| **Serialized field names** (snake_case) | `pub nucleon_id: …` in `OidcIdentity`, `audit`, `admission`, matrix `types` | **high** — these are JSON-API + on-disk TOML field names. Renaming changes the wire/disk format → breaks deployed instances and existing on-disk state |
| **On-disk paths** | `.cosmon/state/nucleons/<id>/oidc-identity.toml` | **high + load-bearing** — this is the §8j **posture-(b) root of trust** (operator-written, BLAKE3-sealed). Renaming the directory breaks admission until every instance's state volume is migrated |

The safe strategy separates the harmless type-name flip (do now) from the
wire/disk-format change (do later, behind read-compat), and never touches the
posture-(b) binding semantics.

---

## Phase A — type-name rename (DONE, this branch)

**Scope:** `crates/cosmon-rpp-adapter` PascalCase type identifiers.

- `NucleonId` → `HabilitationId`
- `NucleonMap` → `HabilitationMap`
- `SharedNucleonMap` → `SharedHabilitationMap`
- `NucleonMapBuilder` → `HabilitationMapBuilder`
- `NucleonBindingSpec` → `HabilitationBindingSpec`

The canonical names are now `Habilitation*`. Backward-compat is a thin shim of
`#[deprecated] pub type Nucleon* = Habilitation*;` aliases at the bottom of
`nucleon_map.rs`, re-exported `#[allow(deprecated)]` from `lib.rs` for any
out-of-workspace / not-yet-migrated consumer.

**Invariants preserved (verified):**
- No serialized field name changed (`nucleon_id` field kept verbatim).
- No on-disk path changed (`.cosmon/state/nucleons/` kept verbatim).
- The module file is still `nucleon_map.rs` and the module path
  `cosmon_rpp_adapter::nucleon_map` still resolves (file rename deferred to B).
- Posture-(b) binding unchanged: 1→1 sealed `(iss,sub)→noyau`, BLAKE3 seal,
  SIGHUP reload, operator-only host-side write. `v1_binding_scopes`,
  `admission_test`, `v1_noyaux`, `v1_auth_me`, `tenant_isolation_test` all green.

**Gate:** `cargo check -p cosmon-rpp-adapter --tests` + full
`cargo test -p cosmon-rpp-adapter` green; `cargo check -p cosmon-thin-cli
--tests` green (external importer).

### Phase A.2 — remaining crates (not yet done)
Apply the identical PascalCase rename + deprecated-alias shim to:
- `crates/cosmon-matrix-tick` — its **own** `NucleonId` newtype
  (`types.rs:21`) and `NucleonMap` (`nucleon_map.rs`). Independent of the
  adapter's types; same wire-neutral technique applies.
- Any other crate defining (not just importing) a `Nucleon*` type.

Importers (`cosmon-thin-cli`, `cosmon-cli`, `cosmon-api`, `cosmon-state`,
`cosmon-core`) flip to the canonical name in the same sweep; the deprecated
aliases are the safety net while the sweep is in flight.

---

## Phase B — wire/disk field + path migration (planned, dual-read)

This is the format-breaking part. Execute only with a **read-compat window**.

1. **Serialized fields** `nucleon_id` → `habilitation_id`:
   rename the Rust field AND add `#[serde(alias = "nucleon_id")]` so **new
   writes** emit `habilitation_id` while **old reads** (existing on-disk TOML,
   in-flight JSON from older clients) still deserialize. Touch every
   `#[derive(Deserialize/Serialize)]` struct with a `nucleon_id` field:
   `OidcIdentity`, `audit`, `admission`, matrix `types`, `cluster`, `whispers`,
   `motion`, `token_meter`, event_v2 schemas.
2. **On-disk directory** `.cosmon/state/nucleons/` → `.cosmon/state/habilitations/`:
   loader reads the new path first, **falls back** to the legacy `nucleons/`
   path when the new one is absent. Keep the fallback for the full deprecation
   window. **The directory rename is operator/runtime-owned, not image-owned**
   (see Tenant-Demo hand-off below).
3. **API response contracts**: the `/v1/` JSON surface that emits `nucleon_id`
   gains the renamed field; bump the API-reference catalogue
   (smithy `docs/specs/cosmon-rpp-api-reference.md`, `just verify-api-ref`).
   Coordinate with any deployed client (CLI ships in-repo, lockstep; AWS
   instances need redeploy before the alias is dropped).

**Do not start Phase B until** every instance runs a Phase-A.2 binary (canonical
types) and the client/API consumers are inventoried.

---

## Phase C — drop the compat shim (planned)

Once (a) all crates use canonical names, (b) all deployed instances run a
Phase-B binary, (c) every on-disk state volume has migrated `nucleons/` →
`habilitations/`, and (d) the deprecation window has elapsed:

- delete the `#[deprecated] pub type Nucleon*` aliases and the
  `#[allow(deprecated)]` re-export;
- remove the `#[serde(alias = "nucleon_id")]` read-compat;
- rename the module file `nucleon_map.rs` → `habilitation_map.rs` and drop the
  legacy on-disk path fallback.

---

## Adjacent debt (ADR-0022 §6.ii) — `tenant`

`tenant` / `multi-tenant` / `CrossTenantPivot` were **explicitly rejected** by
ADR-063 but re-entered the code via ADR-080 (44 occurrences in `nucleon_map.rs`
alone). Fold a `tenant` → `noyau`/`habilitation` vocabulary cleanup into Phase
A.2/B (type names + comments are wire-neutral; any serialized `tenant` field
follows the Phase-B alias discipline). Tracked separately from the headline
rename so the security-critical binding tests stay the gate.

---

## Hand-off to Tenant-Demo (runtime / compose / ownership)

Per the smithy↔Tenant-Demo contract (image autonomous; runtime def NOT):

- **Phase B directory rename** (`.cosmon/state/nucleons/` →
  `.cosmon/state/habilitations/`) touches the **persistent state volume mount**
  in the compose / instance provisioning. That is **runtime def**, owned by
  Tenant-Demo. Transmit the path change + the loader's read-fallback contract so the
  volume layout and any `provision-noyau.sh` / seed tooling migrate in lockstep.
- The **image** (binary) change is smithy/cosmon-autonomous.
- Operator-written sealed `oidc-identity.toml` files are the §8j root of trust:
  their migration is an **operator gesture**, scripted with the read-fallback as
  the safety net, never an in-flight auto-rewrite.
