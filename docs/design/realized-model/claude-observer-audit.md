# Claude observer audit — issue #73

Measured on 2026-09-14 by molecule `task-20260914-c10b`, against base
`a90cfaf6`. The durable molecule `result.md` holds the detailed evidence,
verification results, and freshness proposal. No private log contents, session
identifiers, or usage figures are included here.

## Finding

**Conditional:** the measured Claude grammar does not share the Codex
switch-record blind spot. A model change is visible on the next assistant
record's `message.model`. Missing or unrecognized later evidence can still
leave an earlier model looking current; the parser does not assess freshness.

A read-only scan covered the local Claude Code session logs, top-level and
subagent logs alike. The live store was not frozen; the scan read what it
encountered, not an atomic snapshot.

Every assistant record in the scanned logs carried a non-empty string
`message.model`. Observations refer to **records**, not unique responses or
human turns: a response can produce multiple records.

Some assistant records carry the placeholder model value `<synthetic>`; these
are excluded from transitions. That placeholder is a separate limitation:
`ModelId::new` checks non-emptiness, not whether the value identifies a real
model.

Excluding the placeholder, the scan found real mid-session transitions between
concrete models. In at least one real session, two assistant records have
different model values and message identifiers, the same session key, and
`isSidechain == false`; between them are a `system/turn_duration` record and a
user record. This establishes a change between responses in one recorded
session, and the unchanged parser returns the two-point trajectory on it. Its
trigger is undetermined: the intervening records do not contain a recognized
model command or confirmation. No new non-interactive runs were substituted
for a real mid-session witness.


[`realized_models_from_claude_jsonl`](../../../crates/cosmon-core/src/model_realization.rs)
discriminates on `type`, reads `message.model` from each assistant record, and
collapses consecutive duplicate IDs only after extraction. It also accepts
`system/init.model` from stream-json; that bootstrap record is not required
for disk logs. A setting selected before any assistant response is not yet
evidence that the model ran.

The existing `claude_quota_fallback_yields_trajectory` test already expects two
models and needs no parser fix. The additional synthetic
[`claude_model_coverage` tests](../../../crates/cosmon-core/tests/claude_model_coverage.rs)
exercise an inter-turn change without a bootstrap, and characterize the
freshness limitation: a complete stable log and a log missing later model
fields currently produce the same trajectory. These fixtures use invented
values and no copied conversation content.

## Freshness is a separate contract

The return value is only `Vec<ModelId>`. It cannot distinguish one model-bearing
record followed by missing fields from hundreds of complete records naming the
same model. The emission layer further stores only changes, by design. Counting
`ModelObserved` events would therefore misclassify healthy stable sessions.

A follow-up should assess provider-specific coverage before deduplication,
persist evidence-status changes under the existing per-attempt boundary, and
extend the attribution fold and displays. Historical observations remain facts;
insufficient current coverage should surface **“last observed; coverage
degraded”**, while intention retains **“intended, not confirmed”**. Unknown
grammar must not be labeled as a diagnosed upstream change. Legacy coverage,
zero responses, placeholders, incomplete final lines, recovery, and restart
replay need explicit semantics and tests.

This is deferred rather than shipped as an unused counter. A parser-only
addition cannot downgrade an already persisted observation or update CLI/UI
parity. The detailed proposal and acceptance cases live in the molecule's
`result.md`. No runtime behavior or event schema changes in this audit.

Claude's per-assistant coverage rule cannot simply be imposed on Codex. The
grammar measured in issue #73 legitimately emits one `turn_context` for many
turns, with settings events only on change. A sparse ratio can expose limited
evidence, but cannot distinguish a stable model from an unrecognized switch.
As [ADR-177](../../adr/177-harness-settings-are-carried-verbatim-and-dispatch-is-not-execution.md)
requires, confirmation means that the adapter reported a value, not proof of
its internal behavior.
