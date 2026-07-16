# ADR-124 — Tenant Bounded Drain: `POST /v1/molecules/{id}/run` exits the §5.1 closed list

**Status:** accepted
**Date:** 2026-06-11
**Decider:** Noogram (via delib-20260610-9a0c, avatar-surface)
**Authoring task:** `task-20260610-56c4` (B2 — verbe run/do côté tenant)
**Parent deliberation:** `delib-20260610-9a0c` (panel: jobs, torvalds, tolnay,
shannon, janis, godel) — convergence K4, torvalds Q3, godel Q3.

**Binds:**
- [ADR-080](080-remote-pilot-port-https-oidc.md) §5 — the operator-only closed
  list this ADR amends (the §5.2 successor path, consciously taken).
- `task-20260610-e5f6` (B1 moussage resident) — the bounded `cs run` loop this
  route spawns: `RunBounds` (B1 depth / B2 width / B3 budget), named exit codes
  90/91/92/124, `[drain_bounds]` binding section, `GET /v1/quota` read face.
- `docs/formal/` (smithy) — MCStitch I1 single-writer-trunk; the
  co-location of loop, `StateStore` and `trunk.lock` is the validity condition
  of the advisory flock.

---

## 1. Decision

`cs run` leaves the ADR-080 §5.1 operator-only closed list. The RPP exposes

```
POST /v1/molecules/{id}/run        scope: cosmon:molecule:write + cosmon:worker:spawn
```

as a `tenant-verb` route on the §8p frozen surface. The route spawns the B1
resident drain loop (`cs run <root>`) **inside the tenant container**, detached,
and answers `202 Accepted` immediately. Lifecycle is observable on the events
bus (`drain.started`, `drain.terminated` with a named reason token).

## 2. Why the §5.2 objection is dissolved, not waived

ADR-080 §5.1 kept `cs run` operator-only for one stated reason: *"Long-running,
side-effectful, blocks the caller. Inappropriate for a JWT-authenticated
request."* Each clause is addressed by construction:

| §5.1 objection | What changed |
|---|---|
| Long-running | The route detaches: 202 on spawn, never a held connection. The loop's wall clock is itself bounded (`--timeout`, named exit 124). |
| Side-effectful / blast radius | The drain only tackles molecules in the **caller's own noyau**, under the binding-sealed B1/B2/B3 bounds (B3 obligatory — a tenant drain is never unbounded, godel Q3). The bounds are readable (`GET /v1/quota`) and never writable through any §8p route. |
| Blocks the caller | The caller is a request door, not a cockpit: the client DEMANDS, the server DECIDES what to tackle, when, under the lock (torvalds Q3). |

The §5.2 `delegate_for` claim model is intentionally **not** used: there is no
operator authority to delegate. The tenant drains *its own* molecules with
*its own* budget — the same authority chain that already covers
`POST /v1/molecules/{id}/tackle` (one worker spawn), extended to a bounded
sequence of spawns.

## 3. Bounds and refusals (the godel clause)

- The bounds live in a system stronger than the client: `[drain_bounds]` in
  the BLAKE3-sealed binding, resolved against server defaults (128/8/256).
- Refusals are stable and documented: `429 budget_exhausted` (B3),
  `409 max_depth_exceeded` (B1), `429 molecule_quota_exceeded` (B2) — mirrors
  of `cs run` exit codes 90/92/91 (`task-20260610-e5f6`). On the asynchronous
  path the same tokens arrive as the `drain.terminated` reason; the mirror is
  pinned by `drain_exit_reason_mirrors_reject_labels`.
- One resident loop per noyau (`409 drain_already_active`): a second loop
  would serialise on `trunk.lock` (MCStitch I1) while burning budget.

## 4. What stays operator-only

Everything else in §5.1 — `done`, `evolve`, `complete`, `kill`, `purge`,
`reconcile`, `verify`, `whisper`, `drop`, `security activate`. In particular
the *operator* gestures of `cs run` (unbounded drain, arbitrary noyau, local
flags) are untouched: the route never forwards client-supplied bounds, and the
canon line's scope expression is the only admission path.

## 5. Consequences

- `data/surface_events.txt` gains the route (tenant-verb); bijection tests
  (`routes_and_verbs_are_bijective`, both sides) pin it end-to-end.
- `cosmon_thin_cli::coverage::OPERATOR_ONLY` drops `run`;
  `docs/guides/api-cli-coverage.md` reclassifies the row `V0`.
- The tenant CLI (`cosmon-remote`) exposes `molecule run <root>` and the
  purely client-side composition `do` (nucleate + credit guard + tackle +
  follow) — zero new routes for `do`, doctrine §5.1 untouched for everyone
  else.
