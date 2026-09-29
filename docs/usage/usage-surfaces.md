<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Usage surface and migration contract

Cosmon projects one canonical `UsageRecord` onto four surface classes. The
record keeps API-equivalent USD, plan allowance observations, token counters,
scope and coverage independent. An unavailable value is never projected as
numeric zero.

## Surface parity audit

| Surface | Audience | Projection contract |
|---|---|---|
| `cs peek` TUI | Human, live | The ENERGY cell renders token counters, `API equiv.`, every distinct plan window, and worker attribution on separate terminal lines at 80, 120, and 200 columns. Narrow layouts collapse secondary columns to preserve the usage qualifiers. Account windows retain the `account` label; worker attribution remains `worker plan use unavailable` unless worker-scoped evidence exists. Cumulative histories are deduplicated before token or USD addition. |
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

The rollout probe refreshes plan freshness against a ten-minute maximum age.
Human surfaces label stale windows, and show `reset` in place of a percentage
once a window's reset time has passed. JSON retains the distinct `freshness`
state and original observation and reset times. Account scope never becomes
worker attribution during refresh.

The price card's context band is enforced at valuation: cumulative logs cannot
establish each request's context length, so a rate limited to a context band is
a partial short-band estimate with `context_length_unknown` named in coverage.
Cache writes with a reported five-minute/one-hour split use the corresponding
rates. Missing splits remain partial. Unsupported reasoning and cache-write
counters remain unavailable in the canonical record rather than measured zero.

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

Rollout windows and stored status-line windows enter the same canonical worker
record as API-equivalent USD. The stored window path is scoped to the molecule
attempt, refreshed at read time, and remains account-scoped. A status-line
overlay is installed only when local settings resolve without a separate
launch settings flag. A live reading still requires an ordinary callback;
source code and synthetic fixtures alone do not establish that witness.
