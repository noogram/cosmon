# `cosmon-dev` — the robust-dev spore (the first spore of the cosmon repo)

`cosmon-dev` is the mission that turns **one issue reported by an external tester**
into a **deterministic red reproduction** (a validation gate), a **smallest fix**,
a **double clean-room review by two different provider families rendered as two
immutable verdicts**, and a **release** — with **no agent pushing to any remote**.

It is **dogfooding**: unlike a shareable spore that travels to a stranger's machine
(math-attack in the sporarium repo), `cosmon-dev` lives *inside* cosmon and
composes the repo's own primitives by relative path.

## Guiding principle: reuse, do not reinvent

The spore **wires** existing cosmon primitives; it rewrites none:

| primitive | role here | reference |
|-----------|-----------|-----------|
| `cross-provider-committee` | the provider-diverse jury (claude + codex-sol) = the double clean-room review | `../../.cosmon/formulas/cross-provider-committee.formula.toml` (ADR-147) |
| `bug-closure` | the CLOSED/REOPEN verdict over the full semantic surface = the closure gate | `../../.cosmon/formulas/bug-closure.formula.toml` |
| `task-work` | the agentic gate legs (verdict contract in each node topic) | `../../.cosmon/formulas/task-work.formula.toml` |

Only **three** formulas are genuinely new (they ship in `formulas/`):

- `clean-room-repro` — the G2 reproduction gate (deterministic red, frozen before
  the fix, differential refutation, false-green/false-red modes; emits verdict.json).
- `lane-triage` — the lane predicate: four conjuncts over artifacts on disk decide
  fast versus full, fail-closing to full on any absent input. The only recipe here
  declared `deterministic = true`, because its output has to be a pure function of
  four files that a stranger can recompute.
- `converge-clean-room` — the §6bis double-engine convergence, rendered as **two
  immutable verdicts** over `cross-provider-committee`: an INITIAL verdict and —
  only if it found something AND the fix moved the target tree sha — one
  CONFIRMATION verdict. It reads the seats' verdict.json, computes CLEAN as the
  conjunction over the **ballot-carrying** seats, and fail-closes on an absent /
  unparseable / undelivered verdict into a **VOID** verdict that does not advance
  the state. No round counter, no round ledger, no third verdict. `while` is
  deliberately **not** composed — it was the N-round machinery (operator decision D1
  amendment 1 on `delib-20260729-1d4e`).

  Four things the N-round machine had learned on top of itself are carried into the
  two-verdict shape rather than dropped with it: the **roster is an output** (read
  the generator's family first; never hard-code `claude + codex`), per-seat
  **delivery** disposition, **realized-versus-specified** endpoints (the floor is a
  claim about what answered), and the **polarity-relative** reading of a seat's
  verdict (`confirmed` is CLEAN only under polarity `fix`).

  **This file is the canonical source.** `.cosmon/formulas/converge-clean-room.formula.toml`
  is a byte-identical projection of it — that is the path `cs nucleate
  converge-clean-room` resolves. The two copies drifted for a month across 355 lines
  because both were hand-edited; parity is now a gate
  (`crates/cosmon-cli/tests/formula_projection_parity.rs`).

## Layout

```
spores/cosmon-dev/
├── README.md              # this file
├── spore.toml             # the wiring: params + fleet + formula aliases + DAG + seal
├── spore.tla / spore.cfg  # the seal (3 properties — and what it does not model)
├── mission-template.md    # the parameterized briefing
├── formulas/
│   ├── clean-room-repro.formula.toml      # NEW — G2 reproduction gate
│   ├── lane-triage.formula.toml           # NEW — the lane predicate (deterministic)
│   └── converge-clean-room.formula.toml   # NEW — §6bis two-verdict convergence
│                                          #   CANONICAL; `.cosmon/formulas/` holds
│                                          #   a byte-identical projection of it
├── clean-room/            # the chambre-blanche container discipline (§4)
│   ├── README.md          #   one image, three postures, disposable login, two net modes
│   ├── Dockerfile         #   debian bookworm-slim by digest, claude-code pinned, autoupdate off
│   └── scripts/           #   run-clean-room.sh · disposable-login.sh · assert-no-host-mounts.sh
└── repro/                 # the two red repro-contracts (§5 — the seeds)
    ├── README.md
    ├── contract-21-adapter-resolver.md            # #21 (resolver precedence, no LLM)
    ├── contract-20A-root-bypass-spawn.md          # #20A (root spawn, no LLM)
    └── contract-20B-prompt-write-outside-worktree.md  # #20B (fs containment, offline claude -p)
```

## The gate DAG (blueprint §3 — diamond, not pipeline)

```
trace  (root+leaf, always-on sidecar — no edges)
route  (second root — no incoming edge, forbidden from naming the issue) ──┐
                                                                           │
intake(G0) → contract(G1) → reproduce(G2)                                  │
                              ├─→ triage → implement(G4) ─┐                │
                              └─→ falsify(G3) ────────────┴─→ green(G5)    │
                                                               │           │
                                                          ci-gate(G8)      │
                                              ┌────────────────┴──────┐    │
                                       converge (§6bis, EMERGENT)     └─→ rehearsal(G9)
                                              └────────────┬───────────────┘
                                                           ▼
                                              release(G10) ⇒ CANDIDATE-LANDED
                                                           ⋯
                                              confirm(G11) ⇒ CLOSED, asynchronous
```

`dissent` is no longer a node — it is a field of the release manifest, validated by
`release` itself. `rehearsal` runs in parallel with `converge` rather than behind it,
and `confirm` is off the blocking path.

## The two lanes

> **Fast lane if — and only if — you can name one command that goes red on the
> released binary and green on your patch, and your patch changes no public interface
> (no CLI flag, no config key, no on-disk format): everything else is the full lane,
> and if you are unsure, it is the full lane.**

Nobody decides that by judgement. The `triage` node evaluates four conjuncts against
artifacts on disk and the lane is `fast` only if **all four** are literally true:

| conjunct | what it reads | false means |
|----------|---------------|-------------|
| `intake_fields_present` | the four G0 fields as a **schema over the issue body** — verbatim symptom, OS+UID+arch+version, transcript, deterministic-or-flaky | an evidence field is missing |
| `frozen_red_admissible` | `reproduce/verdict.json`: PASS, non-empty `frozen_hash`, the differential refutation recorded **in both directions**, §8 keys self-consistent | there is no artifact that can judge |
| `blast_radius_bounded` | the **declared** `write_set`: non-empty, ≤ `fast_lane_max_files` (5), disjoint from `release_surface` | too wide, or **absent** — omission is the expensive answer |
| `risk_normal` | `risk == normal` **and** no declared path matching the security surface | anything security-touching |

Three properties of that table are the whole design:

1. **The judge is an artifact, not an authority.** *"'Known cause, bounded fix' is not
   an operator adjective — it is the existence of a frozen red whose colour flips
   under one variable. That artifact **is** the definition."*
2. **It fails closed in one direction only.** Every absent, unreadable or unevaluable
   input yields `full`. There is no input whose *absence* can produce `fast`.
3. **The authority is asymmetric, and the schema enforces it.** `--var lane=full`
   widens, always. `--var lane=fast` is a germination refusal — `[spore.params.lane]`
   is an enum over `{auto, full}` and expansion checks membership:

   ```
   $ cs spore validate spore.toml … --var lane=fast
   cs: expand failed: param "lane": value "fast" is not a member of the enum values ["auto", "full"]
   ```

   The operator may force the full lane; **there is no knob that forces the fast
   one.** A `lane` field somebody fills in would be the `risk` field somebody filled
   in, wearing a new name — and `risk = normal` was the default that switched off
   exactly the arm the #20 defect lived in.

**What the fast lane actually buys, and what it costs.** It skips the CONFIRMATION
verdict (the dearest node in the mission — the INITIAL verdict still sits and its
findings still block; what is skipped is the second committee re-reading the
corrected tree) and it does not re-review the G1 contract, which travels as a content
hash `release` recomputes. Both are written into `lane.json.waives` and copied
verbatim into the manifest's mandatory non-empty `arms_not_run[]`. A lane that saves
time without recording what it stopped doing has lost the record of its own cost.

**What the fast lane never touches:** the ballot. It cannot lower the jury floor,
cannot make a VOID verdict admissible, and cannot waive the CLEAN∧CLEAN conjunction —
that conjunction is the entire product of provider diversity.

**The lane is re-checked where the patch is real.** `triage` decides from a *declared*
write set, because it runs before the fix exists. Declared sets rot; `git diff` cannot
lie. So `release` re-evaluates the bound against `git diff --name-only
<base_sha>..HEAD` and BLOCKs a fast lane that outgrew its predicate with
`lane-escaped-its-bound`.

**What `triage` does not do:** it does not skip `intake` or `contract`. Both are
upstream of the artifact that decides the lane, so no lane decision can un-run them.
The fold is the stronger half instead — intake's fields are *re-decided by a schema*
(a predicate cannot be talked into PASS) and the contract travels as a digest.

## The priority lane (a procedure, not a mechanism)

A priority fix preempts the running fleet using **verbs that already exist**. There is
nothing to build:

```bash
cs claim <mol>            # for each ready-but-unstarted molecule: a durable
                          #   hold:pilot the resident defers on. Idempotent.
cs freeze <worker> --by <new-worker> --reason "preempted by <mol>"
                          # ONLY the running workers whose write set intersects
                          #   the priority fix. Freezing the rest buys nothing.
cs tackle <priority-mol>  # dispatch
cs release <mol> ; cs thaw <worker>    # afterwards, in that order
```

*"A tag is a wish; `cs freeze` is a state transition with an event."* The partial
state is already safe: the branch is a real ref, the worktree survives until
`cs done`, and the verdict files live outside the worktree — which is precisely why
ADR-161 exists.

**Three gaps, recorded rather than papered over:**

1. **No atomic verb.** The resident can dispatch between the claim sweep and the
   freeze. Closed by `cs preempt` (K5); until then the race is narrow and real.
2. **A frozen worker's in-context reasoning does not survive.** So every gate step's
   acceptance requires an **append** to `${output_dir}/progress.jsonl` — not one
   write at the end. A worker frozen mid-step must leave behind what it had.
3. **Freezing must be subtree-wide.** Freeze a `converge` worker and its committee
   seat molecules keep running, unwatched, with a collector nobody will dispatch —
   that is the orphan-seat shape, manufactured on purpose.

**The hazard that had to be closed before this lane was usable at all:** a frozen
molecule's already-emitted verdicts certify a tree that no longer exists once the
priority fix lands. The fix is `base_sha` plus the `release` ancestry clause — every
upstream verdict's `base_sha` must be an ancestor of the release commit, or the
release BLOCKs. Both have landed (S3), so the priority lane is usable. Without them,
"preempt and resume" is a way of shipping a stale certificate.

**CLEAN = ballot-weighted conjunction**: every **ballot-carrying** seat
(`on_ballot`, `diversity_weight > 0`) must return CLEAN in the same verdict, and the
ballot-carrying set must be non-empty. A single FINDINGS from a ballot-carrying seat
blocks and the fix corrects the UNION of the ballot-carrying reports; an off-ballot
seat's findings are recorded as residuals and block nothing on their own, because an
off-ballot CLEAN certifies nothing. A verdict that measured nothing — absent,
malformed, unparseable, or a seat not `delivered` — is **VOID**: it does not advance
the state, the seat is re-dispatched once, and a second void collapses on
`verdict-plumbing`. Every non-CLEAN outcome is `blocked` + human escalation with a
typed `exit_reason`, NEVER a silent pass.

## The seal (TLC-verified green)

`spore.tla` + `spore.cfg` model the diamond gate DAG + the bounded convergence and
discharge three properties: **Termination** (the convergence is bounded structurally
by its two named verdict states; the DAG is acyclic; a blocked convergence cascades
to a terminal state, no spin), **GateFailClosed** (no gate promotes on absent/failing
evidence; release SHIPs only when every upstream gate PASSED, the convergence is
CLEAN, and the dissent field is non-empty), and **NoResourceCollision** (no two nodes
are handed the same output path).

`DeterministicParametrization` was **deleted** (operator decision D3 on
delib-20260729-1d4e): it asserted `Roles = ExpandedRoles` over one literal
thirteen-element set, so no reachable state could falsify it.

**What the seal does not model.** `NoResourceCollision` quantifies over the node
output paths germination hands out — not the shared source tree, not the shared
cargo `target/`, not a path a gate's prose names by hand. Inside the module
`ArtifactPath(r) == r`, so what is discharged is the injectivity of `ToString`.
Real file collisions are covered separately by an executable test that does I/O,
`crates/cosmon-core/tests/spore_real_file_collision.rs`; its witness is the pair
`("Route", "route")` — two distinct strings the seal certifies as non-colliding,
one directory on APFS or NTFS — and germination now refuses that pair.

> The seal must always say two things: what it proves and what it does not model.
> A proof of the model must never be presented as a proof of the real environment.

Re-verify (any Java 11+; jar at `../../docs/specs/tla2tools.jar`):

```bash
cd spores/cosmon-dev
export TLA2TOOLS_JAR=../../docs/specs/tla2tools.jar
java -XX:+UseParallelGC -cp "$TLA2TOOLS_JAR" tlc2.TLC -workers auto -config spore.cfg spore.tla
# => Model checking completed. No error has been found.
```

## Run it

```bash
cd spores/cosmon-dev
cs spore validate spore.toml \
    --var issue="#21 --resident ignores COSMON_DEFAULT_ADAPTER" \
    --var affected_ref="v0.2.2" \
    --var upstream_version="0.2.2"
cs spore run spore.toml --var ... --allow-unchecked-seal   # released cs: TLC-verify not wired
cs run --resident --poll-interval 5 <germinated-root>       # absorbs the converge verdicts' children
```

## The limite dure (blueprint §8, honoured)

The spore edges **order** molecules; they do NOT prove a review passed or that two
seats had distinct identities or that a branch rule held. Every gate **emits and
validates** a machine-readable verdict and **fail-closes** on absent/malformed. The
spore germinates the **topology**; identities, credentials, and branch-protection
stay external human controls. No agent pushes to any remote.

## Named follow-ups (surfaced, not botched)

- **Warn-not-collapse** for a model pinned with no reachable adapter (blueprint §5
  adjacent nit to #21) — a germination/dispatch warning, not a collapse.
- **Typed-blocked, not auto-Enter** for an unapproved prompt (blueprint §5 adjacent
  nit to #20B) — an untyped auto-approval is a security fault; retire it from the
  autonomy claims and render a typed `blocked` state under a bound.
- **Re-pin the Dockerfile base digest + apt snapshot date** before the first real
  clean-room run (the committed Dockerfile carries placeholder pins to re-validate).
