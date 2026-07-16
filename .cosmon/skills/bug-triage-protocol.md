---
name: bug-triage-protocol
description: |
  Inject the cosmon bug-triage checklist into any worker handling a molecule
  with kind=issue. Ensures every bug report lands with a minimal
  reproduction, a failing test, an isolated suspect region, and an explicit
  ownership tag before any fix is attempted. Worked example for ADR-042.
applies_to:
  molecule_kinds: ["issue"]
  formulas: []
  personas: []
inject_priority: 100
max_tokens: 2048
disable-auto-injection: false
---

# Bug Triage Protocol

> This is a cosmon **skill** (see [ADR-042](../../docs/adr/042-cosmon-skill-extension-surface.md)).
> It is injected into the briefing of any worker tackling an 🐛 issue
> molecule. Read it once before starting, then keep it next to your
> plan — every numbered item must be closed before `cs complete`.

## 1. Reproduce first, diagnose second

A bug you cannot reproduce on demand is a **symptom**, not a bug. Your
first deliverable is not a fix — it is a command or test that **fails
every time** on the current `main` and passes after the fix. If you
cannot produce this, collapse the molecule with reason
`not-reproducible`; do not guess.

**Exit artifact:** a shell command, a test function, or a script under
`tests/` that exits non-zero on the broken state.

## 2. Minimise the repro

Strip the reproduction to the smallest input that still fails. Remove
every dependency, every flag, every environment variable that does not
affect the outcome. The minimised repro is the *anchor* the rest of
the triage hangs on.

**Exit artifact:** the minimised repro committed alongside the fix,
usually as a new test case.

## 3. Isolate the suspect region

Bisect — by `git bisect`, by binary-search commenting, or by feature
toggle — until you have a **single commit**, a **single function**, or
a **single data path** that, when reverted or removed, makes the repro
pass. Write the suspect locus into `triage.md` in the molecule
directory before touching the code.

**Exit artifact:** `triage.md` with sections *Symptom*, *Repro*,
*Suspect locus*, *Evidence*.

## 4. Tag ownership

Every bug has an owner — a person, a formula, or an ADR. Add the
ownership tag to the molecule:

```
cs tag <molecule-id> --add owner:<area>
```

Common ownership tags: `owner:state-store`, `owner:runtime`,
`owner:surface`, `owner:cli`, `owner:docs`. If no existing tag fits,
invent one and note it in `triage.md` — future triage sweeps will
converge on the right vocabulary.

## 5. Fix, then broaden

Implement the narrowest fix that makes the minimised repro pass. **Do
not refactor, do not rename, do not "clean up while I'm here."** Those
are separate molecules. Once the fix lands, broaden the test: add
adjacent cases the original bug report did not catch but the same root
cause could produce.

**Exit artifact:** a failing test, a passing fix, at least one
additional test case, and a diff under 200 lines whenever possible.

## 6. Close the loop

Before `cs complete`:

- [ ] `triage.md` exists and names the suspect locus.
- [ ] The molecule carries an `owner:*` tag.
- [ ] The failing-test-now-passing is committed.
- [ ] At least one adjacent test case was added.
- [ ] `cargo check + test + clippy + fmt` all pass.
- [ ] The commit subject matches
  `fix(<area>): <one-line summary>` (conventional commits).

If any of these are missing, you are not done — you are *almost* done,
which is a different state with a much higher probability of regression.

---

*This skill is prose, not code. If you want to change the protocol,
edit this file. The next worker on an 🐛 issue molecule will see the
new version automatically.*
