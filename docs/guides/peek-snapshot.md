# `cs peek --snapshot` — the wheat-paste view

**TL;DR.** `cs peek --snapshot` prints a byte-deterministic, fixed-width
(120 cols), ASCII-only view of the fleet. The same fleet state produces
byte-identical output on iPhone-Blink, iPad-Blink, MacBook-Ghostty, and
AWS-SSH-tmux. Two captures `diff` to **zero** bytes.

This is the canonical rule restated for developers and PR reviewers.

## Why

The deliberation `delib-20260422-f52c` (JR §2) refused responsive-CSS —
where a small screen learns *less* than a big one — and named the
opposite rule **wheat-paste**: paint the wall the same way everywhere,
let the small screen scroll more, but make sure both see the same face.

Responsive layouts hide information as a function of width. The phone
user becomes structurally second-class: tables collapse into cards,
sidebars vanish, summary numbers are dropped. Wheat-paste refuses that
asymmetry.

## The rule (citable in code review)

1. `cs peek --snapshot` MUST NOT call `tput cols`, `TIOCGWINSZ`, or read
   `$COLUMNS` / `$ROWS` / `$TERM` **for layout decisions**. Width is a
   constant in the code (`CANONICAL_WIDTH = 120`).
2. A viewport layer MAY read device width to place scroll indicators
   (right-edge arrows, scroll position) — these live **on top of** the
   canonical stream, not inside it.
3. The output is deterministic: same fleet state → same bytes. No
   timestamp header in snapshot mode.

If a reviewer sees `if width < 80 then compact` inside the layout
logic, the PR fails the rule.

## Using the flag

```bash
# Print the canonical view once to stdout, then exit.
cs peek --snapshot

# Capture from two different devices and diff them.
ssh phone  'cs peek --snapshot' > /tmp/a
ssh laptop 'cs peek --snapshot' > /tmp/b
diff /tmp/a /tmp/b   # must print nothing
```

## What the output looks like

Every line is exactly 120 columns wide, padded with trailing spaces.
Pure ASCII, no color codes, no emoji — so *bytes == columns*.

```
COSMON FLEET SNAPSHOT v1
========================================================================================================================

## MOLECULES
  MOLECULE                 TITLE                KIND         STATUS     WORKER             SESSION            ENERGY
------------------------------------------------------------------------------------------------------------------------
  mol-alpha                Alpha task           task         running    w-alpha            cosmon-alpha       1.5K
  mol-beta                 Beta issue           issue        pending    w-beta             cosmon-beta        0
...
```

## Projection onto real devices

| Device                      | Visible viewport          | Behavior                                       |
|-----------------------------|---------------------------|------------------------------------------------|
| iPhone-Blink (portrait)     | ~40 cols × 25 rows        | Letterbox on 120-col wall; horizontal pan; "more →" indicator. |
| iPad-Blink (landscape)      | ~100 cols × 40 rows       | Most fits; narrow horizontal scroll.           |
| MacBook-Ghostty             | 180+ cols × 50 rows       | 120-col wall centered with blank margins.      |
| AWS-SSH tmux pane (80 cols) | 80 cols                   | Works like iPhone horizontally; tmux pans.     |

Empty margins on a large screen are the rule's signature. A larger
screen is not rewarded with *more information*, but with *more air*.

## Where the rule is enforced

- Pure render function: `cosmon_observability::render::render_canonical`.
- Tests: `crates/cosmon-observability/tests/canonical_snapshot.rs`
  - `canonical_snapshot_matches_insta_lock` — locks the exact bytes.
  - `canonical_is_invariant_under_tty_envvars` — freezes the contract
    that `$COLUMNS`, `$ROWS`, `$TERM`, `$LC_*`, `$LANG` never perturb
    the output.
  - `canonical_every_line_is_canonical_width_wide` — pastille-sized
    canvas, never cut.
  - `canonical_is_byte_identical_across_repeated_calls` — pure function.
  - `canonical_is_ascii_only` — bytes equal columns, always.

## Related

- Parent deliberation: `delib-20260422-f52c` (JR §2/§3/§4).
- Parent rule (refused responsive-CSS): `delib-20260422-f6d6`.
- Chronicle (wheat-paste analogy): an internal chronicle — to be
  written after this lands.
