# Reserving the `cosmon` name across registries

> **Goal — namelock.** Stop anyone else from taking cosmon's name on the three
> package registries its audience might look in. The real product is the **`cs`**
> binary, shipped via [GitHub Releases](https://github.com/noogram/cosmon/releases);
> any registry entry here only **holds a name** and points users to that binary.
>
> **This file documents the reservation. It does not publish anything.**
> Publishing claims a public namespace ~permanently and needs registry tokens —
> it is a deliberate **operator gesture** (release phase 2), not a worker action.
> The worker that prepared this had **no registry tokens in its environment** and
> never ran a live publish. `cargo publish` / `npm publish` / `twine upload` are
> irreversible outbound gestures and stay atomic operator gestures.
>
> Mirrors oxymake's `packaging/RESERVE-NAMES.md`. Lineage: delib-20260616-0f4f
> **Q7** (declined-with-rationale: *reservation ≠ publication* — d108 refused
> crates.io **publication** for cosmon; holding a name off to the side is cheap
> insurance that blocks nothing). Parked behind the spine, in the parallel
> finishing fan (Phase G).

---

## ⚠️ Claimability matrix — read this FIRST

The prescribed verify commands (`cargo package`, `npm pack --dry-run`,
`python -m build`) only prove the **artifact builds** — they do **not** check
whether the registry name is free. A live registry probe (read-only GET, never a
publish) tells the real story, and the real story is: **the bare name `cosmon`
is already taken on two of the three registries.**

| Name | crates.io | npm | PyPI |
|------|-----------|-----|------|
| **`cosmon`** | 🔴 **TAKEN** — *"Cosmos Network Monitor"*, v0.0.0, last touched 2021-10-26, 1.6k downloads | 🔴 **TAKEN** — squatter `bukharim96`, v1.0.2 (*"Largest database for up-to-date Node runtime modules…"*) | 🟢 **FREE** (404) |
| **`cs`** (the binary short-name) | 🔴 **TAKEN** — *"common-substrings"*, v0.0.4 | — | — |
| **`cosmon-cli`** | 🟢 FREE | 🟢 FREE | 🟢 FREE |
| **`cosmon-agents`** | 🟢 FREE | 🟢 FREE | 🟢 FREE |
| **`cosmonctl`** | 🟢 FREE | 🟢 FREE | 🟢 FREE |
| **`cosmon-rs`** | 🟢 FREE | 🟢 FREE | 🟢 FREE |
| **`cosmond`** | 🟢 FREE | 🟢 FREE | 🟢 FREE |

Probed 2026-06-16 with `curl` (read-only). `200` = taken, `404` = available; a
crates.io probe needs a `User-Agent` header or it answers `403`.

### What this means — one operator decision is required

The aspirational brand `cosmon` cannot be name-held as-is on crates.io or npm.
Three honest paths (operator's call — this is a naming/branding gesture that
belongs to the operator, not the worker):

1. **Reserve a uniformly-free name** (recommended for a clean one-shot). The only
   names free on **all three** registries are the suffixed forms above.
   `cosmon-cli` reads best, **but** it collides with the existing internal crate
   `crates/cosmon-cli` (the real `cs` binary), so it can't be the placeholder's
   crate name. The next cleanest brandable, free-everywhere option is
   **`cosmon-agents`** (or `cosmonctl`). Pick one, set it as `name =` in
   `crates/cosmon/Cargo.toml` + `packaging/npm/package.json` +
   `packaging/pypi/pyproject.toml`, and the reservation is publishable.
2. **Pursue the squatted `cosmon`.** The crates.io occupant is a dead v0.0.0
   from 2021 (*Cosmos Network Monitor*) — potentially recoverable under the
   [crates.io name-squatting / abandoned-name policy](https://crates.io/policies).
   The npm occupant is a more active squatter. This path is slow and uncertain;
   it is an operator negotiation, not a reservation step.
3. **Reserve only where free.** Hold `cosmon` on PyPI (free), accept the loss on
   crates.io / npm. Weakest namelock; cheapest. Defensible because cosmon's
   audience lives in the terminal + Rust, not pip/npm.

Until the operator decides, the placeholder crate below is wired with
`name = "cosmon"` to document intent. **A local `cargo package` passes, but a
live `cargo publish` of `cosmon` WILL be rejected by crates.io** (name taken).
Changing the one `name =` line to a free name (path 1) makes it publishable.

---

The intended identity, once a name is chosen:

| Field | Value |
|-------|-------|
| name | `cosmon` *(aspirational; see matrix — pick a free name for the actual claim)* |
| version | `0.0.0` (crates, npm, pypi placeholders) |
| description | *Stateless CLI giving AI coding agents identity, a typed lifecycle, and crash-recovery* |
| license | `AGPL-3.0-only` (matches the `cs` binary it points to) |
| repository | `https://github.com/noogram/cosmon` |
| homepage | `https://docs.noogram.org` |
| author | Noogram |

---

## 1. crates.io (Rust) — `cargo`

The name is held by a thin placeholder crate that ships no code. It is the one
crate in the workspace with `publish = true`; the workspace default is
`publish = false` (`[workspace.package] publish = false`, inherited by every
library crate via `publish.workspace = true`), so nothing internal can leak to a
registry by accident.

| | |
|---|---|
| **Files** | `crates/cosmon/Cargo.toml`, `crates/cosmon/src/lib.rs`, `crates/cosmon/README.md` |
| **Version** | `0.0.0` |
| **Verify** | `cargo package -p cosmon` — packages + verify-compiles without publishing |
| **Publish** | `cargo publish -p cosmon` *(rejected today — name taken; see matrix)* |
| **Prerequisite** | `cargo login <crates.io API token>` (token from <https://crates.io/settings/tokens>); the account must own / be able to claim the chosen name |

Verified: `cargo package -p cosmon` → *Packaged 6 files … Finished* (clean).

Conventions carried from oxymake / almanac's healed scars:
- keywords ≤ 5, each ≤ 20 chars;
- categories are exact crates.io slugs (`development-tools`, `command-line-utilities`);
- `readme` path resolves under the crate root (cargo only packages files under
  the crate dir).

---

## 2. npm (JavaScript) — `npm`

Cheap insurance against squatting; low audience overlap (cosmon's users live in
the terminal + Rust). A minimal placeholder package that ships only a README
pointing at the `cs` binary.

| | |
|---|---|
| **Files** | `packaging/npm/package.json`, `packaging/npm/README.md` |
| **Version** | `0.0.0` |
| **Verify** | `cd packaging/npm && npm pack --dry-run` (and `npm publish --dry-run` for the registry-aware simulation) |
| **Publish** | `cd packaging/npm && npm publish` *(rejected today — `cosmon` already at v1.0.2; see matrix)* |
| **Prerequisite** | `npm login` (an npmjs.com account); the chosen name must be free on the registry |

Verified: `npm pack --dry-run` → `cosmon@0.0.0`, 2 files (README.md +
package.json), no warnings. `npm publish --dry-run` **surfaced the conflict**
(*"previously published version 1.0.2 is higher than the new version 0.0.0"*) —
which is exactly how the claimability matrix above learned `cosmon` is taken on
npm. The package is intentionally code-free (`files: ["README.md"]`).

---

## 3. PyPI (Python) — `twine`

A pure name-hold (code-free). **This is the one registry where `cosmon` is
actually free.** Unlike oxymake — whose audience lives in pip/conda and gets a
working thin launcher — cosmon's audience is terminal + Rust, so the PyPI entry
is insurance, not a distribution channel. The wheel is metadata-only
(`[tool.hatch.build.targets.wheel] bypass-selection = true`) because there is no
Python module to ship.

| | |
|---|---|
| **Files** | `packaging/pypi/pyproject.toml`, `packaging/pypi/README.md` |
| **Version** | `0.0.0` |
| **Backend** | `hatchling` |
| **Verify** | `cd packaging/pypi && python -m build` then `python -m twine check dist/*` |
| **Publish** | `cd packaging/pypi && python -m build && twine upload dist/*` |
| **Prerequisite** | a PyPI API token in `~/.pypirc` or `TWINE_USERNAME=__token__` + `TWINE_PASSWORD=<pypi-token>`; build tooling: `pip install build hatchling twine`; the name must be free on <https://pypi.org/project/cosmon/> (free as of 2026-06-16) |

Verified: `python -m build` → *Successfully built cosmon-0.0.0.tar.gz and
cosmon-0.0.0-py3-none-any.whl*; `twine check dist/*` → both **PASSED**.

> **Why metadata-only:** the distribution ships no Python code, so hatchling has
> nothing to auto-discover and `python -m build` would fail wheel selection.
> `bypass-selection = true` builds an empty, valid wheel — a clean name-hold. If
> a real Python launcher is ever wanted (downloads the `cs` binary on first run,
> mirroring oxymake's `packaging/pypi/`), it replaces this in a future minor.

---

## The two-step plan (operator, phase 2 — NEVER automated by CI)

This is a **manual one-shot**, not a recurring job. Two steps:

1. **Reserve** — publish the code-free placeholder at `v0.0.0` to hold the name.
2. **Publish the real thing at release** — *if ever decided*. Today d108 refuses
   crates.io **publication** of cosmon; the product ships as the `cs` binary via
   GitHub Releases. So step 2 may simply never happen, and the reservation stands
   alone as namelock. Reservation blocks nothing; it never gates the public flip.

```sh
# ── PREREQUISITE: pick a claimable name (see matrix) and set it in the 3 files ──
# crates/cosmon/Cargo.toml · packaging/npm/package.json · packaging/pypi/pyproject.toml

# 1. crates.io
cargo login <crates-token>
cargo publish -p cosmon

# 2. npm
npm login
( cd packaging/npm && npm publish )

# 3. PyPI  (`cosmon` is free here today)
( cd packaging/pypi && python -m build && twine upload dist/* )
```

### Why the reservation is a manual one-shot, not a CI job

The same reasoning as oxymake's `RELEASING.md`:

- cosmon's product is the **`cs` binary**, not a published crate. The placeholder
  is published **once**. Automating it on every tag means a `cargo publish` job
  that **fails on every release after the first** (version `0.0.0` already
  exists), forcing a `continue-on-error: true` — the yellow-CI smell to avoid.
- It would put a `CARGO_TOKEN` (and `NPM_TOKEN`, `PYPI_TOKEN`) into the
  tag-triggered path, widening the supply-chain surface the release premortem
  deliberately hardened (no registry secrets in the build/release path).
- **Guard-rail (delib-20260616-0f4f Q8):** "prepare" and "fire" differ by a
  single flag — `cargo publish --dry-run` is one missing flag from a permanent
  claim. The fix is to make "fire" *physically unavailable* to the preparer: no
  registry tokens in the worker env, and the live publish stays a human gesture.

So the reservation is a documented one-shot, run by the operator once.

## Related distribution scaffolding (not a name reservation)

- `packaging/homebrew-tap/` — Homebrew formula scaffolding for the `cs` binary.
  Per-release SHA256 values are filled from release assets, then committed to the
  tap repo. Not a registry name-hold.
