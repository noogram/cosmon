# ADR-073 — Session-ensemble substrate (presence · drop · directed whisper · events tail)

**Status:** Proposed (anchor ADR — blocks C-PRESENCE-CORE, C-DROP-GESTURE,
C-WHISPER-SESSION, C-TAIL-EVENTS, C-DIVERGE-LIVELOCK, C-PEEK-ENSEMBLE,
C-MOBILE-SHORTCUT, C-DOCS-ENSEMBLE)
**Date:** 2026-04-24
**Parent deliberation:** `delib-20260424-c96b` — *Escape the Claude session trap* (7-persona panel: jobs · niel · einstein · wheeler · hawking · turing · torvalds).
**Authoring task:** `task-20260424-e386`.

**Binds:**
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — two-layer model (Transactional Core + Resident Runtime), three regimes.
- [ADR-003](003-multi-channel-nervous-tissue.md) — channel taxonomy (this ADR adds *no* seventh channel).
- [ADR-038](038-whisper-perturbation-port.md) — whisper as 6th channel; this ADR extends its target set to live peer sessions.
- [ADR-047](047-event-log-protocol-v0.md) — event-log substrate (`events.jsonl`) that `cs tail` consumes.
- [ADR-066](066-ux-v2-substrate.md) — wheat-paste UI discipline; presence header + ensemble tab are additional wheat-paste viewports.
- [ADR-061](061-pilot-session-and-causal-closure.md) — `pilot-session` kind; presence formalises the live-pilot side.

**Architectural invariants touched:**
- `§7e` — control plane vs data plane (this ADR strictly honours).
- `§8a` — shared-state substrate under `.cosmon/state/`.
- `§8b` — seal-as-trace, not lock (presence carries sealed-ancestor, verified, never enforced).
- `§8j` — ingress bindings (drop is a typed admission path identical in shape to the session-route cascade).
- `§8k'` — cross-surface wheat-paste (presence header and ensemble tab are viewports, not renderers).

---

## Context

### Three cousins, one basement, one postman

The operator runs 2–10 Claude Code sessions in parallel — different worktrees,
different galaxies, different persona stances — each holding its own cognition
in its own context window. When session A produces a synthesis that session B
needs (a recap, a molecule id, a new directive), the only bus today is the
**operator's clipboard**: cmd-C in A's terminal, cmd-V in B's terminal.
Measured baseline from `delib-20260424-c96b` (niel): **~72 inter-session
copy-pastes per operator per working day**. The operator is the router; the
cognitive cost is a router's cost, paid out of the operator's attention budget
per paste.

Three structural facts frame the problem:

1. **The shared substrate already exists.** Every Claude session reads and
   writes `.cosmon/state/` (§8a). Every molecule writes `events.jsonl`
   (ADR-047). Every worker's branch commits into the same git DAG. The
   three sessions are not islands; they sit on the same plot of land. What
   is missing is a *view of each other*.
2. **No session can see which other sessions are live.** There is no
   chalk-line on the wall saying *"session X is alive, cwd=galaxies/cosmon,
   working on molecule Y, last-heartbeat 14:02"*. A session cannot address
   a peer because it cannot name a peer.
3. **Operator-as-router is pre-cosmon physics.** Cosmon's entire architecture
   (§7e, §8j, §8k') is *"the operator observes; the filesystem routes"*. The
   clipboard-as-bus contradicts this in every molecule.

### Why not just add a broker / queue / mesh?

The deliberation asked this question to seven personas and received seven
independent *no*. The convergence is recorded in §II of the synthesis (Z8
refusal column) and reproduced verbatim in §Alternatives below. In short: a
broker duplicates the filesystem; a mesh duplicates the DAG; a queue
duplicates `events.jsonl`; a PKI duplicates the sealed-ancestor trace
(§8b); and *any* 7th channel fattens the control plane in a way that
ADR-016's regime composition depends on being thin (§7e).

### The RIGHT question (wheeler, panel-confirmed)

> **What is the minimal, durable, on-disk quantum of information that each
> Claude session must emit and read, such that N sessions form one
> distributed cognition without a human in the routing loop — using only the
> six existing channels (no 7th)?**

### What the delib converged on

All seven personas endorsed four artefacts, each a **typed projection over
an existing channel** — no new primitive at the channel level:

| Artefact | Channel (existing) | Role |
|---|---|---|
| `presence` | 3 (filesystem) | chalk-line: a session declaring *I am alive, here, doing X* |
| `drop` | 3 (filesystem) + 5 (propulsion) | universal inbox gesture (operator → spark molecule) |
| `whisper --to-session` | 6 (whisper) | directed text, session→session, advisory only |
| `cs tail` | 3 (filesystem) | fswatch-based reader over `events.jsonl` |

Umbrella concept name (wheeler, endorsed by all): **ensemble**. Already a
cosmon verb (`cs ensemble`), already physics-rooted (statistical ensemble of
micro-states producing macro-cognition), imported nothing.

Retired names (will be purged in C-DOCS-ENSEMBLE): *"session trap"*,
*"session mesh"*, *"cognition fabric"*, *"swarm"*, *"hive"*, *"constellation"*
(as coordination metaphor). *"Fourmilière"* survives as poetic flavour, not
as a domain noun.

---

## Decision

Cosmon adopts the **session-ensemble substrate**: a set of four typed
projections over the existing six-channel substrate, giving Claude sessions
the ability to see each other, address each other, and observe each other's
event streams — **without** introducing a broker, a daemon, a 7th channel,
a central runtime, a PKI, a mailbox, or a new transport.

The substrate is the umbrella; each projection lands as a separate child
task (see the children table below). This ADR freezes the semantics every
child must honour.

### 1. Presence — the chalk-line on the basement wall

A **presence file** is a typed JSON file that one live Claude session writes
on heartbeat and any other session reads on demand. It declares *"I am here,
this is what I am"*.

#### Location
```
.cosmon/state/presence/<sid>.json
```
Sibling directory to `fleets/`, `archive/`, `surfaces/`. Follows the §8a
main-repo-redirect rule, meaning cross-worktree sessions inside the same
repository all see the same `.cosmon/state/presence/` directory.

#### Record (soft schema — expand in C-PRESENCE-CORE)
```json
{
  "sid": "ses-2026-04-24-a3f7",
  "galaxy": "cosmon",
  "cwd": "/srv/cosmon/cosmon",
  "pid": 48213,
  "pane": "tmux://cs-cosmon:worker-0:0",
  "headline": "draft ADR-073 ensemble substrate",
  "molecule": "task-20260424-e386",
  "nucleon_id": "ses-2026-04-24-a3f7",
  "parent_prompt_seal": "<BLAKE3 of the Claude-session root prompt>",
  "last_heartbeat": "2026-04-24T14:02:31Z",
  "last_event_offset": 134217
}
```
Write discipline: **atomic rename-into-place** (`<sid>.json.tmp` → `<sid>.json`).
Readers always see a consistent record. Writers never lock.

#### Lifecycle
- Heartbeat every ~30 s while the session is live (cadence tunable in
  `~/.config/cosmon/presence.toml`).
- Discovered by directory scan (`cs presence ls`); no registry process.
- Stale records GC'd idempotently: if `last_heartbeat` is older than
  `stale_after` (default 3 × heartbeat = 90 s) **and** the `pid` does not
  exist, `cs presence gc` removes the file. A third-party scanner may safely
  run the same GC; it is a pure function of the filesystem.
- `cs presence ping` emits one heartbeat for the caller's session; run by
  the session's own wrapper on a timer (no daemon — the session owns its
  own heartbeat).

#### Authority
- Writers are **self-certifying** — each session owns its own file, indexed
  by `sid`. Two sessions writing to the same `sid` is an operator error
  (not a security boundary).
- **No PKI, no signatures, no mutual authentication.** Trust is derived
  from `parent_prompt_seal` (the BLAKE3 seal produced by `cs nucleate`,
  §8b) and a non-empty `git merge-base` with the reader. This mirrors the
  git model: anyone who can write to `.cosmon/state/` is already inside
  the trust boundary.
- Presence is **advisory** — its only effect is to let other sessions see
  who is live. No state transition depends on presence. No molecule is
  advanced or blocked by the contents of a presence file.

#### NOT a 7th channel
Presence is a **typed projection over channel 3 (filesystem)**. It is
discovered by directory scan, which is exactly how every other cosmon
state primitive is discovered. The six channels registry (ADR-003 + CLAUDE.md)
stays at six. This answers **[Q-presence-channel]** from the delib: no new
row, no "channel 3.5"; presence is channel-3 content with a typed schema.

### 2. Drop — the universal inbox gesture

`cs drop` is the operator-facing verb that turns a moment of operator
cognition (*"remember to pin the Cell B experiment on the roadmap"*) into
a spark molecule, in one gesture, from any context.

#### CLI surface
```
cs drop [TEXT]              # reads TEXT, or stdin, or interactive prompt
cs drop --galaxy <name>     # target galaxy (default: frontmost / cwd-derived)
cs drop --from-clipboard    # Hammerspoon / OS-hotkey path
cs drop --from-voice        # voice capture path (delegated to capture adapter)
cs drop --json              # emit the created spark molecule id
```

#### Semantics
- Nucleates a `spark` molecule in the target galaxy with tag `source:drop`
  and temperature `temp:hot`.
- **No classifier in the hot path.** Drop is a *direct admission path*:
  operator knows it is worth capturing, system accepts it, classification
  happens downstream (`cs session route` and delib-20260424-1b81's formula
  consume the same spark substrate).
- Causal-closure discipline (§8j): every spark carries the operator's
  identity via ambient session context (`nucleon_id`, `operator`), a
  BLAKE3 seal of the raw text, and a sidecar recording the admission
  (timestamp, source adapter, target galaxy). `cs drop` is an **ingress
  binding** — identical in shape to the session-route cascade of ADR-072.

#### OS-level surfaces (implementation deferred to C-DROP-GESTURE)
- **macOS global hotkey** — `⌃⌥Space` via Hammerspoon/Karabiner, opens a
  menubar sheet that pipes into `cs drop`. Operator preference: ask at
  tackling time whether Spotlight/Raycast already owns the chord **[Q-hotkey]**.
- **zsh in-terminal widget** — `Ctrl-G` binds a ZLE widget that expands
  the current line (or voice dictation buffer) into `cs drop …`.
- **iPhone Shortcut** — SSH/Tailscale path into `cs drop --from-voice`;
  lands as `temp:warm` (C-MOBILE-SHORTCUT), not this week.

#### What `cs drop` is NOT
- **Not an app.** It is a verb. Surfaces consume it; they do not replace it.
- **Not a classifier.** Classification lives in `cs session route` (ADR-072)
  and related formulas. Drop writes first, routes later.
- **Not a message to another session.** Drop writes to the galaxy's spark
  substrate; any session (including the drop author's own) can later find
  the spark through normal backlog discovery.

### 3. Directed whisper — `cs whisper --to-session <sid>`

ADR-038 defined whisper as **pilot (human) → worker (live) semantic
injection, advisory only, Propelled regime only, no ACK, unbounded text**.
This ADR extends the target set.

#### Target extension
```
cs whisper --to-molecule <mol_id>    # ADR-038 (unchanged)
cs whisper --to-session <sid>        # this ADR: target a live peer session
```

When `--to-session <sid>` is used:
- The target is resolved via `.cosmon/state/presence/<sid>.json`. Missing
  file or stale heartbeat → **fail-closed** (same discipline as ADR-038's
  `pane_current_command == "claude"` check). No `--force` escape in v0.
- The payload is appended to
  `.cosmon/state/presence/<sid>.log` — an append-only JSONL file beside the
  presence record, one line per whisper: `{ts, sender_sid, payload_sha,
  byte_len}`. The full body lands in
  `.cosmon/state/presence/<sid>.whispers/<ts>-<sha16>.txt`.
- The target session's inbox-discipline (CLAUDE.md — *"at the start of
  each turn, consult presence log"*) surfaces the whisper on the next
  natural transition. No interrupt, no notification daemon, no kill.

#### Authority invariants (unchanged from ADR-038, re-asserted)
- **Advisory only.** Cannot abort, cannot modify `state.json`, cannot
  modify the DAG, cannot transition a molecule.
- **Propelled only.** Forbidden in Autonomous; meaningless in Inert.
- **Human pilot only** — workers MUST NOT whisper. The DAG owns
  inter-molecule control (§7e). Session-to-session directed whisper is a
  **pilot** gesture, not an agent gesture. A Claude session acting on the
  operator's behalf whispers **as** the operator.
- **No ACK.** Rice's theorem: undecidable whether the target used the
  payload. Logged sender-side only.
- **No `events.jsonl` emission.** Whispers are perturbations on the 6th
  channel, not molecule lifecycle events.

#### Scale ceiling (hawking)
Whisper saturates at **N ≈ 10 simultaneous live targets**. The TUI for
`--to-session` presents a flat list; above ten, the operator's cognition
— not whisper itself — is the bottleneck. At saturation the fallback is:
leave a spark (`cs drop`) and let the peer session discover it through
backlog scan. This is documented in the help text; no code-level hard cap.

### 4. Events tail — `cs tail`

A fswatch-based reader over `events.jsonl` that streams newly appended
lines to stdout.

#### CLI surface
```
cs tail                      # tail events.jsonl for the current fleet
cs tail --molecule <mol_id>  # filter to a single molecule
cs tail --kind <k>           # filter by event kind
cs tail --all-galaxies       # cross-galaxy; requires syzygie citation
cs tail --since <rfc3339>    # replay from offset
cs tail --json               # NDJSON (default already NDJSON; kept for symmetry)
```

#### Semantics
- Implementation substrate: the `notify` crate (inotify / kqueue / FSEvents).
- Strict-ordered per-file: respects on-disk append order.
- **Fleet-local by default.** Cross-galaxy tail requires `--all-galaxies`
  **and** an explicit syzygie citation (per the cross-galaxy-edges
  discipline of ADR-035). Never implicit. This answers **[Q-cross-galaxy-tail]**.
- **Read-only.** `cs tail` never writes. No transaction, no reconcile.
- **No replay gap guarantee across rotation.** `events.jsonl` is truncated
  by a hypothetical future rotator (`Q-events-jsonl-growth` — deferred);
  `cs tail --since` must handle `ENOENT` by erroring out (not by silently
  gapping).

### 5. Umbrella naming — **ensemble**

The concept is called an **ensemble**. The operator's 2–10 live Claude
sessions form a **session-ensemble**. The cross-galaxy generalisation is a
**fleet-ensemble** (N sessions × M galaxies).

- `cs ensemble` (existing verb) gains a new meaning: the live-session
  substrate. `cs ensemble --tag temp:hot` already shows the actionable
  backlog; future flags will surface the presence registry.
- Rust types (C-PRESENCE-CORE): `Presence`, `SessionId`, `PresenceRegistry`.
- **No `Session` typestate in `cosmon-core`.** Per einstein's refusal:
  "session" is a pilot-layer artefact, not a domain noun. `cosmon-core`
  already has `Agent`, `Worker`, `Molecule`; adding `Session` would
  duplicate identity at the wrong layer.

### 6. The "no paste" cap — measurable commitment

| Horizon | Target |
|---|---|
| Baseline (today) | ~72 operator inter-session pastes/day |
| End of week 1 after first three children land | ≤ 10 pastes/day |
| End of week 2 | ≤ 2 pastes/day |
| Steady state | Residual paste reserved for non-cosmon text (Element quotes, Zotero excerpts, external URLs). Inter-cosmon-session paste count = 0. |

The metric is operator self-report until a future `cs patrol --paste-audit`
lands (out of scope here; not this week). The cap is **zero
clipboard-as-cosmon-bus**, not zero typing.

---

## Consequences

### Six channels stay six
ADR-003's six-channel registry is unchanged. Presence, drop, events-tail —
all are typed projections over **channel 3 (filesystem)**. Directed whisper
is a typed projection over **channel 6 (whisper)**, extending the target
discriminator from `mol_id` to `mol_id | sid`. CLAUDE.md §Channels line
*"Six channels total"* stays true.

### No broker, no daemon, no new runtime
- Presence files are written by the sessions themselves (heartbeat).
- `cs presence gc` is idempotent, called by any patrol or by `cs reconcile`.
- `cs tail` is a client-side reader, one process per invocation.
- `cs drop` is a one-shot CLI invocation.
- `cs whisper --to-session` is a one-shot CLI invocation.

The Transactional Core (ADR-016, Layer A) absorbs all of it. The Resident
Runtime (Layer B, future) gains one read port — it may consult the
presence registry to render a fleet-level dashboard — but it does not own
the writes. Composition with Layer B is preserved (§4 of architectural
invariants).

### Operator = observer, not router
Every child of this ADR cuts operator routing work. The metric above is
the commitment. The tattoo line (jobs, delib §IV): **"No paste. Drop."**

### Inbox discipline becomes a cosmon convention
A Claude session operating inside cosmon consults, at the start of each
turn, `.cosmon/state/presence/<own-sid>.log` for directed whispers landed
by peers. This is documented in CLAUDE.md §Inbox discipline (already
added). The discipline is **load-bearing for correctness of the whisper
semantics** — a session that ignores its inbox log is functionally
identical to ADR-038's `pane_current_command != "claude"` failure
(ignored payload, no ACK, sender logs only).

### Identity & trust — sealed-ancestor, not PKI
- A session asserts its identity by writing `parent_prompt_seal` into its
  presence record. A reader verifies the seal is BLAKE3-well-formed and
  that the writer has a non-empty `git merge-base` with the reader's
  worktree. Beyond that, trust is filesystem-scoped.
- §8b is strictly honoured: the seal is a **trace, not a lock**. Any
  operator with filesystem access can rewrite `<sid>.json` *and* its
  sealed-ancestor. The system catches the lazy shadow contract, not a
  motivated adversary.
- turing's formal model (shared-tape oracle `(M_1 … M_N, O)`, §II of the
  synthesis) is preserved: presence is the **tape header**, the
  filesystem is the tape, and verification is a pure function of the
  tape's current contents.

### Livelock detection becomes possible
Presence + `blocked_on.json` + Tarjan SCC = `cs patrol --livelock`
(turing, delib §VII). Deferred to C-DIVERGE-LIVELOCK. The substrate for
it exists after this ADR lands.

### Scale properties (hawking)
- `cs peek` saturates at **N ≈ 50 live molecules** (existing invariant).
- `cs whisper --to-session` saturates at **N ≈ 10 live targets** (human
  attention).
- Filesystem scan of `.cosmon/state/presence/*.json` is O(N) in live
  sessions, bounded in practice at N ≤ 50. No index needed.
- `events.jsonl` linear growth is orthogonal (§8d, §Q-events-jsonl-growth
  — deferred engineering detail).

### Cross-galaxy implications
Presence + tail respect galaxy boundaries by default (`cs tail` is
fleet-local unless `--all-galaxies` + syzygie citation). Cross-galaxy
presence lookup is NOT an implicit operation — a session in galaxy A
cannot address a session in galaxy B via `cs whisper --to-session`
unless the operator has explicitly set up a cross-galaxy registry link.
This is deferred; for the near term, an "ensemble" is a single-galaxy set.

### The syzygie frame
Cosmon, mailroom, and showroom form a syzygie — three repos aligned
by prose citation (CLAUDE.md §Syzygie). Once this substrate lands in
cosmon, mailroom and showroom receive a prose inheritance request
via the existing chronicle-lint formula. Each answers `inherit`,
`adapt(diff)`, or `refuse(reason)`. Silent-ignore is a bug.

---

## Alternatives considered — rejected

All seven personas agreed on the refusal list. Reproduced from delib
§VI, consolidated and annotated.

| Alternative | Rejected because | Personas citing |
|---|---|---|
| **Raft / Paxos / BFT consensus** on the molecule ledger | Git commit is already the consensus (content-addressed, distributed, replicated). Adding a second consensus protocol is strictly redundant and fattens the control plane (§7e). | turing, hawking, torvalds, niel |
| **PKI / key-distribution / certificate chains** between sessions | Sealed-ancestor (§8b) + git merge-base is sufficient for the trust model cosmon actually needs. PKI ceremony blocks the hot path, violating the "seal is a trace not a lock" invariant. | turing, wheeler, all |
| **Mailbox / message-queue / pubsub** (NATS, Redis Streams, Kafka, custom TCP) | Duplicates `events.jsonl`. A queue is a filesystem with worse durability guarantees. No broker we can run without a daemon. | all 7 |
| **7th channel** added to the six-channel registry | Every proposed 7th channel (session bus, presence registry, inbox stream) is a typed projection over channel 3 or channel 6. Adding a new row would decouple presence from the filesystem and force a new transport. | all 7 |
| **Centralised database** or **single-writer broker** | Filesystem scan of `.cosmon/state/presence/*.json` is the registry. Adding a DB requires a daemon, a migration path, and a consistency story that git already provides. | torvalds, hawking, niel |
| **Vector clocks / CRDTs** on top of presence | Git DAG is already a causal order (turing). Presence files are last-writer-wins-per-sid — single-writer per sid is the discipline; vector clocks solve a problem we do not have. | turing, torvalds |
| **Fork of Claude Code / custom LLM runtime** | Out of scope. The session trap is not *inside* Claude; it is the gap between sessions, on disk. | niel + all |
| **New SwiftUI app** for ensemble view | mac-pilot already exists. Adding surfaces here means extending existing wheat-paste viewports (ADR-066 §8k'), not spinning a new app. | jobs, torvalds |
| **MCP-as-broker** for inter-session coordination | `cosmon-mcp` is for external callers (editors, non-cosmon LLM clients), not for sessions inside a worktree. CLI-first invariant (§3e) stands. | torvalds, all |
| **Extending `cs whisper` beyond pilot → live-thing advisory** | Hawking caps at N ≈ 10 targets; wheeler treats bulk directed text as utterance, not whisper. `--to-session` stays a single-target advisory injection. | hawking, wheeler |
| **"Session" as a persisted domain noun** in `cosmon-core` | Presence is a pilot-layer artefact, not a domain type. Adding a `Session` typestate duplicates `Agent` / `Worker` / `Molecule` at the wrong layer. | einstein |
| **Voice multiplex / Xbox portable / Stream Deck / dedicated iPhone this week** | Reserved for Q3+ ADRs. iPhone Shortcut lands as `temp:warm` (C-MOBILE-SHORTCUT). | jobs, torvalds |
| **Auto-auth blocking the hot path** | §8b: propose mechanisms of verification, do not impose them. Any auth step that can block `cs drop` or `cs whisper` violates the gesture model. | turing, wheeler |
| **Utterance as a new distinct primitive** alongside `events.jsonl` | Parsimony (answer to **[Q-utterance-or-events]**): treat `utterance` as a typed **view** over `events.jsonl`, not a new file kind. `cs utter` becomes a typed emitter of a specific event shape; `cs tail --kind utterance` reads it. Keeps the substrate single. | this ADR, per wheeler's lens + hawking's scale argument |
| **`relay` as the gesture name** | Retired in favour of **drop**. Same gesture, shorter word, avoids telecom metaphor. | jobs, wheeler |
| **`session-mesh` / `fabric` / `swarm` / `hive` / `constellation`** as umbrella name | All import an outside metaphor. Umbrella is **ensemble** (already in cosmon's physics lexicon). | wheeler, all |

---

## Open questions carried forward

| Tag | Question | Owner | Resolution path |
|---|---|---|---|
| **[Q-hotkey]** | `⌃⌥Space` vs `Ctrl-G` vs both — does macOS Spotlight/Raycast own the chord? | C-DROP-GESTURE | Operator check at tackling time; both can ship, answer default. |
| **[Q-utterance-or-events]** | Distinct primitive or typed view? | **This ADR — answered: typed view**. | `cs utter` writes a `kind=utterance` event into `events.jsonl`. No new file kind. |
| **[Q-presence-channel]** | Seventh channel or typed projection? | **This ADR — answered: typed projection over channel 3.** | Six channels stay six. |
| **[Q-cross-galaxy-tail]** | Does `cs tail` cross galaxy boundaries by default? | **This ADR — answered: fleet-local by default; `--all-galaxies` + syzygie citation.** | Documented in §Decision/4. |
| **[Q-events-jsonl-growth]** | Rotation strategy at N = 500 sessions over months? | Deferred (engineering, post-landing) | Separate ADR when the growth bites. |
| **[Q-voice-dictation-on-drop]** | `⌃⌥Space` held >500 ms auto-invokes macOS dictation? | C-DROP-GESTURE (phase 1.5) | Deferred to child. |
| **[Q-iphone-auth]** | Tailscale vs SSH key vs tmux pipe for iPhone Shortcut? | C-MOBILE-SHORTCUT | `temp:warm`; Week 2+. |
| **[Q-ensemble-cli-shape]** | What does `cs ensemble --live` look like? | C-PEEK-ENSEMBLE | Deferred to peek-ensemble child. |
| **[Q-presence-cross-galaxy]** | Can session A in galaxy X address session B in galaxy Y? | Post-landing, separate ADR | Not needed in v0; single-galaxy ensemble first. |

---

## Children blocked by this ADR

Cross-referenced from delib §XI. All nine ship within 2–3 weeks; this ADR
is the sole blocker for C-DOCS-ENSEMBLE and a content reference for the
other seven implementation tasks.

| # | Child | Kind | Primary artefact | Size |
|---|---|---|---|---|
| 1 | **ADR-ENSEMBLE-SUBSTRATE** | 📐 decision | this file | 1 ADR |
| 2 | **C-PRESENCE-CORE** | 🔧 task | `cosmon-core::presence`, `cs presence ping/ls/gc` | ~300 LoC |
| 3 | **C-DROP-GESTURE** | 🔧 task | `cs drop` + OS hotkey + zsh widget | ~200 LoC + OS hook |
| 4 | **C-WHISPER-SESSION** | 🔧 task | `cs whisper --to-session` | ~90 LoC |
| 5 | **C-TAIL-EVENTS** | 🔧 task | `cs tail` via `notify` | ~200 LoC |
| 6 | **C-DIVERGE-LIVELOCK** | 🔧 task | `cs patrol --livelock` + Tarjan SCC | ~400 LoC |
| 7 | **C-PEEK-ENSEMBLE** | 🔧 task | `cs peek` presence header + ensemble tab | ~150 LoC |
| 8 | **C-MOBILE-SHORTCUT** | 🔧 task | iPhone Shortcut + SSH path | ~200 LoC |
| 9 | **C-DOCS-ENSEMBLE** | 🔧 task | Retire dead names; write Feynman doc | docs PR |

**Temperature policy at nucleation:**
- `temp:hot` → this ADR (closing), C-PRESENCE-CORE, C-DROP-GESTURE.
- `temp:warm` → all other children (operator promotes to `temp:hot` when ready).

---

## Status

**Proposed.** This ADR anchors the concept; each child lands the implementation
or the doc work. The architectural invariants document will gain a short
§8o ("session-ensemble substrate is channel-3 content with a typed schema")
as part of C-DOCS-ENSEMBLE — no new invariant, a clarification rider.

The ADR is the source of truth for every refused alternative above. A later
child that proposes a broker, a 7th channel, a PKI, or a `Session` domain
type MUST cite this ADR as a successor-ADR target and justify the
replacement — per the "one ledger, one writer, one witness" discipline
(ADR-052). Silent drift is forbidden.

---

## Appendix A — Feynman image (for future operator handbook)

> Three cousins share a basement. Each cooks a different dish in a
> different kitchen upstairs. Until today, the only way cousin A told
> cousin B *"I just finished the stew, here's the recipe"* was by the
> operator (grand-mère) running downstairs with a handwritten note. She
> stops eating her own soup to route every recipe.
>
> After this ADR, every cousin keeps a chalk-line on the basement wall
> that says *"I'm alive, I'm cooking X, last stirred 14:02"*. Anyone can
> read anyone's line. The family notebook (already in the basement) is
> where every cousin already writes down what they stirred — now anyone
> can open it too. If cousin A needs cousin B to know *right now*, A
> writes a post-it and sticks it next to B's chalk-line.
>
> Grand-mère goes back to her soup and watches the whole basement from
> the cellar stairs. The basement did not change. One drawer opened.

Tattoo: **no paste, drop.**
