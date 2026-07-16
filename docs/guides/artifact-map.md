# Guide — the artifact map (ADR-057)

Every cosmon galaxy accumulates files of many different kinds:
chronicles, ADRs, deliberation syntheses, partner deliverables,
auto-generated GitHub mirrors, source code. They do not all belong
on the same git branch, and they do not all share the same audience.

The **artifact map** (`.cosmon/artifact-map.toml`) declares what each
tracked file is, who it is addressed to, and — by derivation — where
it should live. Two CLI verbs read it:

- `cs inspect <path>` — classify one path.
- `cs artifacts audit` — walk `git ls-files` and report per-genre
  counts + any unclassified paths.

Governing decision: [ADR-057](../adr/057-genre-and-artifact-map.md).
Residence substrate: [ADR-055](../adr/055-cosmon-residence.md).

## 1 · The shape of the TOML

```toml
# .cosmon/artifact-map.toml

[chronicle]
location = ["docs/lore/**/*.md"]
audience = "author+agent"

[adr]
location = ["docs/adr/**/*.md"]
audience = "public"

[addl]
location = ["docs/addl/<name>/**/*"]
audience = "partner:<name>"

[github-surface]
location = ["docs/surfaces/**/*.md", "STATUS.md", "ISSUES.md"]
audience = "solo"

[deliberation]
location = [".cosmon/state/fleets/*/molecules/*/synthesis.md"]
audience = "author+agent"

[code]
location = ["**/*"]
audience = "public"
```

Each table is one **genre**. Two fields are required:

- **`location`** — list of glob patterns. Longer-fixed-character
  patterns win ties; declaration order breaks remaining ties.
- **`audience`** — one of `public`, `team`, `author+agent`, `solo`,
  `partner:<name>` (the `<name>` token captures the matching path
  component).

Residence is **derived** from audience (§3 below); it is never
declared directly in the map. A `solo` audience becomes a `Solo`
residence; every other audience becomes a `Team` residence.

## 2 · Scenario A — you add a new kind of file

Say you start writing a new series of research pads under
`research/pads/*.md` and you want cosmon to know these are yours
alone, not to be pushed.

1. Open `.cosmon/artifact-map.toml`.
2. Insert a new genre table **above** the `code` catch-all — the
   tiebreaker respects declaration order, and `code` must stay last:

   ```toml
   [research-pad]
   location = ["research/pads/**/*.md"]
   audience = "author+agent"
   ```

3. Run `cs inspect research/pads/my-idea.md` to confirm the new genre
   matches:

   ```
   path:      research/pads/my-idea.md
   genre:     research-pad
   audience:  author+agent
   residence: team
   rot:       today
   ```

4. Run `cs artifacts audit` — the new genre should appear in the
   count table and `invariants: OK` should hold.

That is the full flow. No code to write, no rebuild.

## 3 · Scenario B — the surfaces are polluting `git log`

When `cs reconcile` runs on a galaxy with a GitHub projection
configured, it rewrites `docs/surfaces/issues.md`, `prs.md`, and the
top-level `STATUS.md` / `ISSUES.md`. If those files are tracked on
`main`, every sync lands a `chore(surfaces): refresh` commit whose
only content is a projection of state already held on disk elsewhere.

The noise comes from a genre mismatch: these files are
**regenerable mirrors**, not operator-authored artifacts. Their
audience is `solo` (the local machine is the only legitimate
reader); their residence is `Solo` (excluded from git).

Ship the `github-surface` genre:

```toml
[github-surface]
location = ["docs/surfaces/**/*.md", "STATUS.md", "ISSUES.md"]
audience = "solo"
```

Then exclude the paths from the git index locally:

```sh
git rm -r --cached docs/surfaces STATUS.md ISSUES.md
printf 'docs/surfaces/\nSTATUS.md\nISSUES.md\n' >> .git/info/exclude
```

(`.git/info/exclude` is a **local-only** ignore file that does not
ship with `git push`.)

`cs reconcile` keeps regenerating the files on disk — GitHub still
gets its Issues via the API push channel — but `git log` is quiet
again.

v1 will wrap that three-line migration into a `cs migrate --genre`
command. Until then, the operator applies it by hand once per
galaxy.

## 4 · Scenario C — partner deliverables

Anything under `docs/addl/<name>/…` becomes `audience =
partner:<name>` — the `<name>` token in the glob captures the next
path component and parameterises the audience at classification
time.

```
docs/addl/bob/videos/demo.mp4
  → genre = addl
  → audience = partner:bob
  → residence = team
```

A galaxy that wants partner deliverables on an **encrypted**
narration branch simply configures the galaxy-level residence as
`Encrypted` (ADR-055 §3); the genre stays `addl`, the audience stays
`partner:bob`, and the age wrap happens at the residence layer.
Genre composes cleanly with residence — it does not replace it.

## 5 · The four invariants (what `cs artifacts audit` checks)

- **I1 Totality** — every path classifies. The `code` catch-all at
  the bottom of the map guarantees this by construction.
- **I2 Unique classification** — when two globs could match a path,
  the longer-fixed-character pattern wins; ties break in declaration
  order.
- **I3 Residence well-typed** — every audience maps to a valid
  residence.
- **I4 Audience–residence compat** — a `solo` audience may only
  derive `Solo`; a `public` audience may only derive `Team`. (v0
  reports; v1 enforces at migration time.)

`cs artifacts audit` exits `0` when the invariants hold and `1` when
any I1 violation (unclassified path) is found.

## 6 · What v0 does *not* do

This first version **declares** and **reports**. It does not:

- Move files between residences (`cs migrate --genre`, v1).
- Rewrite `.gitignore` or `.git/info/exclude` based on genre (v1).
- Run as a pre-commit hook (v1).
- Infer `rot` policies per-genre (`rot` is always computed from
  `git log`, never declared).

The v0 value is the classification itself: once every file has a
name and an audience, the next layer of tooling has something
structural to lean on.

## 7 · References

- [ADR-057 — Genre and the Artifact Map](../adr/057-genre-and-artifact-map.md)
- [ADR-055 — Cosmon Residence](../adr/055-cosmon-residence.md)
- Deliberation: `delib-20260420-74b8` (artifact-map panel)
- Deliberation: `delib-20260420-fbe4` (TOML vs Neurion graph)
- Chronicle: *Les six tiroirs* (2026-04-20)
