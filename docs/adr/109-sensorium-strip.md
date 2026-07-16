# ADR-109 — Sensorium strip: five organs, one strip, zero new surface

**Status:** Accepted (2026-05-22).
**Date:** 2026-05-22.
**Decider:** Noogram.
**Empirical motive:** parent deliberation
`delib-20260521-955f`
— *"Architect cosmon-incarné v0"*. Synthesis §5.3 ratifies jr's
*vital strip* discipline (`responses/jr.md`) as the visible-layer
contract: every cosmon organ shows itself through bytes the existing
`cs peek --snapshot` raster already paints, not through a SwiftUI view
of its own.
**Authoring task:** `task-20260522-55a8`.
**Authoring discipline:** jr-style scale-shift — the data was always
on disk; we changed the scale at which the operator sees it.

**Binds:**
[ADR-066](066-ux-v2-substrate.md) (`§8k'` — every cosmon-facing
surface is a wheat-paste viewport over `cs peek --snapshot`; the
strip honours this by emitting bytes inside that raster);
[ADR-068](068-ux-cli-equivalence.md) (UX ↔ CLI parity — every byte
in the strip is queryable as `cs sensorium --json`);
[ADR-016](016-autonomy-regimes-and-resident-runtime.md) (no daemon
on the Transactional Core — the strip is recomputed on each
`cs peek --snapshot` invocation, not watched);
[ADR-047](047-event-log-protocol-v0.md) (file-on-disk = source of
truth — the strip reads `.cosmon/state/sensorium/*` files, never
in-memory state).

**Blocks (sibling implementation molecules):**
- `task-20260522-8bcd` — `peau-morning-digest` formula writes
  `inbox.ndjson`.
- `task-20260522-34e4` — `voix-reply` formula writes `outbox/*.md`.
- Future curate-patrol / coeur formula writes `heartbeat.ndjson`.
- Future `cs verify --visage` writes the BLAKE3-seal drift flag.

---

## Context

The 2026-05-21 deliberation asked nine personas the same question:
*how does cosmon grow a body — peau, cœur, visage, carnet, voix — without
splintering the operator's mental model into five new UIs?* Synthesis
§5.3 converged on jr's answer (`responses/jr.md`):

> *Five glyphs on a strip. The data was always there. We changed the
> scale.*

The visible layer is **one new fixed line** in `cs peek --snapshot`,
between the existing header rule and the molecule list. Every organ
writes into `.cosmon/state/sensorium/*` on disk; the strip reads those
files at render time and renders five glyphs. **Zero new SwiftUI
primitive.** ADR-066 §8k' holds by construction.

The hard rule from `responses/jr.md` §"Silence rule":

> *Stillness is the signal. The strip is byte-identical when nothing
> has changed. The eye learns to ignore it until a glyph moves.*

This is the **opposite** of openclaw's HEARTBEAT.md, which
regenerates on every tick and draws attention even when nothing is
alive (synthesis §6).

---

## Decisions

### D1 — Glyph alphabet (immutable for v0)

**Accepted:** the five-glyph alphabet is **`~ * @ = >`**, plus the
`[off]` kill-switch marker. All 7-bit ASCII. All legible at 16×16 px
in a menubar viewport. All survive grep, tmux capture, JSON quoting,
and the golden-snapshot test.

| Organ | Family | Spoken | Glyph | Rendered example |
|------|-------|--------|-------|------------------|
| channels-in | `peau-*` | *la peau* | `~` ripple | `~ 03` (3 unhandled signals last 24h) |
| heartbeat | `coeur-*` / `curate-*` | *le cœur* | `.` resting / `*` live | `. . . * . . . * . .` (last 10 beats) |
| identity | `<galaxy>/SOUL.md` | *le visage* | `@` | `@ noogram/cosmon` |
| memory | `carnet-*` | *le carnet* | `=` recall / `-` decay | `= 4.2k notes -12 in 6h` |
| channels-out | `voix-*` | *la voix* | `>` outbound | `> 1 awaiting` |

**Rename note** — jr renamed *hippocampe* → *carnet* for the
**visible** glyph (aesthetic + readability grounds, `responses/jr.md`
§(i)). The internal formula prefix may stay `hippocampe-*`; the strip
column reads `carnet`. The rename only affects the user-facing
vocabulary, not the on-disk formula naming.

Emoji shadows (🌊 ❤️ 👁 📓 📣) are *optional viewport tints* — never
the source of truth.

### D2 — Strip format (byte-anchored)

**Accepted:** the strip is one ≤80-column ASCII line, padded to the
canonical `120` cols by `push_padded_line`. The byte layout — frozen
verbatim from `responses/jr.md`:

```
~ 03  . . . * . . . * . .   @ noogram/cosmon   = 4.2k notes   > 1 awaiting
```

With kill-switch:

```
~ 03  . . . * . . . * . .   @ noogram/cosmon   = 4.2k notes   > 1 awaiting   [off]
```

When all values are zero / no state has been written:

```
~ 00  . . . . . . . . . .   @ <galaxy>   = 0 notes   > 0 awaiting
```

The `crates/cosmon-observability/src/render.rs::render_vital_strip`
function is the *single authoritative renderer*. It takes
[`Sensorium`](../../crates/cosmon-observability/src/sensorium.rs) and
returns a `String`. No clock, no env, no locale.

### D3 — File layout (the data plane)

**Accepted:** the sensorium lives under `<state_dir>/sensorium/`:

```
.cosmon/state/sensorium/
├── inbox.ndjson           # peau: one row per landed signal
│                          # {channel, sender, ts, hash}
├── heartbeat.ndjson       # cœur: one row per beat
│                          # {ts, kind, moved:[mol_ids...]}
├── <galaxy>/SOUL.md       # visage: frontmatter `name:`
│                          # optional `seal_drift: true`
├── notes/*.md             # carnet: each note with `decay_at:`
└── outbox/*.md            # voix: each draft with
                           # `permission: pending|granted|denied`
```

Each organ owns its own write path — pure file appends, no shared
schema. The strip reader treats absent files / malformed rows as the
**zero baseline** for that organ. This is the silence rule (§D5).

### D4 — Loader semantics

**Accepted:** the strip aggregate is computed by
`cosmon_cli::sensorium::load_sensorium(&state_dir) -> Sensorium`
(implementation at
`crates/cosmon-cli/src/sensorium.rs`). The loader is:

- **Read-only.** Never writes back, never repairs the input.
- **Tolerant.** Missing file → `0` for the affected organ. Malformed
  JSON / frontmatter → row skipped.
- **Non-blocking.** Walks the tree synchronously; no daemon, no
  inotify, no thread. Called once per `cs peek --snapshot` invocation.
- **Bounded.** Reads only the most recent `HEARTBEAT_WINDOW = 10`
  beats from `heartbeat.ndjson`; reads only files matching
  `*.md` under `notes/` and `outbox/`; reads only the
  lexicographically-first `<galaxy>/SOUL.md` for the visage organ.

The loader lives in the `cosmon-cli` lib (not the binary) so external
integration tests can compute the strip without spawning `cs`.

### D5 — Three silence laws (byte-level)

The visible layer enforces three laws — all measurable from the
byte stream (`cs peek --snapshot`), no log inspection required.

#### L1 — Idempotent rendering

**Same sensorium state → byte-identical output.** The CI
golden-snapshot test (ADR-066 §6) extends from *surface-to-surface
identity* to *tick-to-tick identity when no organ has written*. The
freezing test is
`crates/cosmon-cli/tests/sensorium_strip.rs::canonical_raster_byte_identical_with_sensorium_when_state_unchanged`.
A reviewer can verify by running `cs peek --snapshot > a` and
`cs peek --snapshot > b` consecutively against the same disk state;
`diff a b` MUST exit zero.

#### L2 — No animation without operator gesture

The byte-layer raster contains **no animation hint** by default. A
pulse on `~` for a freshly-landed signal is the *viewport's*
prerogative and requires a recent operator gesture (keystroke,
pointer-move, `cs` invocation in the last 60 minutes). Enforced by
the SwiftUI viewport renderer, not the snapshot bytes. The byte
layer remains pure: `render_vital_strip(&s) == render_vital_strip(&s)`
for any two calls.

#### L3 — Kill-switch visible

When `~/.cosmon/autopilot.off` exists, the strip carries the trailing
`[off]` glyph. **Organs still tick on disk; only the rendering
dims.** Silence is the *guarantee* of the kill-switch, not its
consequence. Loader detection at
`crates/cosmon-cli/src/sensorium.rs::autopilot_off_marker_exists`.

### D6 — UX ↔ CLI parity (ADR-068)

**Accepted:** every byte in the strip is queryable as a structured
JSON object via **`cs sensorium [--json]`**. Stable keys: `peau`,
`coeur`, `visage`, `carnet`, `voix`, `autopilot_off`. Viewports that
want to re-render in their native vocabulary read the JSON shape
instead of re-parsing ASCII.

This satisfies the ADR-068 §1 invariant (*every UI control has a CLI
counterpart*): the menubar, mac-pilot, Souffleur, and Skylight
viewports may read either the strip bytes (the wheat-paste path) or
the structured JSON (the parity path).

---

## Anti-regression — invariants the strip must not break

- **§8k' (ADR-066).** Every glyph above is a byte in `cs peek
  --snapshot`. **Zero new SwiftUI primitive.** Zero per-surface
  rendering.
- **Two-layer model (`docs/architectural-invariants.md`).** The
  strip recomputes on each `cs peek --snapshot` invocation; nothing
  watches the files in the background.
- **`main` est sacré.** The strip is a read-only projection. No
  loader path ever writes to `.cosmon/state/`.
- **Briefing-seal discipline.** When the `visage` organ matures and
  ships its `cs verify --visage` formula, the `seal_drift: true`
  frontmatter field flips and the strip renders `@ <galaxy>!`
  (trailing `!`). Identity drift becomes retrospectively auditable
  via the existing seal-verify discipline, not via a new mechanism.
- **Merge-before-dispatch.** Carnet writes happen in worktrees and
  flow to `main` through `cs done`. No organ writes to `main` outside
  that channel.

---

## Implementation locks

- **Renderer:** `crates/cosmon-observability/src/render.rs::render_vital_strip`.
  Pure function, no I/O, no clock.
- **Data type:** `crates/cosmon-observability/src/sensorium.rs::Sensorium`.
- **Loader:** `crates/cosmon-cli/src/sensorium.rs::load_sensorium`.
- **CLI command:** `cs sensorium` at `crates/cosmon-cli/src/cmd/sensorium.rs`.
- **Wiring:** `crates/cosmon-cli/src/cmd/peek.rs::run_canonical_snapshot`
  loads the sensorium and passes it to `SnapshotConfig::sensorium`.
- **Width constants:** `STRIP_VISIBLE_WIDTH = 80`,
  `HEARTBEAT_WINDOW = 10`. Both are immutable for v0; revising them
  is a successor-ADR move, not a config flag.

---

## Tests

| Test | Pins |
|------|------|
| `vital_strip_zero_baseline` (`render.rs`) | The `~ 00  . . . … = 0 notes  > 0 awaiting` canonical baseline. |
| `vital_strip_jr_canonical_example` (`render.rs`) | The verbatim example from `responses/jr.md`. |
| `vital_strip_kill_switch_appended` | L3 — `[off]` glyph trailing. |
| `vital_strip_seal_drift_marks_galaxy` | Visage seal-drift `!` suffix. |
| `vital_strip_is_pure_ascii_and_within_cap` | 7-bit ASCII; ≤ `STRIP_VISIBLE_WIDTH` chars. |
| `vital_strip_does_not_read_wall_clock` (integration) | L2 — bytes don't depend on time. |
| `canonical_is_byte_identical_tick_to_tick_when_state_unchanged` | L1 — tick-to-tick byte identity. |
| `canonical_raster_byte_identical_with_sensorium_when_state_unchanged` (integration) | L1 over the full raster. |
| `canonical_snapshot_matches_insta_lock` (insta) | Full golden raster including the strip line. |

---

## Consequences

1. The five organs ship in any order. Each writes its own files; the
   strip aggregates whatever is present. A premature `cs peek` against
   a galaxy that has not yet shipped peau-morning-digest renders the
   zero baseline — *not* an error, *not* an empty strip line.
2. Adding a new viewport (Apple TV, e-ink, watch) means writing a
   `WheatPasteView`-style adapter that consumes the byte raster.
   The strip needs zero changes per surface.
3. The `cs sensorium --json` shape is a published contract (ADR-068
   parity). Its top-level keys (`peau`, `coeur`, `visage`, `carnet`,
   `voix`, `autopilot_off`) become stable; per-key shape evolves via
   additive JSON keys only.
4. The decoupling between writers (per-organ formulas) and the
   reader (the strip) means a regression in *any* organ writer never
   crashes `cs peek`. The worst-case failure mode is *silence* for
   the affected glyph.

---

## Non-decisions (explicitly out of scope)

- The implementation of the five organ-writer formulas
  (`peau-morning-digest`, `curate-patrol`, etc.) lives in sibling
  molecules. This ADR ratifies the read contract, not the write
  contracts.
- The `seal_drift` mechanism (BLAKE3 cross-check of SOUL.md) is
  surfaced today as a passthrough frontmatter field. A live
  hash-verify formula is forward work.
- The `cs autopilot tick` system-level orchestration
  (`idea-20260417-66d8`) is independent of the strip — the strip
  reads `~/.cosmon/autopilot.off` regardless of how it was created.
- Viewport-level animation rules (L2 pulse policy) live in the
  SwiftUI / menubar adapter, not in this ADR.
