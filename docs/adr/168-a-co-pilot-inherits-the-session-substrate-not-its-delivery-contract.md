# ADR-168 — A co-pilot inherits the session substrate, not its delivery contract

**Status:** Accepted (2026-08-01).
**Date:** 2026-08-01.
**Decider:** Noogram.
**Authoring molecule:** `task-20260731-0561` — mission co-pilotage M0
(*inventaire expérimental et ADR de delta*).
**Mission source:** the prepared mission brief
*co-pilotage multi-provider des missions Cosmon* (stagecraft CMB outbox,
2026-07-24), read read-only. That document defines the ten mission invariants
this ADR is measured against; it is not restated here.
**Scope:** doc-only. **This ADR changes no CLI surface, no flag, no output
byte.** It is the gate M1–M9 must pass through.

**Evidence attached to this ADR** — all three produced by observing real
sessions and running the real `cs 0.5.0 (8db48169)` binary:

- [`trace-a-claude.md`](168-multi-provider-copilot-session-substrate/trace-a-claude.md)
  — anonymised envelope of a live Claude Code session.
- [`trace-b-codex.md`](168-multi-provider-copilot-session-substrate/trace-b-codex.md)
  — anonymised envelope of a real 8 440-line Codex rollout.
- [`probe-log.md`](168-multi-provider-copilot-session-substrate/probe-log.md)
  — seven reproducible probes (P1–P7) of the existing primitives.

**Binds:**
- [ADR-038](038-whisper-perturbation-port.md) — `cs whisper` as the pilot→worker
  perturbation port, advisory by construction.
- [ADR-061](061-pilot-session-and-causal-closure.md) — `pilot-session`,
  `nucleon_id`, causal closure.
- [ADR-111](111-mission-convention-existing-primitives.md) — a mission is a
  molecule plus `Blocks` edges; no new runtime.
- [ADR-115](115-cs-pilot-cognitive-pilot.md) — `cs pilot`, the external
  cognitive pilot.

---

## Context

The mission wants a second pilot — initially Codex — to watch a primary Claude
Code session live, talk to it, publish a traceable second opinion, and take the
controls when the primary runs out of credit, without double command.

The brief's own table says most of this already exists in cosmon: presence,
whisper, diverge, claim/release, `claudion`, `energy_probe`, `cs pilot`. M0's
job was to find out whether that is true by measurement rather than by reading
the table. It is *half* true, and the half that fails is not the half the table
predicts.

Two facts govern everything below, and neither was visible from the brief.

**First: the substrate exists and its delivery contract does not.** Presence
files, a per-session log, a seek pointer and a tri-valued comparison verb are
all really there. But the four properties the protocol needs from them —
a resolvable session id, at-least-once delivery, idempotent consumption, and an
unknown that stays unknown — are absent, and three of them are absent in ways
that are silent. P1 shows `cs diverge` reading a presence path `cs presence
ping` never writes. P3 shows the mailbox advancing its seek *before* the reader
consumes the text. P4 shows a rotated log swallowing its backlog and reporting
success. P5 shows the reader panicking on a seek that lands inside a multi-byte
character. None of these fail loudly today because nothing yet depends on them.

**Second: the quota signal is on the wrong side.** The mission's trigger is
"Claude reaches a limit". Trace B shows Codex publishing `used_percent`,
`window_minutes` and `resets_at` on every one of 1 451 `token_count` events.
Trace A shows Claude publishing nothing of the sort: a scan of the 40 most
recently modified Claude logs on this host found `rateLimits` only inside
`system`/`api_error` records — the limit is announced as the failure that
already happened, never as an approach. The session that would *lose* authority
is the one that cannot see it coming.

## Decision

### D1 — Inherited unchanged

| Need | Substrate | Why it is inherited as-is |
|---|---|---|
| Live-session directory | `.cosmon/state/presence/` — one file per session, single writer, directory scan, no broker | Satisfies the brief's "state under `.cosmon/state/` in files readable with `cat`/`jq`, no daemon owning the state" exactly. |
| Staleness | `STALE_AFTER` (3 min) + PID liveness + idempotent `gc` | Already fail-closed: a hard-killed session disappears within one heartbeat window. |
| Advisory authority of a message | ADR-038 — a whisper perturbs, it does not command | Is ADVISORY-DRIFT, already decided, already enforced. |
| Mission shape | ADR-111 — molecule + `Blocks`, no mission runtime | M0–M9 are ordinary molecules. No `MoleculeKind` is added. |
| Worker channel | `cs whisper <mol>` and its tmux pane-signature gate | Untouched. Pilots are not workers; the two channels stay separate (see D5). |
| Cost measurement | `claudion` (Claude), `cosmon_core::codex_energy` (Codex) | The token/price arithmetic is correct and provider-specific. Only its *reading* discipline changes (D2). |

### D2 — Inherited but adapted

| Need | Substrate today | Required adaptation | Evidence |
|---|---|---|---|
| Session identity | `SessionId` is a free string; `cs presence ping` defaults to `$COSMON_SESSION_ID` or a tty-derived id | Key on `<provider>:<native-session-id>`. Both providers already carry a stable native id — Claude in the filename *and* in every record, Codex in `session_meta.payload.session_id`. | Traces A, B |
| Repo identity | Claude's project directory name via `sanitize_path`; Codex via `session_meta.payload.cwd` | Never decode a directory name. Read `cwd` from inside the log for both providers. `sanitize_path` maps every non-alphanumeric byte to `-` and is provably non-invertible. | P6 |
| Session discovery | `resolve_codex_session_by_cwd` returns the most recently modified log matching a cwd | Return the *set*, and make the caller disambiguate by native id. Collapsing two sessions in one cwd to one is the brief's own falsifier. | P6 |
| Incremental read | `claudion::parse_session` reads the whole file and errors on the first incomplete line | A byte-cursor port that tolerates a partial trailing line and survives truncation and rotation. | P7 |
| Presence record | `Presence { session_id, galaxy, cwd, pid, started_at, heartbeat_at, current_molecule, headline, tty }` | Add `provider`, `native_session_id`, `role`, `follows`, `capabilities`, `checkpoint_id`. Additive only — every existing field keeps its meaning. | — |
| Session mailbox | `<sid>.log` + `<sid>.seek`, plain-text line, `from:<os-username>` | An envelope with id, source *session*, sequence, content hash and state. The current sender identity is the OS username, which cannot distinguish two pilots on one host. | P3 |
| Comparison verb | `cs diverge` — tri-valued, decidable, structural | Keep the tri-valued shape and the Rice disclaimer. Fix the two defects below before anything is built on it. | P1, P2 |

### D3 — Refused

1. **Quota-triggered takeover, in any form.** Not merely because
   TAKEOVER-SUPERVISED says so, but because Trace A shows the primary has no
   proactive quota signal to trigger on. A heuristic built on Codex's own
   `used_percent` would be the co-pilot deciding, from its own fuel gauge, that
   the *other* pilot should stand down. The transfer stays an operator gesture.
2. **A broker, a daemon, or a resident mailbox process.** The mission's own v0
   scope forbids it and the presence directory does not need it.
3. **Copying provider conversations into cosmon.** Traces A and B in this ADR
   are envelopes with content removed; that is the permanent ceiling. What
   crosses into cosmon is checkpoints and message envelopes, never transcripts.
4. **A drift score.** A finding cites two assertions and their evidence, or it
   is `INCONCLUSIVE`. No opaque number, no confidence percentage.
5. **Absorbing `cs session` or `cs pilot`.** `cs session` stays the operator
   notebook; `cs pilot` stays the cognitive REPL of ADR-115. The plural surface
   `cs sessions` is a third thing.
6. **Reusing `cs whisper <mol>`'s tmux path for pilot-to-pilot messages.**
   Pasting into another pilot's pane would inject keystrokes into a live human
   conversation and break OBSERVATION-NEUTRE. Pilot messages go through the
   file channel only.

### D4 — Four defects that are M2/M3 entry criteria

These are pre-existing and are **not** fixed by this molecule — M0 changes no
surface. They are recorded here because each one, left alone, silently falsifies
an invariant the moment a co-pilot depends on it. Each must land as a *failing
test first* in the molecule named.

| # | Defect | Invariant it breaks | Owner | Status |
|---|---|---|---|---|
| P1 | `PresenceStore` writes `presence/<sid>.json`; `cs diverge` reads `presence/<sid>/presence.json` | PROVIDER-ID-NATIVE — no session is addressable by id | M2 | **Closed** by `task-20260731-0c2d`. `cs diverge` now decodes the path through `PresenceStore`, the writer that owns it. |
| P2 | An unresolvable session exits 1 (*disagree*), not 2 (*inconclusive*) | ADVISORY-DRIFT — unknown is rendered as a verdict | M3 | Open. |
| P3 | `poll` advances the seek before the reader consumes the tail | MESSAGE-TRACE — delivery is at-most-once, not at-least-once | M2 | **Closed** by `task-20260731-0c2d`. The tail is flushed before the seek moves. |
| P4/P5 | A stale seek past a rotated end silently swallows the backlog; a seek inside a multi-byte character panics the reader | MESSAGE-TRACE, and the M1 acceptance clause on truncation and rotation | M1/M2 | **Closed** by `task-20260731-0c2d` for the text channel, which clamps the offset both ways. The traced mailbox has no offset at all (see below), so neither failure exists there. |

M2 chose not to *fix* the byte cursor for the pilot mailbox but to not have
one. P4 and P5 are properties of offsets rather than of that particular code:
an offset is a claim about a file, and the file can invalidate it without
telling anyone. `<sid>.inbox.jsonl` is consumed by acknowledging a
`MessageId` in `<sid>.inbox.ack.jsonl`, which is a claim about a *message*,
and a message cannot be rotated out from under its own id. The clamping above
is what the legacy `cs whisper --to-session` channel gets, because that channel
keeps its offset.

### D5 — State schema

Additive to what exists. `PilotPresence` and `PilotMessage` landed in M2
(`task-20260731-0c2d`) as `cosmon_core::presence::Presence`'s six new fields
and `cosmon_core::pilot_message::PilotMessage`; `PilotCheckpoint` and
`DriftFinding` landed in M3 as the `cosmon-pilot-checkpoint` crate; `PilotLease`
landed in M4 (`task-20260731-9cf4`) as `cosmon_core::pilot_lease::PilotLease`,
with `LeaseEpoch`, `LeaseRequest` and the `authorize` guard beside it and
`cosmon_filestore::PilotLeaseStore` behind it. `PilotPresence` is the existing
`Presence` struct plus six fields — eight after M4 added `mission` and
`lease_epoch`, which are the two halves of the claim the guard checks; the
other four records are new files under `.cosmon/state/`, each one line of
JSON, each readable with `cat` and `jq`.

M4 added one record the sketch below does not name: `LeaseRequest`. The
schema has `PilotLease` and no way to ask for one, and D6 requires that a
pilot may *request* while only an operator grants. Those are two writers, so
they are two records in two files — `pilot-lease/<mission>.requests.jsonl`
written by pilots and `pilot-lease/<mission>.grants.jsonl` written by the
operator. Keeping the ask out of the authority ledger is what makes the M4
crash clause hold by construction rather than by care: a process killed
between the two has appended to the first file and not the second, and a
transfer is one append, so there is no half-transfer state to recover from.

M5 (`task-20260731-e4d0`) added **no record at all**, which is the claim it
was supposed to be able to make: `cs sessions` is a cockpit over the five
registries above and writes nothing they do not already own. It reaches them
through data-level entry points on `cs presence` rather than through a second
copy of the ordering rules — one writer per file, one place where an ack lands
after the text has left the process, one place where a seat is checked against
the ledger before it is written. `cs presence`, `cs session` and `cs pilot`
keep their bytes; the plural verb is the third thing D3.5 said it would be.

M6 (`task-20260731-0d49`) added **two files and no registry**, both under
`.cosmon/state/pilot-hooks/` and both owned by one session: `<sid>.cost.jsonl`,
the append-only measurement the mission's *coût mesuré* clause asks for, and
`<sid>.draft.json`, the single staged checkpoint the hook publishes at a
transition. Neither is a registry: the draft is overwritten by each
`cs sessions checkpoint stage` and deleted when it is published, and the ledger
is read only by `cs sessions hook status`. The checkpoint that lands is
`CheckpointStore`'s, in the shape M3 defined; staging is a *delay*, not a
second dialect — `stage` and `publish` build the record from the same flags
through the same function.

The division that made this possible is worth naming, because it is what keeps
the hook inside D6. A hand-over record's **content** is the pilot's — its
hypotheses, its intended next actions, its unresolved questions — and a hook
knows none of it. Its **moment** is the hook's, and that is all the hook
contributes. A hook that filled in the content would publish a checkpoint whose
author never held those positions, and `cs sessions drift` would then compare it
as though a mind were behind it: the opaque score D3.4 refuses, arrived at from
the other direction. So no draft means no publication, said once on stderr.

Two more properties the implementation had to choose, and did:

- **The mailbox is drained only where the pilot can read what comes out.**
  Claude feeds a `SessionStart` and `UserPromptSubmit` hook's stdout to the
  model and discards a `Stop` hook's. Acknowledging an envelope at a moment
  whose output is discarded would consume a message and show it to nobody —
  at-least-once delivery turned into a shredder. `stdout_reaches_pilot` is that
  rule, and it is why `turn-end` is the checkpoint moment and `turn-start` the
  mailbox one.
- **The heartbeat carries no `--role`.** Every co-pilotage field is carried
  forward from the snapshot the operator wrote. A hook pinging `--role primary`
  every thirty seconds would be a takeover nobody decided, executed by a process
  nobody is watching — precisely what D6's second bullet reserves for an
  operator gesture.

M3 records `lease_epoch` as a bare `u64` and M4's `LeaseEpoch` serialises
transparently as one, so the two agree on the wire without
`cosmon-pilot-checkpoint` taking a dependency on `cosmon-core` it does not
otherwise need. The guard is deliberately *not* wired to checkpoint
publication: D6 lists checkpointing among what a co-pilot may do.

```text
ProviderSessionRef {
  provider,                 # "claude" | "codex" | …
  native_session_id,        # Claude: log filename == record .sessionId
                            # Codex:  session_meta.payload.session_id
  repo_identity,            # resolved galaxy root, NOT a substring match
  cwd,                      # read from inside the log, never decoded from a dir name
  source_locator,           # absolute path of the provider log
  optional_display_name,    # alias only — never used to break a tie
  started_at, last_observed_at
}

PilotPresence  = Presence + { provider, native_session_id, role, follows,
                              capabilities, checkpoint_id }
                 role ∈ { PRIMARY, COPILOT }

PilotLease     { mission_id, holder_session_id, epoch, granted_by,
                 granted_at, expires_at, request_id? }

LeaseRequest   { id, mission_id, candidate_session_id, observed_holder?,
                 observed_epoch?, requested_by, requested_at, reason }
                 # added in M4 — the ask D6 requires and the sketch omitted

PilotCheckpoint{ id, mission_id, session_id, lease_epoch, scope,
                 current_hypotheses, evidence_refs, completed_actions,
                 intended_next_actions, open_risks, unresolved_questions,
                 created_at }

PilotMessage   { id, from, to, sequence, payload_ref, payload_hash,
                 created_at, read_at, expires_at }

DriftFinding   { id, checkpoint_a, checkpoint_b, class, cited_claims,
                 evidence_refs, verdict ∈ {FINDING, AGREE, INCONCLUSIVE},
                 created_at }
```

Payloads larger than a line are content-addressed files, as `cs whisper`
already does for molecule whispers. Registries carry envelopes and references
only.

### D6 — Authority model

One sentence: **authority is a lease with an epoch, and every mutation carries
the epoch it believed it held.**

- A mission has at most one valid `PilotLease`. `role: COPILOT` is read-only for
  operator gestures — it may observe, message, checkpoint and publish findings,
  and nothing else.
- Only the operator grants or transfers a lease. A pilot may *request*. Neither
  pilot, nor a quota reading, nor a heartbeat gap executes a transfer.
- A transfer increments `epoch`. A mutation presenting a stale epoch is refused
  **before** it takes effect, not compensated afterwards.
- Unknown session, unknown lease or unknown epoch ⇒ read-only. There is no
  default authority and no authority inherited from a timeout.
- Reuse of the existing `hold:pilot` tag is deliberate: `cs claim` already makes
  the runtime defer unconditionally on a molecule. The lease is the same idea
  raised from one molecule to one mission, and it does not replace the tag.

The `hold:pilot` tag as it stands has no holder and no epoch — it is a boolean.
It stays a boolean. The lease is a separate record; M4 wires the guard, and the
guard is what refuses, not the tag.

**What M4 wired the guard to, and why that gesture.** The model's rights are
unchanged and `cs done` is no more autonomous than it was: the guard went on
`cs presence ping --role primary`, the one existing gesture that *claims* the
authority the lease grants. Before M4 that flag was a self-declaration anyone
could write, so two sessions could both render as PRIMARY in a directory scan
and falsifier 2 was reachable without a single line of new code. It is now
checked against the ledger before the snapshot is written — refused before it
takes effect, per the third bullet above — and the claim itself is recorded on
the snapshot as `mission` + `lease_epoch`, so a stale primary is *readable*
rather than merely wrong.

Two consequences the implementation had to choose, and did:

- An explicit `--role primary` the ledger refuses is an **error** that writes
  nothing. A gesture is refused as a gesture.
- A *carried-forward* primary — the hook's bare heartbeat, every ~30 s, with
  no flags — that the ledger refuses is **demoted** and the heartbeat still
  lands. Failing it would blind the fleet to a session that is very much
  alive, which trades a split-brain for a false death. The demotion is
  announced on stderr, so it is visible without being fatal.

That second rule is why the former primary loses the seat without anybody
having to tell it: its own next heartbeat reads the ledger and steps down.

## Falsifiers

This ADR is wrong if any of the following turns out to be true. Each is checkable.

1. A Claude session log publishes a proactive quota signal (an approach, not an
   error) somewhere this survey missed. Then D3.1's *empirical* leg falls, and
   only the doctrinal leg (TAKEOVER-SUPERVISED) holds.
2. Two pilots hold a valid lease on one mission at the same instant.
3. A pilot mutates after its epoch has been superseded.
4. Two unnamed sessions in the same cwd resolve to the same
   `<provider>:<native-session-id>`.
5. A worktree is selected because its path contains the galaxy name as a
   substring.
6. Observing a session appends a byte to its log, sends a key to its pane, or
   changes its conversation.
7. A duplicated message produces two actions, or a crashed reader loses a
   message that was never consumed.
8. A missing checkpoint is rendered as `AGREE`, or an unresolvable session is
   rendered as anything other than `INCONCLUSIVE`.
9. Codex can only resume by re-reading the whole Claude transcript.
10. Adding a third provider requires editing `cs sessions` rather than adding an
    adapter and fixtures.

Falsifiers 2–10 are the mission's own, restated so this ADR can be checked
without the brief in hand. Falsifier 1 is new and belongs to this ADR alone.

## Consequences

- **M1 gains a hard entry criterion it did not have:** the session-probe port
  must expose a byte cursor and survive a partial trailing line, truncation and
  rotation, because `claudion` today does none of the three (P7).
- **M2 and M3 each start with a failing test**, from D4. A green `cs diverge`
  on an unresolvable session is the exact shape of the bug the co-pilot exists
  to catch; shipping the co-pilot on top of it would be self-defeating.
- **M4 loses a tempting shortcut.** There is no quota-watching automation to
  build, on either side. The transfer surface is `request` / `grant` and
  nothing else.
- **The traces set the confidentiality ceiling for the whole mission.**
  Envelopes with pseudonymised identifiers and redacted paths cross into
  cosmon; content does not. Every later fixture inherits that rule.
- **Nothing in this ADR is executable.** No flag, no verb, no output byte
  changed. The next molecule that touches a user-facing command owes the
  CLI/UI parity audit, `cs help` and `man cs` in the same change.
