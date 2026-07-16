# `cs peek` — zoom continu

*"Ne dessine pas la fractalité — rends-la traversable."* — JR (delib-20260422-f6d6),
the literary mood the surface tries to evoke.

> **Companion axis.** This guide covers the *spatial* dimension
> (ville → immeuble → peau). The *temporal* dimension — running,
> future, past — lives in [`peek-temporalities.md`](peek-temporalities.md).
> The two axes are independent: zoom controls the scale; temporality
> controls the time slice.

> **Vocabulary note (task-20260428-5a35, verdict task-20260427-d604).** The
> three zoom scales below are **navigation marks over a continuous
> interpolation**, not a fractal recursion. C4 was falsified at this layer:
> ville / immeuble / peau call three structurally distinct rendering paths
> (`draw_table` / `draw_immeuble` / `draw_peau`), with no shared `render(scope)`
> primitive. The visual rhyme between scales is consistent design discipline,
> not a parameterized re-rendering. The Koch-style pocket inside `cs peek`
> lives elsewhere — in the `DetailRenderer` trait family (briefing, log,
> events, synthesis, responses, notes, git, tree, verify, tmux). Conflating
> the two would mislead a future reader of the code; the operator's mental
> zoom axis is real, but it is not in the rendering primitive.

`cs peek` is traversable at three fixed marks, navigated by a continuous
zoom level: **ville** (fleet) → **immeuble** (molecule) → **peau** (raw
artifact). Each keypress nudges the zoom level; the TUI splices the two
nearest scales side-by-side so the transition itself carries information —
the operator sees *where in the whole* the current detail sits. The marks
are not derived from one operation; they are three sibling formatters that
share input data and a wheat-paste discipline (bigger characters on the
same wall).

## Keybindings

### Zoom

| Key | Action                                         |
|-----|------------------------------------------------|
| `+` | Zoom in  — ville → immeuble → peau             |
| `-` | Zoom out — peau → immeuble → ville             |
| `=` | Reset to *ville* (fleet table, full-width)     |

### Cockpit actions (task-20260423-16ad, delib-20260423-becf §I)

The operator pilots the fleet from inside `cs peek` — no need to drop to a
terminal. Each key opens a thin modal that fires **one** `cs <verb>` one-shot
when confirmed. Esc cancels every modal without side effects.

| Key | Action          | Runs                                               |
|-----|-----------------|----------------------------------------------------|
| `n` | nucleate        | Modal: formula + topic → `cs nucleate <f> --var topic="..."` |
| `t` | tackle          | Confirm: `cs tackle <selected-id>`                 |
| `m` | merge-and-done  | Confirm (y/N): `cs done <selected-id>` (destructive) |
| `w` | whisper         | Modal: body → `cs whisper <selected-id> -m "<body>"` |
| `.` | session note    | Modal: body → `cs session note "<body>"`           |

Jobs-cockpit rationale (synthesis §I of delib-20260423-becf):
*"the morning IS the portal. There is no second app to learn, no second
surface to keep in sync, no second binary to ship."*

**Name collisions resolved.** `n` used to toggle the notes detail pane and
`t` the tree detail pane; both moved to their shifted counterparts (`N` for
notes, `T` for tree) so the lowercase letters could carry operator actions.
The mouse-capture toggle (previously `m`) is now `M` — same logic.

### Detail panes (toggle on/off)

| Key | Pane               |
|-----|--------------------|
| `p` / `Space` | tmux pane capture |
| `b` | briefing.md        |
| `l` | log.md             |
| `e` | events.jsonl       |
| `s` | synthesis.md       |
| `r` | responses/         |
| `N` | notes/             |
| `g` | git log            |
| `T` | tree view (DAG)    |
| `v` | verify report      |

Step size is `0.25`: five keypresses cross a full scale. The status bar
announces the current zoom label and numeric level after each press.
Zoom is clamped to `[0.00, 2.00]`.

## The three scales

### Ville — density view (`zoom = 0.00`)

The default: the fleet table — quartiers (rows) with pulse, heartbeat,
temperature, energy, and age. No detail. The operator sees which
molecules are alive and which are parked. This is unchanged from the
pre-zoom `cs peek` default, and stays fully compatible with every
existing keybinding (`j/k`, `→/←`, `b`, `l`, `s`, `p`, …).

### Immeuble — one molecule pleine-page (`zoom = 1.00`)

A single molecule becomes the whole wall. Three boxes stacked
vertically: the previous neighbour (above, dim), the current molecule
(centre, cyan + bold), and the next neighbour (below, dim). Each box
shows the molecule id, current step, kind, formula, status, and topic.

Boxes are connected by **DAG cables** — straight monospace characters:

- `│` (solid yellow) — a typed link exists between the two molecules
  (one blocks the other).
- `·` (dim) — the two molecules are spatially adjacent in the filtered
  list but have no DAG relationship.

No layout engine, no force-directed graph — JR's "wheat-paste rule":
bigger characters on the same wall.

### Peau — raw artifact text (`zoom = 2.00`)

The last layer. Raw text at full resolution — citations, commit
hashes, exact bytes. When a detail pane is active (`b`, `l`, `s`,
`e`, `g`, …), peau renders that pane's content pleine-page. When no
detail pane is active, peau falls back to `briefing.md` (or
`prompt.md` if the briefing has not been rendered yet) — the same
artifact any worker reads when it wakes up. The operator sees exactly
what the agent sees.

## Continuous transitions

Zoom values between the three navigation marks show two scales side-by-side
with a ratio that tracks the fractional part:

| Zoom range    | Layout                                   |
|---------------|------------------------------------------|
| `0.00`        | Pure ville                               |
| `(0.00, 1.00)`| ville (left) + immeuble (right), blended |
| `1.00`        | Pure immeuble                            |
| `(1.00, 2.00)`| immeuble (left) + peau (right), blended  |
| `2.00`        | Pure peau                                |

The blend is width-based: at `z = 0.25`, immeuble takes ~25% of the
body area and ville 75%. At `z = 0.75`, the ratio flips. This preserves
the spatial relationship between the overview and the detail at every
instant of the transition — no discrete jump, no orientation loss.

## Intentionally not in scope

- **No graph with force-directed nodes** — "galaxie d'épinards" is
  the explicit anti-pattern. Cables at immeuble scale are straight
  lines between neighbouring boxes only.
- **No Mermaid, no webview, no Tauri, no 3D** — the operator lives
  in the terminal. Plain text, bigger font, fewer neighbours.
- **No new crate dependency** — everything uses the existing ratatui
  scaffold; zoom is a state machine on `App::zoom_level`.

## Reference

Source of truth: `crates/cosmon-cli/src/cmd/peek_tui/mod.rs` — search
for `ZOOM_MIN` / `ZOOM_MAX` / `draw_zoom_body` / `draw_immeuble` /
`draw_peau` / `immeuble_lines` / `molecule_box` / `cable_line`.

Parent deliberation: `delib-20260422-f6d6` — synthesis § "Livrables
décidés" row 7, and § "Convergences" item 4.

Origin task: `task-20260422-1da5` — "Zoom-continu dans cs peek".
