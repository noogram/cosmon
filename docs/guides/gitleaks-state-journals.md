# gitleaks × cosmon state journals — the shared scan baseline

**TL;DR** — cosmon writes a free-text journal (`.cosmon/state/events.jsonl`).
gitleaks' entropy heuristic mistakes that prose for secrets and blocks
`cs done`. The fix is **one federation-shared gitleaks config**, scaffolded by
`cs init`, that silences *only* the noisy rule on *only* the journal — while
still catching a real secret pasted into a `reason`. Do **not** hand-roll a
per-galaxy allowlist.

---

## The pathology

Every molecule lifecycle event is appended to `.cosmon/state/events.jsonl`. The
`reason` field is **free-text natural language** — a one-line human summary of
what happened. Example reason that triggered this work:

```
... D2 mechanism=mailroom, artefact=knowledge sub-MOC, no new galaxy ...
```

A galaxy that (a) **tracks** that journal in git and (b) runs **gitleaks** in a
pre-commit hook hits a structural false positive: gitleaks' `generic-api-key`
rule flags secrets by *entropy + assignment shape* (`keyword … = … value`), and
benign `word=word` prose fragments like `artefact=knowledge` look exactly like
an assigned API key. (The `secret`-prefixed token `mailroom` nearby is
enough to arm the keyword side of the heuristic.)

Result: the pre-commit reports `leaks found`, **every `cs done` that flushes
state fails**, the merge aborts, and the harvest is blocked until a human
intervenes. A blocked harvest is a **broken invariant** — `cs done` must always
be able to complete — not mere friction. ("Le réacteur apprend de ce qu'il
brûle".)

## Why not the obvious quick fixes

Two tempting fixes were **rejected**:

- **Sanitise `reason` at write time.** `events.jsonl` is the *hash-sealed
  source of truth* for cognitive history (see `.gitignore` rationale and
  `docs/architectural-invariants.md` §9). Mutating the prose to dodge an
  entropy heuristic corrupts the record, breaks the seal chain, and is endless
  whack-a-mole on natural language. No.

- **Stop tracking `events.jsonl`.** It is the authoritative cognitive log that
  application galaxies legitimately keep on their main branch. Gitignoring it
  loses the record. (cosmon-the-repo is special — a `team` residence whose
  state lives on the `cosmon/state` orphan branch — but that is *not* the model
  for application galaxies.) No.

The third option — **a shared scan profile** — is the one we shipped.

## The fix: one shared baseline

`assets/gitleaks/cosmon-baseline.gitleaks.toml` is the canonical config. It:

1. `useDefault = true` — keeps the **entire** high-confidence gitleaks ruleset
   active everywhere (GitHub PAT, Slack tokens, GCP keys, PEM private keys, …).
2. Adds a dedicated **`cosmon-aws-access-key-id`** rule. gitleaks' default set
   has *no* keyword-free `AKIA…` rule — it catches AWS keys only via the
   entropy heuristic we're about to silence on journals. This rule (mirroring
   cosmon's own `leak-corpus.toml` K02) keeps AWS coverage on journals.
3. Adds a **rule-scoped allowlist** (`targetRules = ["generic-api-key"]`) that
   suppresses *only* the entropy rule on *only* state-journal paths
   (`events.jsonl`, `.cosmon/**/*.jsonl`). Every other rule still scans those
   files.

The net effect, verified empirically (gitleaks 8.30):

| Input | Default gitleaks | With baseline |
|-------|------------------|---------------|
| `artefact=knowledge` in `events.jsonl` | ❌ flagged (FP) | ✅ clean |
| real `ghp_…` GitHub PAT in `events.jsonl` | flagged | **still flagged** |
| real `AKIA…` AWS key in `events.jsonl` | flagged (via entropy) | **still flagged** (dedicated rule) |
| `secret_key = "<entropy>"` in `app.py` | flagged | **still flagged** (rule untouched off-journal) |

So the Wasabi-class accident (a real secret landing in state, commit
`5390909d` in mailroom) is **still caught at commit time** — only the
heuristic that structurally misfires on prose is disarmed, and only where it
misfires.

## How a galaxy gets it

**New galaxies** — `cs init` scaffolds `.gitleaks.toml` at the repo root
automatically. Born-correct, nothing to do.

**Galaxies already in flight** — run `cs init --upgrade <path>`. The upgrade
pass backfills `.gitleaks.toml` if absent (it never overwrites a customized
one). Verify with:

```bash
gitleaks protect --staged --no-banner -v   # should pass on a cs-done commit
```

**If you have a customized `.gitleaks.toml`** that you can't replace wholesale,
copy the two blocks (`[[rules]]` AWS + `[[allowlists]]` targetRules) from
`assets/gitleaks/cosmon-baseline.gitleaks.toml` into yours. Keep the
`# cosmon-gitleaks-baseline vN` marker so drift is greppable across the
federation.

## Relationship to `cs doctor leaks`

cosmon ships its own native secret scanner, `cs doctor leaks --corpus`, reading
the high-confidence pattern corpus at `~/.config/cosmon/leak-corpus.toml` (no
entropy heuristic, so no prose false positives by construction). It is a
complementary belt-and-suspenders gate — a galaxy may run it in pre-commit
*instead of* or *alongside* gitleaks. The gitleaks baseline exists because many
galaxies already standardise on the gitleaks pre-commit hook; both paths reach
the same place: high-confidence detection without the entropy FP on journals.

## Anti-pattern: per-galaxy whole-file allowlists

The original mailroom workaround (commit `d4bdf651`) added
`\.cosmon/state/events\.jsonl$` to the **global** `[allowlist].paths`. That
disables *all* scanning on the file — a real secret would slip through. It also
diverges silently from every other galaxy. The shared baseline replaces it:
same unblock, but the high-confidence rules keep watching the journal, and
there is one source of truth instead of N drifting copies.

---

*Decided 2026-06-23, `task-20260623-e9f0` (signal from mailroom). Source of
truth: `assets/gitleaks/cosmon-baseline.gitleaks.toml`.*
