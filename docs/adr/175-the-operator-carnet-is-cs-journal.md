# ADR-175 — The operator carnet is `cs journal`

**Status:** Accepted (2026-08-10).
**Date:** 2026-08-10.
**Decider:** Noogram.
**Authoring molecule:** `task-20260809-a57a` — child C1 of the deliberation.
**Deliberated by:** `delib-20260808-3ebc` (five-persona panel, deep-think 4/4).

**Scope:** one CLI verb, its store directory, and the surfaces that spell
either of them. No behaviour changes: `start / note / end / promote / route /
review` do exactly what they did, on files of the same format.

**Binds:** ADR-061 (pilot session and causal closure), ADR-072 (session-route
formula and sidecar invariants), ADR-078 (session-route for utterances),
ADR-168 (a co-pilot inherits the session substrate), ADR-173 (a cockpit is a
command surface).

---

## 1 · Context

Two unrelated surfaces answered to one word, one being a strict prefix of the
other:

| Command | Referent |
|---|---|
| `cs session` | the operator's carnet — an append-only, BLAKE3-sealed markdown file the human writes into while working |
| `cs sessions` | the co-pilotage cockpit — two agent sessions on one mission, presence, mailbox, checkpoint, takeover |

This is the only strict prefix-extension in the ~95-word `cs` namespace whose
two members name unrelated things and which is not separated by a hyphen. The
hyphenated extensions in the namespace (`verify-trace`, `release-audit`) carry
a visible family marker; `session → sessions` carries a plural that promises
"more of the same thing" and delivers a different thing. Shell completion
cannot disambiguate a bare prefix, so the tooling has no way to render the
distinction even in principle.

The panel found unanimously (5–0) that this is a design defect rather than a
cosmetic annoyance. It adjudicated 3–2 that the *carnet* is the side to move:
the cockpit's own word, `cockpit`, is unavailable — `crates/cosmon-cockpit`
and `cosmon-cockpit-http` are the web UI, and adopting it would resolve one
two-referent overload by creating another.

## 2 · Decision

**The carnet is `cs journal`. The cockpit keeps `cs sessions`.** Its store
moves from `.cosmon/state/sessions/` to `.cosmon/state/journals/` in the same
commit.

`journal` is an ordinary English noun for a register kept over time, which is
what the surface is. No other subcommand begins with `j`, so the prefix is
unique and completion resolves it at one character.

### 2a · A correction to the deliberation: the francophone precedent is false

The synthesis recommended `carnet`, answering the objection that the word
denotes nothing to an English reader by asserting that the register is
"already non-Anglophone by design — `cs mur`, `cs ensemble`, `cs spore`,
`cs quench`".

**That precedent was checked and is false.** `cs mur` does not exist in this
CLI. The remaining three are English words: an *ensemble* and a *spore* are
English nouns, and *quench* is an English verb meaning precisely what the
command does. So is the whole physics set — *nucleate, evolve, collapse,
freeze, thaw, entangle, observe*. Every one of the ~95 verbs is an English
word. `carnet` would have been the only non-English name in the namespace, and
its actual English sense is a customs document — a false friend pointing at
the wrong object.

The objection the synthesis was answering therefore stands unanswered, and the
operator chose `journal` instead: it satisfies what `carnet` was picked for
(the project already calls this surface a carnet in 114 places, and a journal
is what a carnet *is*) without introducing the only foreign word in the
namespace.

The French word **carnet** remains correct and is deliberately kept in French
prose and in existing comments. It names the artefact; `journal` names the
command.

### 2b · The summary line is part of the decision, not a follow-up

The old line — *"Session — operator carnet (start/note/end), append-only,
BLAKE3-sealed"* — spent its first word restating the command's own name and
its last on a hash function, and never mentioned `promote / route / review`,
which is the reason the surface exists: a note becomes a molecule without the
operator stopping to file one. A rename that kept that line would fix the
collision and leave the surface unused for the same reason it went unused for
a year. The shipped line is:

> Write down what you notice while you work; anything worth doing becomes a
> task without you stopping to file it.

## 3 · Transition: a hidden, working alias

`Session` is retained as `#[command(hide = true)]`, fully functional,
delegating to the same handler, and emitting one deprecation line on stderr.

**It is never made visible.** All five panelists independently ruled that a
*visible* alias preserves the confusable token in `cs help` and in shell
completion — which is the defect itself. The help goldens assert the absence:
`cs --help` lists `journal` and not `session`.

It exists for exactly one reason. `~/Applications/mac-pilot.app` is installed
on the operator's machine, hardcodes `["session", …]` in
`apps/mac-pilot/mac-pilot/CosmonBridge.swift`, and **no cargo gate crosses
that Swift boundary** — `just gates` is cargo-only. A tombstone would have
broken an installed app silently, at a moment nobody was looking.

**Removal trigger — observable, and able to fire.** Delete the hidden verb in
the first commit after *both*:

1. `grep -rn '"session"' apps/mac-pilot/` returns zero, and
2. `just install-mac-pilot` has been run against a tree containing this
   rename, verified by one journal note written from the menubar.

Both are checkable in under a minute. This deliberately replaces a
date-shaped trigger: ADR-052 §D3 set four of those on 2026-07-17, and on
2026-08-09 **all four aliases still existed and 0 of 4 carried `hide = true`**.
A date does not fire. A grep does.

## 4 · What was deliberately not touched

- **`crates/cosmon-session-probe`.** It names the *substrate*: provider
  transcripts on disk really are sessions (`claude-session.jsonl`,
  `codex-session.jsonl`). Renaming it would falsify a true name.
- **The HTTP namespace `/session/*`** in `cosmon-api`. Different namespace, no
  sibling, no collision. The adapter's argv now spells `journal`, because that
  crosses a cargo gate and the wire contract does not.
- **The file prefix `session-<ts>.md` and the `session_id` field.** They name
  a work session, which is a true thing; the *command* names what the operator
  does with it. Changing them would move a format, not a name.
- **The bodies of ADR-061 / 072 / 078 / 168 / 173.** An ADR records a decision
  as it was made; rewriting one falsifies the record. This ADR is the
  amendment, which is why it exists.

## 5 · Consequences

- The `state/sessions/ → state/journals/` move is a hard cutover with no
  read-both shim. It is free **only** because zero carnets exist anywhere on
  disk — verified before the change, the directory did not exist. It is never
  free again: the first galaxy with a sealed carnet turns this into a
  migration.
- The window for renaming at all closes at `0.6.0`. Once a shipped binary in
  someone else's hands carries the verb, the answer becomes keep-both
  permanently. Acting now is the entire argument.
- The seven `help__session*` goldens were **deleted**, not merely superseded.
  `INSTA_UPDATE=always` writes the new files and leaves the old ones on disk,
  green and unreferenced, and no gate in `just gates` catches a stale
  snapshot.

## 6 · What would show this was the wrong fix

Adopted from the panel, recorded so it can be checked rather than assumed:

**The cross-surface correction test.** Define the event as an invocation of
one surface that errors on a subcommand belonging to the sibling, *or* is
followed within 120 s by the same verb re-issued against the sibling. Once
both surfaces reach ≥30 invocations after this change, **≥2 such events**
would show the confusion was conceptual rather than lexical — the rename
bought nothing and what is owed is better descriptions, not a second rename.
Precondition, still unverified: the `operator.*` envelopes in
`crates/cosmon-cli/src/operator_event.rs` must carry the invoked command name.

## 7 · What this does not settle

`cs sessions` remains named after its operands rather than its function, and
it overlaps `cs presence` — same operand, overlapping verbs (`send`, `inbox`,
lease), same ADR-168. The collision is gone; the mis-registration is not.
That is a separate deliberation (child C4 of `delib-20260808-3ebc`), and
nothing here claims the cockpit is well named.
