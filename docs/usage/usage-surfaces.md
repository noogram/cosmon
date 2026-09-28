<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Usage surface and migration contract

Cosmon projects one canonical `UsageRecord` onto four surface classes. The
record keeps API-equivalent USD, plan allowance observations, token counters,
scope and coverage independent. An unavailable value is never projected as
numeric zero.

## Surface parity audit

| Surface | Audience | Projection contract |
|---|---|---|
| `cs peek` TUI | Human, live | The ENERGY cell shows `API equiv.` and every distinct plan window. Account windows retain the `account` label; worker attribution remains `worker plan use unavailable` unless worker-scoped evidence exists. Cumulative histories are deduplicated before token or USD addition. |
| `cs peek --snapshot` | Human, fixed 120 columns | Each worker row is followed by fixed-width usage lines. API coverage, account scope/window and worker-attribution availability are separate lines, so the old 18-column COST cell cannot truncate a qualifier. |
| `cs ensemble` | Human, fleet aggregate | Worker rows show both dimensions. Fleet and grand USD values are labelled `API equiv.` and report priced/total history coverage. Plan percentages remain per-window observations and are never included in a subtotal. |
| `cs peek --json` | Machine, molecule view | `molecules[].usage` is an additive array of canonical records, deduplicated by cumulative history. Empty means unobserved, not zero. |
| `cs ensemble --json` | Machine, worker view | `workers[].usage` is authoritative. The existing `workers[].cost` field remains a deprecated, lossy compatibility projection for transition consumers. |
| `events.jsonl` | Machine, durable replay | New producers emit `usage_observed`. Readers accept both `usage_observed` and legacy `energy_tick`; raw-kind readers correlate migration samples and difference cumulative histories independently. |

The macOS and iOS wheat-paste viewports consume the canonical snapshot bytes;
they do not own another usage renderer. No separate native UI change is needed
for this read-only projection.

## Aggregation and identity

`subject.history` is the addition key. Repeated observations of one cumulative
history replace an earlier sample; they do not add to it. Distinct histories
may add API-equivalent estimates, but the result is complete only when every
history has complete price coverage. Known plus unknown therefore renders a
partial subtotal with `priced/total` coverage. All unknown renders
`API equiv. unavailable`, never `$0`.

Plan windows are observations, not debits. Exact duplicate readings collapse;
different readings remain separate when no safe account correlation exists.
Their percentages are never summed. In particular, two workers that observe
the same shared account do not each acquire that account percentage as worker
consumption.

## Durable compatibility boundary

Reader support for `usage_observed` landed before this producer cutover. A
current reader replays both generations through `Envelope::usage_record()`.
A positive legacy `energy_tick.cost_usd` remains a legacy estimate with unknown
rate provenance; legacy zero remains `legacy_zero_ambiguous`. Journals are not
rewritten.

The current `cs peek --no-tui` producer emits only `usage_observed`, and emits
nothing when no canonical record exists. It also suppresses an unchanged
cumulative observation. There is no dual emission, so downstream readers do
not need to guess whether an `energy_tick` and `usage_observed` are the same
sample. Old binaries that do not know the new enum variant are not promised to
deserialize new lines; the supported transition is new-reader/old-journal,
followed by new-producer/new-reader.

The raw `cs pulse` reader does not link the typed enum. It recognizes both
event tags, keys canonical samples by history, aliases a preceding legacy
sample from the same worker during migration, and computes first-to-last deltas
per history. Interleaved workers therefore cannot turn cumulative totals into
cross-worker subtraction.

## Known collection boundary

Codex rollout plan windows are integrated through the supported parser and
remain account-scoped. Claude API-equivalent usage is integrated from session
logs. Claude plan collection remains unavailable unless dispatch can supply a
supported effective-settings overlay and ordinary authorized activity yields a
live reading. Rendering that honest unavailable state is not evidence of live
Claude plan collection.

