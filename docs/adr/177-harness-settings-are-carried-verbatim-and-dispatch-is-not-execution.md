# ADR-177 — Harness settings are carried verbatim, and dispatch is not execution

**Status:** Accepted (2026-09-10) — the contract for issue #65, written before
any adapter is touched.
**Date:** 2026-09-10.
**Decider:** Noogram.
**Authoring molecule:** `task-20260910-346a` (P4 of the #65 polymer).
**Entry artefacts.** GitHub issue #65 (*per-step harness settings across
adapters*); `delib-20260910-9d1b` **Décision 2** (the seam) and **Décision 3.2**
(why the contract is a doc-only step rather than a spore). The shipped
precedent this record extends is the four-level model chain
([`ModelSelectionSource`](../../crates/cosmon-core/src/event_v2.rs)) and its
ex-ante / ex-post event pair, `ModelSelected` / `ModelObserved`.

**What this record is.** A contract, not an implementation report. Nothing
described here exists in `crates/` yet; the implementation is the sibling
molecule P5. This document is deliberately first: the panel found that #65's
real risk is **a second mechanism merged by accident**, and that risk is retired
by writing the precedence table and the event schema down *before* an adapter
acquires a new argument — not by adding review gates around the writing.

---

## Context

cosmon can pin *which model* a formula step runs on. It cannot pin *how* that
model runs. Codex's `model_reasoning_effort` stays at whatever the machine's
`~/.codex/config.toml` says; Claude Code's `--effort` is never passed;
opencode's `--variant` is unreachable. A spore that ships to a recipient
therefore depends on the recipient's machine-wide harness config being right for
each node — which is the one thing a spore exists to stop depending on.

Issue #65 proposes a `[steps.harness]` map on the formula step, carried to each
adapter's own override channel. The seam is right. Five things about it are
settled here, because each one is cheap to write now and expensive to withdraw
later.

---

## Decision 1 — The map is opaque, and cosmon recognises **zero** keys

The harness map is carried **verbatim** from its source to the adapter's native
override channel, and logged **verbatim as sent**. cosmon never normalises a
key, never rewrites a value, never summarises the map, and never maintains a
list of keys it accepts.

Issue #65's proposal to give the `claude` adapter an allowlist
(`effort`, `settings`, `fallback-model`, `max-budget-usd`, "reject unknowns
loudly") is **dropped**.

**Why.** The moment cosmon recognises a key, that key set becomes public API —
and it is public API carried in files on other people's disks. *An allowlist is
a promise you cannot version.* Spores are not in cosmon's dependency graph and
`cargo semver-checks` cannot see them, so there is no mechanism by which
removing a recognised key can be announced to the artefacts that use it. An
unknown key already fails in the harness's own parser, at launch, loudly — which
is where the knowledge about that harness's keys actually lives and stays
current.

Note also that for `codex` the allowlist would be **vacuous**: `-c key=value`
accepts any dotted key and TOML-parses the value, so there is no unknown-key set
to reject against. cosmon would be enumerating a set the harness does not have.

The verbatim rule is not fastidiousness. A verbatim record is the only thing
that can later be **diffed against the harness's own echo** (Decision 5). A
normalised record proves that cosmon's normaliser ran, and nothing else.

**Its falsifier.** Exhibit one key that cosmon must *interpret* in order to
route correctly — two adapters accepting the same key name with incompatible
meanings. Carriage is then not sufficient and a typed subset was required.

## Decision 2 — Precedence is `flag > step > adapter default`, merged per key

Three levels, resolved in order, **merged key by key and never replaced
wholesale**:

| Rank | Level | Where it lives | Shape |
|---|---|---|---|
| 1 | CLI flag | `cs tackle --harness k=v` (repeatable) | the operator's in-the-moment choice; always wins, per key |
| 2 | Formula step pin | `[steps.harness]` on the executing step, beside the existing `model` / `adapter` (`crates/cosmon-core/src/formula.rs`) | the per-workflow override a spore carries |
| 3 | Adapter default | whatever the harness's own config says (`~/.codex/config.toml`, Claude Code settings) | cosmon pins nothing; the harness decides |

This is the same shape as the shipped `model` chain, one level shorter: there is
no env level and — see Decision 3 — no cosmon config level in the first PR. Rank
3 is not a cosmon surface at all; it is the floor, and the floor is *silence*.
cosmon passing no key for `k` is the only way the harness's own default can
apply, exactly as `ModelSelectionSource::Default` resolves to `None` rather than
to a named model.

**Per-key merge is the load-bearing half.** A step that raises effort must not
thereby drop a budget cap set at another level. Wholesale replacement makes
every level a complete restatement of every other, which is how a two-key map
silently loses a key.

## Decision 3 — The config level is **deferred to its own PR**, and here is the verified reason

`[adapters.<name>.harness]` — level 3 of #65's part B — is **not** in the first
PR.

Issue #65's own table states that `extra_args` is *aider-only and config-only*.
**That is false**, and the correction changes the design. Verified in this
worktree at `ea778e3a`:

- `crates/cosmon-core/src/config.rs:1834` — `pub extra_args: Vec<String>` is a
  field of the **generic `AdapterEntry`** (the `[adapters.<name>]` row), not of
  an aider-specific struct. Its own doc comment states the semantics: for the
  `codex` adapter in interactive mode a non-empty row **replaces** the built-in
  defaults *verbatim* — replace, not merge.
- `crates/cosmon-cli/src/cmd/tackle.rs:5811-5812` — `spawn_codex_and_prompt`
  reads `adapter_entry.extra_args` and threads it into
  `CodexSessionConfig.extra_args` (`tackle.rs:5860`) on **every codex spawn**.

So a per-key-**merge** `[adapters.<n>.harness]` table, sitting on the same
`[adapters.<n>]` config node as a wholesale-**replace** `extra_args` table, both
reaching the same command line, is exactly the **second mechanism** #65's own
acceptance criterion forbids ("aider's `extra_args` is reachable through the
same map — no second mechanism"). Two adjacent keys with opposite merge
semantics is not a naming problem; it is two mechanisms.

Folding `extra_args` into the harness map is therefore a **migration on a
documented, in-use surface** and needs a deprecation window. It is not a
first-PR line item, and deferring it costs a spore author nothing: the flag and
the step pin already cover every case the issue motivates.

**Its falsifier.** `grep -rn "extra_args" --include=*.toml` across the galaxies
and `docs/`. If no tracked config uses it and no galaxy relies on the replace
semantics, the fold is a free rename rather than a migration, and the config
level should return to the first PR.

## Decision 4 — The acceptance claim is **"dispatched at"**, never **"ran at"**

Issue #65's acceptance line reads: *"a sporarium acceptance run has to be able
to prove, from `events.jsonl`, that a step **ran** at the effort it was pinned
to."*

**As written that is unfalsifiable, and it is corrected here to "was
*dispatched* at."**

`selection_source` records which branch of the resolver fired. It is minted
*before the process exists*. It cannot be evidence about a process that has not
been spawned — no observation of the resolver can refute a claim about the run,
because the resolver's output is identical whether the harness later honours the
flag, clamps it, ignores it, or crashes.

This repository has already solved this once, for `model`, and the vocabulary is
reused rather than reinvented:

| Axis | Event | Nature | Failure of silence |
|---|---|---|---|
| Ex-ante (intention) | `ModelSelected` — carries `selection_source` | what cosmon asked for | `model: None` means *nothing pinned it* |
| Ex-post (realization) | `ModelObserved` — parsed from the harness's own log | what the harness said it did | **the event is simply not emitted** |

`ModelObserved.model` is a bare `String`, never an `Option`, precisely so that
"ran but unknown" has no representable value (`crates/cosmon-core/src/event_v2.rs`,
the honesty invariant). Effort inherits the same structure, and the same doc
sentence names the honest floor for the ex-ante half alone: *intended, not
confirmed*.

**The strongest sentence an acceptance run may print** is therefore:

> cosmon requested effort *E* through channel *C*; the harness's own log
> reported *E*.

That is a **two-party agreement**, not a proof of behaviour. Whether the echo is
truthful, and what `high` denotes in actual computation, live inside the oracle
and are not closable from here. Any acceptance text that claims more than the
sentence above is claiming something no artefact in this repository can support.

## Decision 5 — The event schema

### Ex-ante — on the existing selection receipt

Alongside `selection_source`, a harness-settings dispatch records, per resolved
key:

| Field | Why it exists |
|---|---|
| `selection_source` | which level of Decision 2's table fired — the same enum shape as `ModelSelectionSource`, so `flag` and `formula_pin` carry their origin (flag text; formula + step id) |
| **realized argv fragment** | the bytes actually appended to the command line (`-c model_reasoning_effort="high"`), verbatim as sent. This is the only field that can be diffed against the echo. |
| **channel** | *which* override surface carried it — codex `-c`, a claude flag, a settings file. Two keys of the same map may leave through different channels; a reader must not have to infer which. |
| **harness version** | the harness's self-reported version at spawn. An echo is only interpretable against the version that produced it, and flag semantics move between releases. |
| **launch status** | whether the process came up at all. A harness that rejected a flag at launch produces *no* echo, and that absence must not be readable as "the setting was silently accepted". |

**Fail closed at launch.** If the harness rejects a flag, the dispatch fails and
names the adapter — the same posture as the illegal adapter/model pair today. A
setting is never silently dropped.

### Ex-post — the echo, and the cheapest real evidence available

Codex already writes, per turn:

```json
{"type":"turn_context","payload":{"model":"gpt-6-astra","effort":"high"}}
```

cosmon already reads that exact line — for `model` only. `CodexPayload`
(`crates/cosmon-core/src/model_realization.rs:271-274`) declares a single field,
`model: Option<String>`, and `CodexLine::TurnContext` is already wired to the
per-turn record.

The schema therefore specifies:

1. `effort: Option<String>` added to `CodexPayload`, read from the same record
   the model already comes from.
2. An **`EffortObserved`** sibling event, structured on `ModelObserved`: the
   effort field is a bare `String` (silence is the *absence of the event*, never
   a value meaning "unknown"), scoped by `worker_id` and `adapter_name`, emitted
   on the first turn carrying a concrete value and re-emitted only on change.
3. The realized axis is **never back-filled from the pin or the config** — the
   `reasoning_effort_is_never_inferred` discipline that
   `crates/cosmon-core/src/adapter_attribution.rs` already enforces for the
   model axis, applied to the axis it was named after.

For adapters with no echo, the ex-post half does not exist, and the record must
say so plainly rather than implying more: what remains is the version probe and
the launch status.

**Its falsifier.** Run codex once with `-c model_reasoning_effort=high` against
a `~/.codex/config.toml` set to `low`, and read the `turn_context` records. If
`payload.effort` is absent, or reports `high` while the session demonstrably
behaved as `low`, the echo is not evidence, the ex-post half does not exist for
codex either, and Decision 5 reduces to its ex-ante table.

## Decision 6 — The portable `reasoning_effort` alias is **deferred**, with its constraints written now

Part C of #65 — one cosmon-level key that each adapter translates — is deferred,
not refused. It is the only part of #65 that **mints permanent public
vocabulary**, and the only part that can be added later without breaking
anything. That asymmetry is the whole ordering argument. The raw map already
satisfies the concrete case the reporters hit (`model_reasoning_effort=high` on
codex), so the user need is met without minting a word that can never be
withdrawn.

The constraints are recorded here so that whoever re-opens it inherits them
rather than rediscovering them:

1. **It may *select*, never *promise*.** No claim of equivalence between
   adapters. The resolved **native** pair is what lands in the trace and in
   `events.jsonl`; the alias vanishes at the boundary and appears in no
   downstream artefact.
2. **Two losses, written rather than discovered.** Claude Code's real vocabulary
   is `low | medium | high | xhigh | max`. Through a `low | medium | high | max`
   alias, **`xhigh` is unreachable** and **`max` is a lossy cell**. A mapping
   table that does not state its own lossy cells is a promise of the kind clause
   1 forbids.
3. **It must never degrade to a silently-ignored soft advisory** on an adapter
   with no effort knob. #65 proposes exactly that, by analogy with the
   cross-family model pair. A silently-ignored effort pin is the precise failure
   mode Decisions 4 and 5 exist to kill; an adapter that cannot honour the alias
   must refuse it at launch, naming itself.

**The alternative to adopt if the deferred alias is re-opened and found
unworkable:** no alias at all — raw keys plus a lint that names the native
effort key per adapter. A lint carries knowledge without carrying a promise, and
can be corrected without breaking a spore already on disk.

---

## Scope fences

**opencode is out of #65.** It is out for a reason that is worth stating rather
than eliding: `crates/cosmon-transport/src/opencode.rs` passes no `-m`.
`OpencodeSessionConfig` (L80-98) has no model field at all, `spawn_opencode_session`
builds `opencode run [prompt]` and nothing more (L114-139), and the call site
`spawn_opencode_and_prompt` (`crates/cosmon-cli/src/cmd/tackle.rs:4209`) is the
only adapter arm in that dispatch that is not handed `preferred_model` — every
sibling arm (claude, codex, aider) receives it. The opencode adapter therefore
**silently drops the `--model` pin that the shipped four-level model chain
honours everywhere else**. That is a *correctness defect against a shipped
feature*, not a missing enhancement, and it belongs in its own small PR that can
merge before #65. Bundling a bug fix inside an enhancement hides it from the
changelog and from anyone bisecting.

**deepseek-harness is out.** A Web-UI plugin framework in developer preview is a
different adapter *shape* — an HTTP/session question closer to `SupervisionMode`
than to spawn flags. It is tracked separately, once its CLI surface stabilises.

**Also explicitly deferred**, listed so no reader mistakes silence for oversight:

| Item | Why |
|---|---|
| `reasoning_effort` alias (Decision 6) | mints permanent public vocabulary; addable later without breaking anything |
| `[adapters.<n>.harness]` config level + the `extra_args` fold (Decision 3) | a migration on a documented in-use surface; needs a deprecation window |
| deepseek-harness | a different adapter shape |

---

## Consequences

**For P5, the implementation molecule.** Its scope is exactly A + B(flag) + D:
the verbatim map on the formula step, `cs tackle --harness k=v`, the three-level
per-key merge of Decision 2, and the full event schema of Decision 5 including
the `CodexPayload.effort` field and the `EffortObserved` sibling. D ships *with*
A and B or A is unverifiable in the field — the audit surface **is** the
falsifier infrastructure for the carriage.

**For the spore-format docs.** #65 asks for a one-line note that
`[steps.harness]` is carried opaquely and does not affect the TLA+ seal (model
pins already do not). That note belongs in the PR that makes the map exist:
documenting a user-facing format key before it is readable is a statement the
repository cannot honour. It is P5's obligation, recorded here so it is not
lost.

**What this record costs if it is wrong.** Each decision above carries its own
falsifier, and three of them are cheap enough to run in an afternoon. The
expensive failure mode is the one this document exists to prevent: shipping a
per-key-merge harness map beside a wholesale-replace `extra_args` on the same
config node, discovering the collision after a release, and owning both
mechanisms forever.
