# Runbook — `verify-surface` mindguard

**Status**: live since 2026-05-27.
**Scope**: every `cs complete <MOL>` invocation, every galaxy under the
cosmon syzygie.
**Source**: `crates/cosmon-cli/src/mindguard/surface_visual.rs`.

## TL;DR

If `cs complete <MOL>` refuses with:

```
✘ mindguard surface_visual refused task-…
  molecule task-… touched the visual surface but no verify-surface
  molecule landed GREEN in the last 60 minutes.
  Remedy: cs nucleate verify-surface --var target=task-…
```

then your molecule changed something visual (HTML / CSS / JS / wiki /
lumen-web) and the gate is waiting on an *independent* witness that
the surface still renders as intended. Run the remedy command, let
the verify-surface molecule complete GREEN, then re-run `cs complete`.

The gate is **fail-closed**: when its machinery itself cannot run
(git unreachable, state store corrupt, config malformed) it refuses
with `mindguard unavailable`, NOT with a default-pass. To proceed
*only when the gate machinery itself is broken*, use:

```
cs complete --override-mindguard-down --justification "…" <MOL>
```

Every override lands write-once in `~/.cosmon/audit/mindguard-overrides.jsonl`.

---

## Why this exists

The auto-pilote wrote *« DRAIN COMPLET »* on 2026-05-27 without
looking at the rendered surface. Not an étourderie — the third
Kahneman trap in eight days, all of identical structure:

| date | task | proxy that lied |
|---|---|---|
| 2026-05-21 | MLX-GPU readiness | `sysctl` returned "true" |
| 2026-05-24 | PDF correctness | `make all` exit code = 0 |
| 2026-05-27 | rendered surface ready | `git push OK` |

Each time a saillant proxy (which the agent's own channel produced)
stood in for the real observable (which an independent channel must
touch). Pearl Harbor, Bay of Pigs, Challenger: same pathology, smaller
stakes, identical structure.

The cosmon-ward axiom inscribed 2026-05-27:

> Aucune claim d'état — *complete*, *verified*, *deployed*, *prêt* —
> n'est valide si l'observable qui la définit n'a pas été touchée,
> par un témoin indépendant du canal qui produit la claim, dans la
> fenêtre de temps qui précède la claim.

`surface_visual` is the v0 enforcement: a CLI-critical-path wrapper
(janis §3a: not a worker hook, which the agent can fabricate around)
that reads the git diff itself and refuses the claim if the
independent witness (a verify-surface molecule) is missing.

---

## When the gate activates

The gate computes "surface touched" at gate time from the
**molecule-attributable** diff:

```
git diff --name-only <base>...<head>
```

with `<head>` the molecule's `feat/<MOL>` branch when it exists
(`HEAD` of the invocation context otherwise), and `<base>` taken in
order from the molecule's blocker branches (fork-point attribution,
ADR-114), then `origin/main`, then `main`. The last resort is a plain
`git diff HEAD` run **in the molecule's worktree** when one is checked
out — never against the galaxy root's working tree, whose pre-existing
dirty files are not the molecule's doing (the automata misfire of
2026-06-07, `task-20260607-18ee`). If *any* file in that diff matches
the configured `surface.paths` globs, the gate activates.

Default patterns (see `crates/cosmon-cli/templates/mindguard-surface.toml`):

```toml
paths = [
    "poc/optix-modernization/lumen/web/**",
    "wiki/**",
    "**/*.html",
    "**/*.css",
    "**/*.js",
]
```

Override locally by writing `~/.config/cosmon/mindguard-surface.toml`
with new globs / `t_max_minutes`. Missing config falls back to the
defaults — silently widening or narrowing is impossible.

When the gate activates, it requires a sibling molecule satisfying
*all* of:

1. `formula_id == "verify-surface"`;
2. `variables.target == "<MOL>"`;
3. `status == Completed`;
4. `updated_at >= now − T_max` (default 60 min).

If none found → `MindguardRefused`. The molecule cannot complete
until the operator runs:

```
cs nucleate verify-surface --var target=<MOL>
```

This nucleates the verify-surface molecule. The worker (a panel of
mindguard personae or a manual operator screenshot pass) touches
the *actual* rendered surface, records pass / concern / fail, and
the molecule completes. Then `cs complete <MOL>` works.

`verify-surface.formula.toml` is a **builtin formula**: `cs init`
seeds it on every new galaxy, and `cs init --upgrade` backfills it on
existing ones. A galaxy missing it cannot satisfy a refused
`cs complete` at all — run the upgrade rather than hand-copying the
file.

---

## Override path (mindguard down)

Only the `Unavailable` arm is overridable. `Refused` means the gate
fired *intentionally*; the remedy is the verify-surface molecule,
not the override.

To force completion when the gate machinery itself cannot run
(git missing, state store unreachable, config malformed):

```
cs complete --override-mindguard-down --justification "…" <MOL>
```

The justification is required. The override lands a record in
`~/.cosmon/audit/mindguard-overrides.jsonl`:

```jsonl
{"timestamp":"2026-…","gate":"surface_visual","molecule_id":"task-…","justification":"…","underlying_error":"…"}
```

The ledger is append-only — never trimmed, never rewritten. Audit
it periodically:

```
jq 'select(.timestamp > "2026-05-01")' \
   ~/.cosmon/audit/mindguard-overrides.jsonl \
   | jq -s 'group_by(.justification) | map({reason: .[0].justification, count: length})'
```

If the same justification repeats more than once or twice, the
underlying gate machinery is the real problem — fix that, not the
overrides.

---

## Audit & forensics

**See every override ever taken**:

```
cat ~/.cosmon/audit/mindguard-overrides.jsonl | jq .
```

**See every refusal that fired** (no central log yet — refusals
print red on stderr and the molecule stays uncompleted, so a
*lack* of a `cs ensemble` row in `Completed` for a surface-touching
molecule is the signal):

```
cs ensemble --status completed --formula task-work \
  | jq 'select(.tags[]? | startswith("surface=touched"))'
```

When this set is empty for a molecule you remember pushing, the
gate likely refused — re-run `cs complete <MOL>` to see the red
remedy.

---

## Precedent — 2026-05-27 incarné

The deliberation `delib-20260527-c940` (Conseil "DRAIN COMPLET") is
the founding case. The auto-pilote landed 16 children from a
ferrari polymerization onto `main`, then wrote *« 16 children
polymerization landed — surface restructurée »* without ever
opening the rendered deck. The operator opened the browser and
saw three different empty / broken screens.

Synthesis §CV3 + §T7 + §SI2 + janis §3-§5 produced the spec for
this gate. Task `task-20260527-f835` shipped it.

The gate's reason for existing is the gate's first refusal: the
auto-pilote that wrote "DRAIN COMPLET" without looking would, on
the next attempt to claim that state under the same conditions,
have hit *this* refusal first. The pattern that produced it cannot
reproduce without an independent visual witness landing GREEN.

---

## See also

- `crates/cosmon-cli/src/mindguard/` — implementation.
- `crates/cosmon-cli/templates/mindguard-surface.toml` — config template.
- An internal chronicle — 2026-05-27 entry
  *L'auto-pilote qui claim sans regarder*.
- `~/.cosmon/audit/mindguard-overrides.jsonl` — override ledger
  (append-only).
- `~/.claude-accounts/dev@noogram.dev/CLAUDE.md` — global agent
  instructions, *Auto-pilot session mode* section (the rule that
  references this gate by name).
