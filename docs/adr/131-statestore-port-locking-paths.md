# ADR-131 — StateStore port: what belongs on the port vs. what stays welded to the FileStore adapter (locking & paths)

**Status:** Accepted (Phase-A landed — path concerns promoted, long tail swept;
the locking port is *specified here and deferred* to its own mechanical PR)
**Date:** 2026-06-23
**Decider:** Noogram
**Parent work:** `task-20260622-7072` D1 (delib-20260622-187a F-ARCH-6 — the
"decorative port" pathology), which made `StateStore` a real seam
(`Context::store` / `store_at`) and routed the high-traffic commands through it.
**Implementing molecule:** `task-20260623-5621` (this note + the long-tail sweep).

**Binds:**
- `crates/cosmon-state/src/lib.rs` — the `StateStore` port.
- `crates/cosmon-filestore/src/lib.rs` — the JSON `FileStore` adapter.
- `crates/cosmon-cli/src/cmd/mod.rs` — the single construction seam
  (`Context::store` / `store_at` / `open_store`).

---

## Context

After the high-traffic commands started routing persistence through
`&dyn StateStore`, a **long tail** of cosmon-cli commands still imported
`cosmon_filestore::FileStore` concretely — not out of laziness, but because
they call **FileStore methods that are not on the port**. The question this ADR
settles: *which of those methods are genuinely storage concerns that belong on
the port, and which are adapter-internal mechanisms that should stay welded?*

The candidate FileStore-only surface (after `molecule_dir` was already promoted
in D1) was exactly four inherent methods:

| Method | Shape | Used by |
|--------|-------|---------|
| `project_root()` | `-> Option<PathBuf>` | `thaw`, `patrol` |
| `with_fleet_lock<F,T,E>(f)` | generic closure | `complete`, `evolve`, `done`, `tackle`, `resurrect`, `heartbeat`, `nucleate` |
| `with_trunk_lock<F,T,E>(cmd, f)` | generic closure | `stitch`, `done` |
| `acquire_trunk_lock(cmd)` | `-> TrunkLockGuard` (RAII) | `done` |

The two groups (`project_root` vs. the three lock methods) are **not the same
kind of thing**, so they get different answers.

## Options Considered

### Option A — Weld the whole long tail to FileStore (status quo, do nothing)

Accept that `project_root` and the lock methods are "filesystem plumbing" and
leave every long-tail command importing `cosmon_filestore::FileStore`.

- **Pros:** zero churn; the crash-recovery core stays byte-identical.
- **Cons:** the "swap the JSON backend = change one method" promise stays false
  for ~25 commands; `project_root` (a storage-location concern every backend
  answers) gets miscategorised as adapter-internal; the decorative-port
  pathology (F-ARCH-6) is only half-cured. **Rejected** — it mistakes the
  *mechanism* (flock, a worktree path) for the *concern* (atomic RMW, "where is
  the store rooted").

### Option B — Build a full object-safe Locking + Paths port now, convert everything in this PR

Promote `project_root`, and replace the generic `with_fleet_lock` closures with
an object-safe RAII-guard port, converting all ~23 lock call sites in the same
change.

- **Pros:** maximal consistency; `FileStore` vanishes from every command's
  production path in one stroke.
- **Cons:** the ~23 closure→guard rewrites land in `done`/`evolve`/`tackle`/
  `complete` — the highest-stakes, most-tested crash-recovery commands — each a
  *control-flow* change (closure scope → lexical scope), not a type swap.
  Riding that on top of a 17-file mechanical import sweep blows the 400-line PR
  ceiling, makes the diff un-bisectable, and churns the reactor core for a port
  that has exactly one implementation until a second backend exists.
  **Rejected for this PR** — correct destination, wrong vehicle.

### Option C — Promote the path concern now; specify the locking port but defer its conversion (CHOSEN)

Split by concern: `project_root` is promoted to the port immediately (it is the
twin of `molecule_dir`), the lock-free long tail is swept through the seam, and
the locking port is *designed and accepted in principle here* but its
implementation + 8-command conversion is a **separate mechanical PR**.

- **Pros:** each piece is independently correct and reviewable; the
  crash-recovery core is left un-churned; the gap is *named and bounded*, not
  silent.
- **Cons:** until the follow-up lands, a reader sees an inconsistency — most
  commands route through `&dyn StateStore`, eight still hold a `FileStore`.
  Mitigated by this ADR + a `temp:warm` follow-up bead.

### Option D (sub-decision) — How to reach a foreign store without a Context

Two reads address a *foreign* state directory directly (a cross-galaxy molecule
via `cs deps`; a captured session via `cs diverge`) and have no `Context` in
scope. Considered: **(D1)** thread `ctx` through the pure resolvers, vs.
**(D2)** a Context-free construction helper.

- D1 ripples `ctx` through otherwise-pure resolver signatures (`deps`,
  `diverge`'s clause builder) purely to satisfy a lexical rule, for state
  whose backend is *not this invocation's to choose*. **Rejected.**
- D2 (`cmd::open_store(path)`, which `Context::store_at` also delegates to)
  keeps FileStore construction at one point while serving both entry shapes.
  **Chosen.**

## Decision

Take **Option C** (with sub-decision **D2**).

### Decision 1 — `project_root` IS a storage concern → promote to the port (DONE)

"Where is this store rooted?" is a property of *the store*, not *the
filesystem*. A future SQLite/Dolt backend running inside a worktree still
answers it — `cs done` merges that worktree, worker repo paths resolve relative
to it. It is the exact twin of `molecule_dir` (D1's precedent), so:

- Added `fn project_root(&self) -> Option<PathBuf>` to `StateStore`, default
  `None`; `FileStore`'s trait impl delegates to its inherent method.
- `thaw` and `patrol` now build the adapter through `Context::store_at` and
  depend on `&dyn StateStore`. `patrol`'s seven sweep helpers retyped their
  `store: &FileStore` parameter to `&dyn StateStore`.

### Decision 2 — Locking is a *storage-atomicity* concern; its object-safe port is specified but deferred (DEFERRED)

The instinct "locking is filesystem plumbing, leave it welded" (Option A) is
**wrong**: every backend needs *atomic read-modify-write of fleet state*.
FileStore provides it with `flock`; a SQLite backend would with
`BEGIN IMMEDIATE`. The differing *mechanism* does not make the *need*
adapter-internal — it is the textbook hexagonal case where **the port expresses
the need and each adapter expresses the mechanism**. Three facts decide *how*
and *when*:

1. **The generic closure form is not object-safe.** `with_fleet_lock<F,T,E>`
   cannot sit on a `dyn`-compatible trait.
2. **An RAII-guard form IS object-safe — and the closures only ever touch port
   methods.** All ~23 production closure bodies call `s.load_fleet()` /
   `s.save_fleet()`, so the closure exists *only* to bound the lock lifetime,
   which a guard does just as well:
   ```rust
   trait StateStore {
       /// Exclusive guard over fleet-state RMW; releases on drop. Default:
       /// a no-op guard. FileStore returns its flock guard; a DB adapter a txn.
       fn lock_fleet(&self) -> Result<Box<dyn FleetGuard + '_>, CosmonError> {
           Ok(Box::new(()))
       }
       fn lock_trunk(&self, cmd_hint: &str)
           -> Result<Box<dyn TrunkGuard + '_>, CosmonError> { /* default no-op */ }
   }
   ```
   `acquire_trunk_lock` already returns an RAII guard, so the trunk half is
   nearly mechanical. Call shape: `store.with_fleet_lock(|s| { … })` becomes
   `let _g = store.lock_fleet()?; …`.
3. **The lock-coupled set is the crash-recovery core** (Option B's con). The
   conversion is therefore a dedicated PR (follow-up bead `temp:warm`). Until it
   lands, the lock-coupled commands keep their concrete `FileStore`
   construction — now the *only* sanctioned reason a command touches
   `cosmon_filestore::FileStore` in production.

### What actually shipped in this molecule

- **`project_root` promoted** (Decision 1); `thaw`, `patrol` unwelded.
- **Long-tail sweep**: `status`, `freeze`, `stuck`, `resume`, `teardown`,
  `quench`, `note`, `interaction`, `migrate`, `notarize`, `deps`, `verify`,
  `verify_graph`, `await_operator` now build through the seam; their
  `cosmon_filestore::FileStore` production imports are gone.
- **One construction seam, two entry points** (sub-decision D2): `Context::store`
  / `store_at` for this-invocation's backend, `cmd::open_store(path)` for the
  foreign-store reads (`cs deps` cross-galaxy resolution, `cs diverge`). Both
  funnel through the same one-line FileStore construction.

## Out of scope (and why)

- **`PresenceStore`** (`cs presence`, `cs whisper`) is a *different port* with
  its own `upsert` / `scan` / `log_path` / `snapshot_path` / `seek_path`
  surface. Not the `StateStore` port; not in this ADR's perimeter. If it merits
  the same treatment, that is its own ADR.
- **The locking-port implementation** — Decision 2, deferred by design.

## Consequences

- **Positive.** The "swap the JSON backend = change one method" promise is now
  true for every command *except* the lock-coupled core, and the design that
  closes that last gap is specified, not hand-waved. `FileStore` appears in
  exactly one production module (`cmd/mod.rs`) plus the deferred lock set.
- **Risk / negative.** Until the follow-up lands, the codebase is *visibly
  inconsistent* — most commands route through `&dyn StateStore`, eight still
  hold a `FileStore`. If the follow-up bead is dropped, the inconsistency
  ossifies and a future reader may cargo-cult a fresh `FileStore::new` into a
  new command, re-seeding the decorative-port pathology. The bead + this ADR's
  §Decision-2 are the mitigation; the `open_store` doc comment is the inline
  tripwire ("the FileStore name must not reappear outside this seam").
- **Risk / migration.** The deferred closure→guard conversion is a control-flow
  change in the crash-recovery core; it must be landed under its own tests
  (lock-contention, RMW atomicity) and bisectable in isolation, never folded
  into an unrelated change.

## Verification

`cargo check/test/clippy/fmt` clean. The object-safety of the (future) guard
port is provable the same way `StateStore` already proves it — the
`state_store_is_object_safe` test in `cosmon-state`.
