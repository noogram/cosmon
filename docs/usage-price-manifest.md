<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Usage price manifest

Cosmon values observed model/token segments under a named comparison basis:
the Standard, global, direct-provider list price. It is an API-equivalent
reference estimate, not a bill. Taxes, credits, negotiated discounts,
subscription fees, non-token tools, regional uplifts, Batch discounts, and
premium speed tiers are outside this basis.

The canonical offline dataset is
`crates/cosmon-core/data/usage-prices.json`. Its adjacent `.sha256` file pins
the exact reviewed bytes. The manifest keeps prior cards and selects one
`current_revision`; a refresh adds a new immutable card instead of editing an
old valuation basis. Model matching is exact. Only aliases explicitly listed
from provider documentation are accepted.

## Sources and limitations

The current `standard-2026-09-30-sonnet-5-5` card was checked on 2026-09-30.
It preserves the prior card and adds the exact `claude-sonnet-5-5` id using
the [official model price card](https://platform.claude.com/docs/en/models/sonnet-5-5/overview):
$2 input, $0.20 cache read, $2.50 five-minute cache write, $4 one-hour cache
write, and $10 output per million tokens. The earlier `claude-sonnet-5` rate
remains available for historical usage.

The `standard-2026-09-28-codex` card was checked on 2026-09-28 against the
official model cards and price lists recorded in each entry. Primary references
are:

- The OpenAI [API pricing page](https://developers.openai.com/api/docs/pricing)
  for the exact Codex-observed GPT-6, GPT-5.6, and GPT-5.5 ids. The card uses
  its Standard short-context rates; GPT-5.5 is limited to its published
  below-272K-context band.
- OpenAI model cards for
  [GPT-5-Codex](https://developers.openai.com/api/docs/models/gpt-5-codex),
  [GPT-5.1-Codex](https://developers.openai.com/api/docs/models/gpt-5.1-codex),
  [GPT-5.2-Codex](https://developers.openai.com/api/docs/models/gpt-5.2-codex),
  and [GPT-5.3-Codex](https://developers.openai.com/api/docs/models/gpt-5.3-codex).
- Anthropic model cards for
  [Claude Opus 4.6](https://platform.claude.com/docs/en/models/opus-4-6/overview),
  [Claude Sonnet 4.6](https://platform.claude.com/docs/en/models/sonnet-4-6/overview),
  [Claude Opus 5.5](https://platform.claude.com/docs/en/models/opus-5-5/overview),
  plus the official [model overview](https://platform.claude.com/docs/en/models/overview)
  and [price list](https://platform.claude.com/docs/en/about-claude/pricing).

Claude Code's aggregate `cache_creation_input_tokens` counter does not state
whether a write used the five-minute or one-hour duration. Those writes have
different rates, so valuation preserves the priceable fresh-input, cache-read,
and output subtotal and names `cache_write_duration` as missing. It does not
guess. The same rule applies to any future model whose context band or cache
write category is not established by the observed counters.

## Review process

The usage-accounting maintainers own the manifest. They review every entry at
least every 30 days, before a release, and whenever a supported model or
provider billing schema changes. An observed unknown model also triggers a
review; it never inherits a similar model's rate.

A refresh is a reviewed data change:

1. Recheck the exact model id, Standard/global applicability, every token
   category, context boundary, published effective date, and official URL.
2. Add a new card revision. Do not rewrite an existing card.
3. Recompute `usage-prices.json.sha256` from the complete JSON bytes.
4. Update independent arithmetic and historical-selection tests. Expected
   amounts must be calculated from cited source rates, not production
   constants.
5. Run the scoped core and reader tests, then `just quick` and `just gates`.

Retrieval may prepare a drift report, but runtime code never fetches prices and
no automated job silently republishes a changed tariff. A card older than the
30-day interval remains usable as a clearly dated historical basis, but it
must not support an unqualified current-price claim.
