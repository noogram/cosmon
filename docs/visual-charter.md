# Cosmon visual charter

The visual charter is the single source of truth for how cosmon concepts
*look*. It is defined in one TOML file and consumed by every renderer —
`cs watch`, `cs run`, `cs help charter`, and the Horizon HTTP cockpit.

## The one-struct-two-renderers rule

```
                crates/cosmon-core/src/visual.toml
                          |        |
                          v        v
                     Role / Status / EnergyBucket
                          |
                          v
                     VisualToken
                   /             \
                  v               v
       cosmon-style::ansi      cosmon-style::css
       (ANSI TrueColor)        (/charter.css)
```

- **One struct**: [`VisualToken { role, status, energy }`](../crates/cosmon-core/src/visual.rs)
  lives in `cosmon-core`. Renderers receive it and nothing else.
- **One TOML**: [`crates/cosmon-core/src/visual.toml`](../crates/cosmon-core/src/visual.toml)
  is the only palette source. It is `include_str!`ed so the binary
  has no runtime file dependency.
- **Two renderers**: `cosmon-style::ansi` paints terminal strings in
  TrueColor; `cosmon-style::css` emits the stylesheet served at
  `/charter.css`. Neither is allowed to invent a concept the other
  doesn't know.

**A new concept lands in `visual.toml` first.** Renderers pick it up
automatically. If a renderer needs something the TOML cannot express,
the TOML schema extends — not the renderer.

## Three orthogonal axes (jr directive, 2026-04-11)

| Axis    | What it encodes | Where it lives visually   |
|---------|-----------------|---------------------------|
| HUE     | **Role** — which crew owns the molecule | fill color of the row |
| STROKE  | **Status** — where it sits in its lifecycle | border weight / dash / opacity |
| SPARK   | **Energy bucket** — how much it costs  | monochrome `▁..█` glyph   |

> **Status never repaints the fill.** The jr directive is absolute on
> this: a running row and a waiting row with the same role have the
> same hue. The difference is the border language.

### Why energy is monochrome

Painting cost red would collide with the `stuck` overlay (vermilion).
The eye cannot tell "expensive" from "needs human attention" if both
look the same. Energy gets its own axis (sparkline glyphs) and no
color.

## Role palette (HUE)

Daltonian-tuned. Verified for deuteranopia + protanopia before landing.

| Role slot      | Name       | Hex       | Maps to `AgentRole` | Semantic           |
|----------------|------------|-----------|---------------------|--------------------|
| `writer`       | azure      | `#4C9AFF` | `Implementation`    | Code worker        |
| `reviewer`     | amber      | `#F5A623` | `Validation`        | A–F review, gates  |
| `fact_checker` | magenta    | `#D946EF` | `Research`          | Source verifier    |
| `editor`       | emerald    | `#10B981` | `Orchestration`     | Coordinator        |
| `chief`        | parchment  | `#E8E1C4` | `Advisory`          | Strategy & counsel |
| `patrol`       | slate      | `#94A3B8` | `Infrastructure`    | Fleet health       |
| `pilot`        | vermilion  | `#EF4444` | —                   | Human-in-the-loop  |

`pilot` has no `AgentRole` peer — it represents the human operator and
is also borrowed as the `stuck` overlay color (see below).

## Status language (STROKE)

Status is rendered as a border grammar over the role hue. The fill
stays the role color; only the edge changes.

| Status     | Glyph | Domain status    | Stroke              | Fill opacity | Notes              |
|------------|:-----:|------------------|---------------------|:------------:|--------------------|
| `pending`  | `○`   | `Pending`        | 1px dashed          | 0.40         | Empty container    |
| `waiting`  | `◐`   | `Queued`         | 2px solid, 35% sat  | 1.00         | Drained, low sat   |
| `active`   | `●`   | `Running`        | 2px solid, full sat | 1.00         | The only full-sat  |
| `stuck`    | `◉`   | `Frozen`         | 2px solid vermilion overlay | 1.00 | Calls the pilot    |
| `completed`| `◌`   | `Completed`      | 1px solid           | 0.60         | Fades back         |
| `collapsed`| `·`   | `Collapsed`      | no border           | 0.15         | Near-invisible     |

`Queued` renders as `waiting` (not `queued`) because visually the state
is "drained, waiting its turn", not "queue ticket". Domain truth stays
in [`cosmon_core::molecule::MoleculeStatus`]; [`cosmon_core::visual::Status::for_molecule_status`]
is the single mapping point.

## Energy sparkline (SPARK)

| Bucket | Glyph | Fraction upper bound |
|--------|:-----:|:--------------------:|
| `B0`   | `▁`   | 0.125                |
| `B1`   | `▂`   | 0.250                |
| `B2`   | `▃`   | 0.375                |
| `B3`   | `▄`   | 0.500                |
| `B4`   | `▅`   | 0.625                |
| `B5`   | `▆`   | 0.750                |
| `B6`   | `▇`   | 0.875                |
| `B7`   | `█`   | 1.000                |

Use [`EnergyBucket::from_fraction`](../crates/cosmon-core/src/visual.rs)
to assign a normalized cost to its octile.

## ANSI rendering rule

The CLI renderer uses **TrueColor** (`ESC[38;2;R;G;Bm`), never the 16
base colors. The 16-color palette remaps per terminal theme and would
make `cs watch` drift from `/charter.css` depending on the operator's
dotfiles. If a terminal only speaks 256 colors, use
[`truecolor_to_256_cube`](../crates/cosmon-core/src/visual.rs) to snap
the hex to the 216-color cube (16..231) and emit
`ESC[38;5;<idx>m`.

## CSS rendering rule

Every role becomes:

- `--cs-role-<slug>` CSS variable on `:root`.
- `.cs-role-<slug>` utility class (sets `background` and `color`).

Every status becomes a `.cs-status-<slug>` utility class that emits
the border language described above. The inline `<style>` block in
`static/index.html` may no longer contain palette hex values — it
links to `/charter.css` and uses the generated variables.

## Where it's implemented

| Layer        | File                                              |
|--------------|---------------------------------------------------|
| Palette      | `crates/cosmon-core/src/visual.toml`              |
| Domain types | `crates/cosmon-core/src/visual.rs`                |
| ANSI adapter | `crates/cosmon-style/src/ansi.rs`                 |
| CSS adapter  | `crates/cosmon-style/src/css.rs`                  |
| Swatch       | `crates/cosmon-style/src/swatch.rs`               |
| CLI consumer | `crates/cosmon-cli/src/event_log.rs`              |
| HTTP consumer| `crates/cosmon-cockpit-http/src/main.rs` (`/charter.css`) |

## How to add a concept

1. Edit [`visual.toml`](../crates/cosmon-core/src/visual.toml). A new
   role is a `[roles.<slug>]` section; a new status is `[statuses.<slug>]`.
2. Add the matching variant to [`Role`] / [`Status`] / [`EnergyBucket`]
   in `visual.rs` — keep the slug in the match arm aligned with the
   TOML key.
3. If it maps to a domain type, extend the `for_*` helpers.
4. Add a row to this document's mapping table.
5. `cargo test -p cosmon-core visual` and `cargo test -p cosmon-style`
   — the `every_role_has_a_spec` / `every_status_has_a_spec` tests will
   catch anything missing.

## Anti-patterns

- **Hand-picking an ANSI color.** `.red()`, `.yellow()` etc. are
  banned outside `cosmon-style::ansi::paint_hex`.
- **Hex codes in `index.html`.** Only `--bg` (outer canvas, darker
  than any charter slot) may stay local. Everything else comes from
  `/charter.css`.
- **Encoding status in fill color.** Status is a border grammar.
  If you want a new distinction, add a glyph or a stroke modifier,
  not a hue.
- **Extending a renderer past the TOML.** If the TOML doesn't have
  the concept, the renderer doesn't either. Extend the TOML first.
