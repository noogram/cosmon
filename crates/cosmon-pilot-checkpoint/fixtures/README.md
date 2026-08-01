<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# M3 checkpoint fixtures

Three hand-written `PilotCheckpoint` records, used by
`tests/acceptance_m3.rs`. They are the M3 half of the confidentiality ceiling
ADR-168 sets for the whole mission: **envelopes, never content**. No transcript
line, no real session id, no machine path — the session ids are `sess-claude`
and `sess-codex`, and every evidence locator is a repository-relative path that
exists in this checkout.

| File | Who it stands for | What it is for |
|---|---|---|
| `primary-claude.json` | the PRIMARY pilot, a Claude session | the reference side of every comparison |
| `copilot-codex-drifted.json` | a COPILOT that has drifted | fires all three acceptance classes at once |
| `copilot-codex-aligned.json` | a COPILOT that has not | the `AGREE` case, so the tests prove the comparison can also say yes |

`copilot-codex-drifted.json` is deliberately *one* file rather than three
single-defect files. A comparison that finds a scope change only when nothing
else is wrong would pass three isolated fixtures and still be useless on a real
hand-over, where the three arrive together.

The pair differs from `primary-claude.json` exactly as follows:

- **scope** — the co-pilot added `presence mailbox` to `includes`
  (→ `scope_change`);
- **`rotation-restarts-read`** — held with the opposite stance
  (→ `contradictory_hypothesis`);
- **`merge-after-gates`** — intended with the opposite stance
  (→ `contradictory_intent`);
- **`third-provider-needs-adapter-only`** — asserted with an empty `evidence`
  list, on a subject the primary also addresses (→ `missing_evidence`);
- **`cursor-is-byte-offset`** — identical stance on both sides, so the same
  report also carries a `subject_agreement` record. A comparison that only ever
  emits findings would be as useless as one that never does.
