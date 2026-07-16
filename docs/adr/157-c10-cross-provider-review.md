# C10 — Cross-Provider Adversarial Review of the Safe-Runtime Net (ADR-157)

**Molecule:** task-20260713-a293 · **Posture:** REFUTATION · **Reviewer family:**
CLAUDE (distinct from the CODEX sessions that authored the envelope — ADR-147
provider-family-diversity witness).
**Target (main @ `aafe728e4`):** ADR-156 (`51faf5c9a`) + mechanisms C1–C5.
**Parent deliberation:** `delib-20260713-92fe` (synthesis.md / outcomes.md).

---

## VERDICT-DOOR → **trous-restants** (holes remain — the door stays RED)

The net does **not** yet hold. A re-ignited runtime can still commit failure
**#2** (unreviewed security merge) **deterministically and trivially**, can
still commit failure **#3-preempt** through a residual race, and closes failure
**#1** only over a *subset* of "broken main" — a hung, doctest-breaking, or
feature-gated merge slips through, and on a compile *hang* the rollback never
fires and the trunk lock deadlocks the whole fleet. C4 (adapter routing) is
*fail-safe* but its central claim — "preserves routing intent" — is **false on
the only path that runs**: the field it reads is never emitted, so every
autonomous dispatch is silently forced to `local`.

**Do not flip `enabled=true`.** Findings B, D.1 and D.3 are each independently
blocking. Finding B is the sharpest: a one-line `cs tag --remove` defeats
RR-SAFE-2.

Plain-language: the kitchen built a smoke alarm (compile gate), a lock on the
"humans-only" door (reservation), and a name-tag reader (routing). But the
humans-only lock can be opened from the *inside* by the worker itself (B); the
smoke alarm is deaf to three kinds of smoke — recipe-comment fires, optional-
burner fires, and a stove that never stops heating (D); and the name-tag reader
is wired to a socket that is always empty, so everyone is sent to the cheap
station no matter what their tag says (A).

---

## RR-SAFE-2 (Authority) — **REFUTED.** `needs-review` is not monotone.

**Claim (ADR §RR-SAFE-2, synthesis janis Q2):** "review-required completed work
is held for a human"; the tag is "a **monotone** `needs-review` tag the worker
**cannot strip** (self-clearing = self-approval)."

**Probe:** is the tag actually monotone / worker-unstrippable?

**Result — HOLE (blocking).** There is no protection anywhere. `cs tag <id>
--remove needs-review` succeeds for any caller:
- `crates/cosmon-cli/src/cmd/tag.rs:31-33,53-63` — `--remove` is an unguarded
  free list; the only errors are invalid-syntax / empty-request.
- `crates/cosmon-state/src/ops/tag.rs` — `TagError` is `{EmptyRequest,
  MoleculeNotFound, StoreUnavailable}`; no protected-tag class.
- The runtime's merge guard is `requires_review(m) && !review_confirmed_on_disk(m)`
  (`crates/cosmon-runtime/src/resident.rs:508`, predicate at `:316-321`). It reads
  the tag set **live each tick** — so removing the tag removes the guard.
- `cs done` itself has **no** review check (only the compile gate). Confirmed by
  absence in `crates/cosmon-cli/src/cmd/done.rs` (the sole "review" hits are
  comments/tests).

**Exploitation path (deterministic, no race):**
1. Runtime dispatches a `security` / `needs-review` task (or a human tackles it).
2. The worker, using the ordinary CLI it is *told* to use (CLAUDE.md "CLI-first
   for workers"), runs `cs tag <own-id> --remove needs-review` from its worktree.
3. Worker `cs complete`s.
4. Next tick: `requires_review(m)` is now `false` → `ReadyFrontierScheduler`'s
   Done-sweep emits `Decision::Done` → `shell_out` runs `cs done` → branch merges
   to main with **zero** independent review.

This is failure **#2** re-committed verbatim. janis's design named monotonicity
as the load-bearing property; it was never implemented. `review_confirmed_on_disk`
returning `false` (fail-closed) is defeated because the *predicate that triggers
it* is worker-erasable.

**Fix direction (not applied):** make `needs-review`/`security*` removal refuse
unless the caller is the operator (no `COSMON_RUNTIME_ACTIVE`, no
`--by runtime:*` provenance), or move the review flag to a field that lives
outside the tag set. File as a blocking bead before re-ignition.

---

## RR-SAFE-1 (Integrity) — **PARTIAL.** Gate ⊊ "main is not broken".

**Claim (ADR §RR-SAFE-1):** "The runtime never advances `main` beyond a failed
combined-state gate." Mechanism: `cargo check --workspace --all-targets`
post-merge, `git reset --hard` + non-zero exit on RED
(`done.rs:1804-1847`, `run_post_merge_workspace_check` `:3537-3550`,
`reset_hard` `:3553-3568`).

Four probes; three are holes.

### D.1 — doctests are outside the gate (blocking)
`cargo check --workspace --all-targets` does **not** compile or run doctests
(only `cargo test --doc` does). A merge that breaks a ` ```rust ` doc example
passes the gate and lands on main. Every downstream worker's DoD gate is
`cargo test --workspace` (which *does* run doctests) → it goes red on unrelated
work — but the runtime never runs `cargo test`, so it keeps dispatching
dependents onto a main whose doctest surface is broken. The invariant says
"combined-state gate"; doctests are a real compile+run surface it omits.
CLAUDE.md itself mandates `cargo test --doc` as a Production gate, so this class
is live in this repo.

### D.2 — non-default features are outside the gate (medium)
`--all-targets` builds **default features only**. This workspace has non-default
features: `cosmon-registry/neurion-fallback` (pulls `rusqlite` + `neurion-core`),
`cosmon-transport/{integration,test-support}`, `cosmon-core/test-harness`
(`crates/*/Cargo.toml`). A merge that breaks code behind any of them passes the
gate and lands. Lower severity (optional surfaces) but the invariant as worded
does not hold.

### D.3 — no timeout: a compile *hang* wedges the fleet and never rolls back (blocking)
`run_post_merge_workspace_check` uses `Command::output()` with **no timeout**
(`done.rs:3538-3541`), and it runs **while the trunk lock is held** (acquired
`:1567`, gate `:1814`, dropped `:2044`). Consequences of a hung `cargo check`
(proc-macro loop, blocking `build.rs`, a git-dependency fetch that stalls):
- The git merge has **already landed on main** (before the gate); `reset_hard`
  only fires on a non-zero **exit**, never on a hang → main is left **advanced
  and possibly broken with no rollback**.
- `cs done` blocks forever holding `trunk_guard` → every other `cs done` and
  `cs stitch` blocks → **global merge deadlock**, recoverable only by the
  kill-switch (i.e. exactly the failure #6 posture the envelope wanted to retire).

This is the brief's explicit question — *"le reset --hard restaure-t-il proprement
si le compile hang/timeout?"* — and the answer is **no**. Wrap the gate in a
timeout; on timeout treat as RED (roll back + release lock + non-zero exit).

### D.4 — `reset --hard` is destructive to main-checkout WIP (advisory)
On a clean RED, `git reset --hard <pre_merge_head>` on the **main checkout**
discards *any* uncommitted changes there. CLAUDE.md explicitly notes the main
checkout carries operator WIP / unrelated notes. Fail-closed rollback can eat
that. At minimum emit a witness of what was discarded; ideally `--keep` or a
stash guard.

**Net:** RR-SAFE-1 holds against the *originally-observed* class (test-target
arity — the `egress.rs:645` E0061, which `--all-targets` genuinely catches) but
not against the general claim "main is not broken."

---

## RR-SAFE-3 (Ownership) — **PARTIAL.** No CAS; the last-moment gate ignores `hold:human`.

**Claim (ADR §RR-SAFE-3):** "the runtime's own claim is made by **compare-and-swap
while holding `lock_fleet()`**"; "Human reservation is an authority boundary, not
a scheduling hint."

### C.1 — the CAS does not exist (medium; contradicts the ADR text)
In `cs tackle`, the molecule is loaded at `crates/cosmon-cli/src/cmd/tackle.rs:291`
(`resolve_molecule`), far **outside** any lock. The lock at `:1248-1284` writes
`updated.mark_tackled(tackled_by)` on that *stale* value with **no re-read and no
compare** of the current `tackled_by`. The code's own comment `:1312-1320` admits:
"this check still happens **outside** the fleet lock, so a racer … can still
produce the same symptom; the proper structural fix is to make every
read-modify-write writer take `with_fleet_lock` (**TODO bead**)." turing flagged
"Q6-CAS is immediate fast-follow" — it did **not** land, yet ADR §RR-SAFE-3
states the CAS as an accomplished fact. The ADR overclaims its own mechanism.

### C.2 — the pre-dispatch recheck enforces `hold:pilot` but not `hold:human`/reserved-decision (medium)
`recheck_tackle_candidate` (`resident.rs:1071-1118`) is the *only* fresh-from-disk
gate before the shell-out. It checks `status=="pending"`, `tackled_by=="human"`,
and the `hold:pilot` tag — but **not** `reserved_for_human` (`hold:human`, or
`kind=="decision" && !auto:ok`). Those are filtered only in the scheduler's
*stale* snapshot (`:520`). So an operator who reserves via `hold:human` (or
converts a molecule to a reserved decision) in the window between the snapshot
and the dispatch is **ignored** — `recheck` returns `Dispatch`, `forget_dispatch`
never runs, the runtime tackles the just-reserved molecule. The two reservation
gestures are enforced at *different* consistency points; only `hold:pilot` gets
the authoritative last read. RR-SAFE-2 leans on `hold:human` being "an authority
boundary, not a scheduling hint" — for that gesture, the boundary has a residual
race (≈ one poll interval, default 1 s). `hold:pilot` (the jobs steering-wheel)
*is* correctly closed; `hold:human` is not.

**Fix direction:** re-check `reserved_for_human` inside `recheck_tackle_candidate`
too (it already fetches `tags` + `kind` via `cs observe --json`), and implement
the CAS the ADR already promises.

---

## RR-SAFE-4 (Legibility) — **witnesses PRESENT, but C4's core claim is FALSE.**

### A — adapter routing is dead plumbing; every dispatch is forced to `local` (correctness/legibility, not a danger)
**Claim:** "C4 preserves adapter-routing intent … a directional policy can replace
this floor through `EnsembleMolecule::adapter`."

**Result — HOLE.** The schema the runtime parses has **no adapter field**:
- `MoleculeStateEntry` (`crates/cosmon-cli/src/cmd/ensemble.rs:86-119`), the shape
  of the `molecule_states` array, defines `{id, status, kind, tags, blocked_by,
  merged_at, stuck_at}` — **no `adapter`**. `build_molecule_states` (`:128-150`)
  never sets one.
- The only `adapter` ensemble projects (`:377`, from `MoleculeProcess::adapter_name`)
  is the **running worker's** adapter, stamped by `cs tackle` — it does not exist
  for a *pending* molecule, and it is on the human-table row struct, not
  `MoleculeStateEntry`.
- Therefore `EnsembleMolecule::from_json` (`resident.rs:281-284`) parses
  `adapter = None` from **all** real CLI output, and `next_decisions` (`:534-538`)
  substitutes `SAFE_DEFAULT_ADAPTER = "local"` **every time**.

So the resident runtime renders `--adapter local` for every molecule it
dispatches, regardless of any routing intent. The passing unit test
`resident_dispatch_preserves_a_codex_adapter_pin` (`resident.rs:1662`) feeds
synthetic JSON containing an `"adapter"` key **the live CLI never emits** — a
fixture exercising an unreachable path (a fixture-independence smell in spirit).

This is **fail-safe for budget** — turing's #3-budget leak genuinely cannot
happen, no paid adapter is ever silently inherited — so it is *not* a danger and
does not by itself block the gate. But the ADR/synthesis wording "preserves
routing intent" is wrong: intent is uniformly **overwritten** to local, and a
`kind=decision`/deliberation meant for a frontier model is **silently downgraded
with no witness**. Either wire a real routing policy that writes the field and
emit it in `MoleculeStateEntry`, or restate C4 honestly as "safe-local floor,
routing deferred (L1)".

### C5 — output witness exists, but "distinguishes real output" is overstated (advisory)
`last_output_at` is bumped **only** on step-advance
(`crates/cosmon-cli/src/cmd/evolve.rs:647,769`) and never by heartbeat
(`heartbeat.rs:76` bumps only `last_progress_at`). Good — that is turing's fix,
and `OutputStalled` folds correctly (`patrol.rs:533-557`). **But** a step
"advance" bumps `last_output_at` even when the step produced **no durable
artifact and no commit**: the auto-commit is best-effort and *skips empty diffs*
(CLAUDE.md), so a worker looping `cs evolve --evidence "…"` over empty steps keeps
the witness perpetually fresh with zero output — the "touch-without-commit"
evasion the brief asked about. The witness distinguishes *step-advance* from
*heartbeat-liveness*, not *durable-commit* from *empty-advance*. Since RR-SAFE-4
is witness-only (non-blocking, §8b), this does not gate re-ignition, but the
"real output" language should be softened, or the bump should be conditioned on a
non-empty commit / artifact write.

---

## Summary

| Invariant | Verdict | Blocking? | Anchor |
|---|---|---|---|
| RR-SAFE-2 Authority | **REFUTED** — `needs-review` worker-strippable | **yes** | tag.rs:31-63; ops/tag.rs; resident.rs:508 |
| RR-SAFE-1 Integrity D.3 hang → no rollback + trunk deadlock | **HOLE** | **yes** | done.rs:3538, 1567–2044 |
| RR-SAFE-1 Integrity D.1 doctests uncovered | **HOLE** | **yes** | done.rs:3539 |
| RR-SAFE-1 Integrity D.2 non-default features | HOLE | medium | crates/*/Cargo.toml |
| RR-SAFE-1 Integrity D.4 reset --hard eats WIP | advisory | no | done.rs:3553 |
| RR-SAFE-3 Ownership C.1 CAS absent (ADR overclaims) | HOLE | medium | tackle.rs:291,1248-1320 |
| RR-SAFE-3 Ownership C.2 recheck ignores hold:human | HOLE | medium | resident.rs:1071-1118 |
| RR-SAFE-4 C4 routing dead → forced local | HOLE (fail-safe) | no* | ensemble.rs:86-150; resident.rs:534 |
| RR-SAFE-4 C5 witness present, evasy by empty-advance | advisory | no | evolve.rs:647; heartbeat.rs:76 |

\* not a danger; a false *claim* + a silent frontier-downgrade. Fix the wording
and/or wire the policy.

**Assigned-dissent note (per ADR §Verification):** the ADR asked whether a
permanent gate could itself harm the drain. Finding D.3 confirms a *sharper*
version of that fear — the gate can **deadlock** the drain (hung compile under
the trunk lock), and D.4 shows it can destroy operator WIP. The gate is not just
possibly-too-strict; in its current shape it has a liveness failure mode. Bound
it with a timeout and a WIP guard before it is trusted unattended.

### Live corroboration of Finding D (observed during this review)
At the exact target revision `aafe728e4`, `cargo fmt --all -- --check` **fails**
on ≥8 tracked files (`tackle_env.rs:478`, `doctor/supervision.rs:388/431/456`,
`tackle.rs:6824`, `verify.rs:1460`, `interaction.rs:864`,
`cosmon-rpp-adapter/tests/install_sh_portability.rs:48`), with a working tree
that is otherwise clean. So **main is, right now, in a state that fails a DoD
gate** (`cargo fmt --check`) which the runtime's C1 integrity gate does **not**
run — `cargo check --workspace --all-targets` is fmt-blind. This is empirical
proof of the D-class claim: the C1 gate ⊊ "main is healthy." A runtime that
merged this state would report success while main is DoD-red. (These files are
outside this molecule's scope; per the no-fix mandate they were left untouched
and are surfaced here as evidence, not patched.)

**Recommendation:** keep `enabled=false`. File blocking beads for B, D.1, D.3;
medium beads for C.1, C.2, D.2; correctness/wording beads for A and C5. Re-run
C10 after B/D.1/D.3 land. ADR-156 stays **Proposed**, not binding.
