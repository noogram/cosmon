# ADR-035: Cross-Galaxy Edges

## Status

Proposed (2026-04-13) — Phase 1 implemented (2026-04-25, see
`Implementation status` below); Phases 2–4 still design-only.

Derived from deliberation `delib-20260413-fb7b` (trajectory deliberation):
synthesis §D4 (cross-universe wiring), §C2 (P3 prerequisite), §C3
(content-addressed galaxies); adversary §3 (failure modes).

**v0 ships SAME-galaxy edges only.** This ADR defines the design for
cross-galaxy edges so the type system is prepared, but no cross-galaxy
code path is implemented until the prerequisites land.

## Implementation status

| Phase | Status | Landed in | Notes |
|-------|--------|-----------|-------|
| Phase 1 — alias-addressed edges | ✅ implemented | task-20260425-04ff (2026-04-25) | `--blocked-by`/`--blocks` accept `<alias>:<mol_id>` and `<alias>@<mol_id>`; resolver chain `cosmon-registry → ~/.cosmon/galaxy-aliases.toml → /srv/cosmon/<alias>/`; `cs deps --json` surfaces the edge with a resolution status; one-writer-per-galaxy preserved (no remote mutation, only local typed link). |
| Phase 2 — content-addressed identity | 📐 design | — | `GalaxyHash = BLAKE3(genesis_blob)`; survives renames. Requires Neurion `universes` table. |
| Phase 3 — remote completion receipts | 📐 design | — | Signed `CompletionReceipt` (HMAC-BLAKE3); `cs wait` walks remote state. Depends on ADR-034 retraction tombstones. |
| Phase 4 — CT-logged receipts | 📐 design | — | Append-only log + monitor/auditor split (RFC 9162). |

Delta vs full design (Phase 1 only):
- Identity is the human-readable alias, not the genesis hash. Renaming
  a galaxy breaks alias-only edges; this is acceptable for the manual
  cross-session workflow that triggered the implementation
  (`mailroom → tenant-demo` deliberation handoff) and will be replaced
  by content-addressing in Phase 2.
- Symmetry is asymmetric: only the source galaxy records the link.
  The remote `CrossGalaxyBlockedBy` reciprocal is not filed (one-writer
  ADR-052). Deps walking is direct only — no transitive cross-galaxy
  traversal yet.
- Reachability is best-effort: `cs nucleate` warns on stderr if the
  target galaxy/molecule cannot be located, but persists the edge
  anyway. ADR-035 §6's `StaleEdge` event is not emitted yet — the
  `CrossGalaxyResolution::GalaxyUnknown` / `MoleculeMissing` JSON
  output of `cs deps` is the Phase 1 stand-in.

## Context

Cosmon molecules today live in a single galaxy — a single `.cosmon/`
directory tree rooted in one project. All `MoleculeId` references,
`MoleculeLink` edges (Blocks, BlockedBy, DecayedFrom, DecayProduct,
Entangled), and DAG operations assume that the target molecule is
resolvable by walking the local `.cosmon/state/` filesystem.

This is correct for v0 but insufficient for the multi-project future:

1. **Cross-project dependencies.** A molecule in `cosmon` may need to
   block on a molecule in `noogram-engine` — both are cosmon-managed
   galaxies on the same machine.
2. **Federated agent fleets.** A pilot running molecules across multiple
   repos needs typed edges, not free-text `Entangled` links with
   human-interpreted semantics.
3. **Retraction tombstones.** ADR-034 introduces `Retraction` as a
   terminal state. A cross-galaxy edge pointing at a retracted molecule
   must resolve to a valid terminal state (`Annihilated`), not a dangling
   reference. This means **P3 (Retraction) is a prerequisite for shipping
   cross-galaxy edges** — confirmed by the panel (synthesis §C2).

### Why content-addressed galaxy identity

The adversary (synthesis §3, delib-fb7b) identified the killer failure
mode: **galaxy rename/move breaks all `@old-galaxy` edges.** If galaxy
identity is name-based (`@cosmon`, `@noogram-engine`), renaming a
directory or moving a repo invalidates every cross-galaxy reference.

Content-addressing solves this: a galaxy's canonical identity is a hash
of its genesis state (the immutable seed created by `cs init`). Names
are human-readable aliases resolved by Neurion. The edge itself carries
the content hash; the name is a routing hint that can change without
invalidating the edge.

This is a direct application of ADR-011 (Content-Identity Principle):
identity = f(content), not f(location).

## Decision

### 1. Galaxy identity: content-addressed

A **galaxy** is a cosmon project instance — the scope of a single
`.cosmon/` directory. Each galaxy has a canonical identity:

```
GalaxyId = BLAKE3(genesis_blob)
```

Where `genesis_blob` is the serialized content of the `.cosmon/config.toml`
at `cs init` time (project name, creation timestamp, initial project ID).
The hash is computed once during `cs init`, stored in `.cosmon/config.toml`
as `galaxy_hash`, and never recomputed.

**Display format:** the first 16 hex characters of the BLAKE3 hash
(64 bits — collision-resistant for any plausible fleet size).

```toml
# .cosmon/config.toml
[project]
name = "cosmon"
id = "cosmon-a1b2"
galaxy_hash = "3f7a9c2e1d4b8f06"  # BLAKE3 truncated to 16 hex chars
created_at = "2026-04-01T00:00:00Z"
```

### 2. Cross-galaxy molecule reference: `mol@galaxy` syntax

A **qualified molecule reference** extends `MoleculeId` with an optional
galaxy qualifier:

```
<molecule-id>@<galaxy-ref>
```

Where `<galaxy-ref>` is either:
- A galaxy hash (canonical, portable): `task-20260413-d1b3@3f7a9c2e1d4b8f06`
- A galaxy name (human-friendly alias, resolved via Neurion): `task-20260413-d1b3@cosmon`

**Parsing rules:**
- No `@` → local molecule (same galaxy). This is the v0-only path.
- `@<hex16>` → canonical galaxy hash reference.
- `@<name>` → Neurion-resolved alias. Fails if Neurion is unavailable
  and no cached resolution exists.

**Type representation:**

```rust
/// A reference to a molecule, possibly in another galaxy.
pub struct QualifiedMoleculeRef {
    /// The molecule's local ID within its galaxy.
    pub molecule_id: MoleculeId,
    /// The target galaxy. `None` = local (same galaxy).
    pub galaxy: Option<GalaxyRef>,
}

/// How a galaxy is referenced in an edge.
pub enum GalaxyRef {
    /// Content-addressed hash (canonical, portable).
    Hash(GalaxyHash),
    /// Human-readable name (resolved via Neurion at edge creation time;
    /// the resolved hash is stored alongside for offline resolution).
    Named {
        name: String,
        resolved_hash: GalaxyHash,
    },
}
```

When a `Named` galaxy ref is created, the resolver immediately looks up
the galaxy hash via Neurion and stores both. The `name` is a convenience;
the `resolved_hash` is authoritative. If the name changes, the hash
still resolves.

### 3. Cross-galaxy link variants

`MoleculeLink` gains cross-galaxy awareness by accepting
`QualifiedMoleculeRef` instead of bare `MoleculeId` in its edge targets:

```rust
pub enum MoleculeLink {
    // ... existing variants unchanged for v0 ...

    /// Cross-galaxy blocking dependency (future, post-v0).
    CrossGalaxyBlocks {
        target: QualifiedMoleculeRef,
    },
    CrossGalaxyBlockedBy {
        source: QualifiedMoleculeRef,
    },
}
```

**v0 constraint:** the existing `Blocks` / `BlockedBy` variants continue
to accept bare `MoleculeId` (same-galaxy only). Cross-galaxy variants are
defined in the type system but gated behind a feature flag
(`cross-galaxy-edges`) that is OFF in v0. Any attempt to create a
cross-galaxy link without the feature flag returns
`Error::CrossGalaxyNotSupported`.

### 4. Retraction tombstones as terminal state (prerequisite: ADR-034)

When a cross-galaxy edge resolves its target, the target may be in one of
these states:

| Target state | Edge resolution |
|---|---|
| `Pending`, `Running`, `Active` | Edge is live — `cs wait` polls |
| `Completed` | Edge is satisfied — dependent can proceed |
| `Collapsed` | Edge is satisfied (terminal) — dependent sees failure reason |
| `Annihilated` (ADR-034) | Edge resolves to **tombstone** — content hash + reason + timestamp, no payload |
| **Unreachable** (galaxy offline) | Edge is `stale` — timeout triggers `StaleEdge` event |

The tombstone is the critical piece: a retracted molecule leaves behind
a minimal record (molecule ID, galaxy hash, annihilation reason,
annihilation timestamp) that cross-galaxy edges can resolve without
accessing the full molecule state. This is why ADR-034 must land before
cross-galaxy edges ship.

**Tombstone record (persisted in local galaxy's edge cache):**

```rust
pub struct RemoteTombstone {
    pub molecule_id: MoleculeId,
    pub galaxy_hash: GalaxyHash,
    pub reason: String,
    pub annihilated_at: DateTime<Utc>,
    pub content_hash: ContentHash,  // hash of the original molecule state at annihilation time
}
```

### 5. Signed completion receipts

Cross-galaxy edges introduce a trust boundary: galaxy A claims molecule
M is `Completed`, but galaxy B has no way to verify this claim
independently. Without verification, a compromised or buggy galaxy can
inject fake `done` signals, causing downstream molecules to dispatch on
lies (adversary §3, failure mode "Adversarial galaxy injecting fake
done").

**Completion receipt:**

```rust
pub struct CompletionReceipt {
    /// The molecule that completed.
    pub molecule_id: MoleculeId,
    /// The galaxy where completion occurred.
    pub galaxy_hash: GalaxyHash,
    /// Terminal state reached.
    pub terminal_state: TerminalState,
    /// BLAKE3 hash of the molecule's final `state.json`.
    pub state_hash: ContentHash,
    /// BLAKE3 hash of the molecule's `events.jsonl` at completion time.
    pub events_hash: ContentHash,
    /// Timestamp of completion.
    pub completed_at: DateTime<Utc>,
    /// HMAC-BLAKE3 signature over the above fields, keyed by the
    /// galaxy's signing secret.
    pub signature: ReceiptSignature,
}
```

**Signing key:** each galaxy generates an HMAC signing key at `cs init`
time, stored in `.cosmon/secrets/galaxy.key` (gitignored). The key never
leaves the galaxy. Verification is done by the consuming galaxy, which
must have obtained the signing galaxy's public verification material
through an out-of-band trust establishment (e.g., Neurion registration).

**v0 scope:** receipts are generated locally for all completions
(self-signed, stored in `.cosmon/state/receipts/`). Cross-galaxy
verification is post-v0. This means the receipt infrastructure ships
incrementally: first the data model and local signing, then remote
verification.

**Trust model:**

| Trust level | Description | When |
|---|---|---|
| **Self-signed** (v0) | Galaxy signs its own receipts. No external verification. | Now |
| **Neurion-attested** (v1) | Neurion holds galaxy public keys. Consuming galaxy verifies signature via Neurion lookup. | Post-v0, with Neurion universes table |
| **CT-logged** (v2) | Receipts are logged to a Certificate Transparency-style append-only log. Monitors can audit. | Future, per PoPE spec |

### 6. Resolution protocol (post-v0)

When `cs wait` or `ready_frontier` encounters a cross-galaxy edge:

1. **Resolve galaxy:** look up `GalaxyRef` → filesystem path. First check
   Neurion; fall back to local worktree cache.
2. **Read remote state:** walk to the remote galaxy's
   `.cosmon/state/fleets/*/molecules/<id>/state.json`. Read terminal
   state or current status.
3. **Verify receipt:** if terminal, verify the `CompletionReceipt`
   signature against the galaxy's registered public key.
4. **Cache result:** store resolved state + receipt in local
   `.cosmon/state/edges/remote/<galaxy-hash>/<molecule-id>.json` with
   TTL.
5. **Return edge status:** `Satisfied`, `Pending`, `Stale` (timeout),
   or `Tombstone` (retracted).

**Failure modes and mitigations (from adversary §3):**

| Failure | Mitigation |
|---|---|
| Network partition / galaxy offline | Timeout → `StaleEdge` event; cache last-known state with TTL |
| Galaxy rename/move | Content-addressed identity (§1) — hash doesn't change |
| Neurion downtime | Worktree caches last-known Neurion snapshot; degrade with warning. **Neurion NOT on critical path of `cs evolve`** |
| Fake `done` injection | Signed completion receipts (§5) |
| GDPR-delete of referenced molecule | Retraction tombstone (§4) — edge resolves to valid terminal state |

### 7. Neurion integration (minimal)

Neurion gains one table in its v0 universes work (per synthesis §"Neurion
change list"):

```sql
CREATE TABLE universes (
    galaxy_hash TEXT PRIMARY KEY,   -- BLAKE3 truncated 16 hex
    name        TEXT NOT NULL,      -- human-readable alias
    root_path   TEXT NOT NULL,      -- filesystem path to .cosmon/
    public_key  BLOB,               -- HMAC verification key (NULL until v1)
    registered_at TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
```

`cs init` registers the new galaxy in Neurion (if available).
`cs galaxy resolve <name-or-hash>` looks up galaxy metadata. This is the
extent of Neurion's role in v0 — **no cross-universe edge resolution
logic in Neurion itself.**

## What this ADR does NOT decide

1. **Implementation timeline.** Cross-galaxy edge code is post-v0.
2. **Multi-machine federation.** This design assumes galaxies are on the
   same filesystem (or reachable via local mount / rclone sync). Network
   federation (gRPC, MCP relay) is a separate ADR.
3. **Cross-galaxy DAG scheduling.** `cs run` operates on a single galaxy's
   DAG. Cross-galaxy DAG orchestration (a meta-runtime) is a separate
   design problem.
4. **Key rotation.** The signing key is generated once. Rotation protocol
   is deferred to v1.
5. **Conflict resolution.** Two galaxies with conflicting state for the
   same cross-galaxy edge. Deferred — v0 is single-writer-per-galaxy.

## Consequences

### Positive

- **Type system is prepared.** `QualifiedMoleculeRef` and `GalaxyHash` can
  land in `cosmon-core` without any cross-galaxy code path being active.
  Future work extends, not rewrites.
- **Content-addressed identity eliminates rename fragility.** Galaxy hash
  survives directory renames, repo moves, and Neurion registry changes.
- **Retraction tombstones compose with cross-galaxy edges.** A retracted
  molecule is a valid terminal state, not a dangling reference.
- **Signed receipts establish a trust primitive.** Even self-signed v0
  receipts create the audit trail that PoPE (Proof of Provable Effort)
  will eventually consume.
- **Same-galaxy edges are unaffected.** v0 behavior is unchanged; the
  feature flag keeps cross-galaxy paths inert.

### Negative

- **Type surface area grows.** `GalaxyHash`, `GalaxyRef`,
  `QualifiedMoleculeRef`, `CompletionReceipt`, `RemoteTombstone` are new
  types, even if unused in v0 code paths.
- **`cs init` gains new responsibilities.** Computing the genesis hash
  and generating the signing key adds complexity to project initialization.
- **Neurion dependency for name resolution.** Named galaxy refs require
  Neurion. Mitigated by the `resolved_hash` fallback stored at edge
  creation time.

### Neutral

- **No behavioral change in v0.** All cross-galaxy code paths are behind
  a feature flag. This ADR is purely a design commitment.

## Prerequisites (must land before implementation)

| Prerequisite | ADR | Status |
|---|---|---|
| Retraction molecule kind + `Annihilated` terminal state | ADR-034 | Proposed |
| `cosmon-hash` crate (BLAKE3 hashing primitives) | — | Planned (P1a in trajectory) |
| Neurion `universes` table | — | Planned (trajectory §"Neurion change list") |

## Implementation plan (post-v0, when prerequisites land)

**Phase 1 — Types only (can land with v0):**
- `GalaxyHash` newtype in `cosmon-core::id`
- `galaxy_hash` field in `.cosmon/config.toml` (computed by `cs init`)
- `QualifiedMoleculeRef` type (parsed but only `None` galaxy accepted)
- `CompletionReceipt` and `RemoteTombstone` structs (defined, not used)

**Phase 2 — Local receipts:**
- `cs init` generates signing key in `.cosmon/secrets/galaxy.key`
- `cs complete` / `cs collapse` emit `CompletionReceipt` to
  `.cosmon/state/receipts/`
- Self-signed, no verification

**Phase 3 — Cross-galaxy resolution:**
- `MoleculeLink::CrossGalaxyBlocks` / `CrossGalaxyBlockedBy` enabled
- `cs wait` gains remote resolution (§6)
- Neurion `universes` table lookup for galaxy path resolution
- Remote state caching with TTL
- Receipt signature verification via Neurion-registered public keys

**Phase 4 — CT-logged receipts (PoPE):**
- Append-only receipt log
- Monitor / auditor separation (per Certificate Transparency RFCs)
- Integration with PoPE v0 spec

## References

- `delib-20260413-fb7b/synthesis.md` — §C2, §C3, §D4
- `delib-20260413-fb7b/responses/adversary.md` — §3 (failure modes)
- `delib-20260413-fb7b/responses/wheeler.md` — `mol@galaxy` typed edges
- ADR-011 — Content-Identity Principle
- ADR-034 — Anti-Molecule (Retraction)
- THESIS.md Part IV (Domain Model), Part V (Vocabulary)
- RFC 6962 / 9162 — Certificate Transparency (future PoPE reference)
