# ADR-071 — `cs ask` conversational ingress (rule-first MVP)

**Status:** Accepted
**Date:** 2026-04-23
**Parent:** delib-20260423-95fe (urgent-reflex deep-think — architect, niel, jobs, godin, jr)
**Sibling:** ADR-070 (`cosmon-registry` — prerequisite for galaxy resolution)
**Supersedes:** none — this verb is additive

## Context

Under time pressure the operator still types `claude` inside
`/srv/cosmon/<name>/` instead of `cs …`. Current cosmon ingress
(`cs nucleate <formula> --kind <k> --var topic="…"` then
`cs tackle <id>`) costs ~80–120 keystrokes, ~800 ms, and one
formula-choice decision. `claude /project` costs ~5 keystrokes,
~200 ms, zero formula choices — and the operator still reaches for
it by reflex.

The panel (delib-20260423-95fe) converged on one remedy: a single
verb that takes free text, auto-selects formula + galaxy, and
dispatches with zero ceremony. Target for the MVP:

* Five prefix keystrokes (`cs a "`).
* Sub-50 ms to the first worker action on the rule-path.
* Zero formula choice.

## Decision

Introduce `cs ask "<free text>"` as a **write-only composition of
existing verbs**. No daemon. No mailbox. No LLM in the MVP.

The pipeline lives in `crates/cosmon-ask/`:

```text
cs ask "<text>"
  │
  A. Parse via RuleParser (table-driven, ≤ 5 ms)
  │   → (AskTokens, confidence)
  │
  B. Resolve galaxy via cosmon-registry
  │   → Galaxy { name, path, fleet, default_formulas }
  │
  C. Confidence gate ≥ 0.85
  │   │
  │   └── below floor → AtomicQuestion (default + named verdicts)
  │
  D. Dispatch (CLI layer, shell-out):
       cs nucleate <formula> --var topic="…" --tag temp:hot
       cs tackle <mol_id>
       (cs wait / cs done stay the operator's responsibility)
```

Typestate (`cosmon_ask::AskState`):

```rust
Parsed { tokens, confidence }
  ─► AskedClarification { reason, question }  (low confidence or unknown galaxy)
  ─► Resolved { galaxy, formula, vars }
  ─► Dispatched { mol_id, worker_id }        (CLI records after shell-out)
```

The rule table (first cut, embedded at compile time in
`crates/cosmon-ask/rules.toml`) carries ~12 intent buckets: `fix /
patch / debug`, `ship / deploy / release`, `triage / review /
audit`, `plan / roadmap / rollout`, `design / architect / spec`,
`delib / deliberate / panel / think`, `chronicle / record /
capture`, `draft / write / author / compose`, `refactor / clean /
tidy / polish`, `explore / investigate / study / probe`, `map /
survey / inventory`, `note / remember / jot`. Declaration order
encodes priority.

Per-galaxy `default_formulas` (from `cosmon-registry`) override the
rule-anchored formula when present — that is how e.g. `mailroom`
would steer `issue` intents toward `bug-closure` instead of
`task-work` without touching the rule table.

The CLI verb is gated behind `--experimental` until telemetry
confirms a hit-rate ≥ 70% on rule-path invocations. Every
invocation appends one NDJSON line to `.cosmon/state/ask.jsonl`
(the audit log).

## Failure modes and mitigations (architect §4)

| Failure | Mitigation |
|---------|-----------|
| Misrouting (wrong galaxy) | Confidence gate ≥ 0.85 always prompts below; explicit galaxy hint in text lifts confidence by +0.05 (capped at 0.99). |
| Runaway dispatch | `--execute` is explicit; atomic-question path surfaces `queue / override / abort` when `cs ensemble --running` ≥ 3 (future wiring). `~/.cosmon/ask.off` is the hard kill-switch. |
| Latency cliff | Rule-path has no I/O beyond registry TOML read. The 400 ms LLM budget is **reserved for the follow-up molecule**, not spent here. |
| Silent drift between rules and intents | Audit log is append-only NDJSON; `jq` over a sliding window tells the operator when new intents are slipping through as zero-confidence. |

## Invariants preserved

* **Stateless core (ADR-054).** No daemon, no background loop, no
  persistent process. Every invocation re-loads registry + rules
  from disk.
* **Three regimes (ADR-016).** `cs ask` dispatches into Propelled
  via `cs tackle`, exactly as today's flow does.
* **Merge-before-dispatch.** Inherited unchanged from `cs tackle` /
  `cs done` — `cs ask` is not a new state machine, just a new
  composition layer.
* **CLI-over-MCP for workers.** The LLM fallback (future) is an
  internal library call; it is never an MCP roundtrip to cosmon.
* **One question, one decision.** The atomic-question surface
  preserves the `1 default / 2 / 3 / later` verdict-door pattern
  (feedback memory `feedback_one_question_one_decision`).
* **Write-read asymmetry (§3b).** `cs ask` writes (audit log,
  molecule via `cs nucleate`); it never reports coupling in the
  same invocation.

## Explicit non-goals

* No runtime daemon. If cold-start latency exceeds 50 ms on typical
  rule paths, file a **decision molecule for daemon arbitration** —
  do not add a daemon in a follow-up PR. ADR-054 stands.
* No LLM parser in the MVP. A follow-up molecule will add
  `LlmParser` (Haiku, 400 ms hard budget) as a low-confidence
  fallback. The trait signature (`Parser::parse(&self, text) ->
  Result<(AskTokens, f32)>`) is shipped today so the pipeline can
  compose them later without a breaking change.
* No mailbox / live dialogue channel. ADR-038 (whisper) and ADR-066
  (wheat-paste viewport) own that plane.

## Consequences

* **Positive.** Operator types `cs a "fix the bug in mailroom"`
  and gets a Propelled worker in under a second, with zero formula
  choice. The rule table is a single TOML file — new intents land
  as data edits, not code.
* **Negative.** Out-of-vocabulary intents degrade to an atomic
  question with 0-confidence — the operator still has to
  disambiguate. The follow-up LLM fallback is the planned relief.
* **Neutral.** The verb is additive; nothing existing changes. If
  the MVP fails its telemetry gate (hit-rate < 70% over 30 days), we
  remove the verb with a single-PR revert.

## Rollback plan

Delete `crates/cosmon-ask/`, remove the `cmd::ask` wiring in
`cosmon-cli`, drop this ADR to `Superseded` status. No migrations,
no state schema changes, no breaking API surface.
