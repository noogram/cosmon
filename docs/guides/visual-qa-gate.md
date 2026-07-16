# Visual-QA Gate (`G_visual`) — operator & worker guide

**One sentence:** before a molecule that produces something a human will
*look at* (a deck, slides, a poster, a rendered diagram, a printable
document) is allowed to complete, it must render the deliverable,
**read the pixels**, and pass a binary adversarial layout checklist —
fail-closed, iterating render→read→correct until every page is right.

Substrate: [`.cosmon/formulas/visual-qa.formula.toml`](../../.cosmon/formulas/visual-qa.formula.toml),
[ADR-120](../adr/120-visual-qa-gate-primitive.md).

---

## Why this exists

`task-work` checks that code compiles. `editorial-work` checks that
prose is coherent. **Neither looks at the rendered page.** A deck can
compile, print to a valid PDF, pass every prose check, and still ship
columns that don't align, a block bleeding over the footer, or a big
empty hole under a title. "The PDF was generated" is not "the PDF is
good."

This bit us for real: a `echo` 7-slide deck shipped 3 broken
slides and needed a human-requested corrective pass after the fact. The
gate makes that interception automatic. Full story:
`/srv/cosmon/echo/presentation/LAYOUT-QA.md`.

## When does it apply?

The test is one question: **does this molecule emit something a human
will LOOK at?**

- Yes → the gate applies (HTML deck, PDF slides, poster, rendered
  chart/diagram, printable doc).
- No → it does not. Code-only → `task-work`. Prose-only with no rendered
  surface → `editorial-work`.

## Two layers — and why wiring lumen alone is only half

| Layer | Tool | Catches | Misses |
|-------|------|---------|--------|
| **Floor (objective)** | `lumen visual-audit` | cross-block overlap, pixel drift — *collisions* | empty zones, imbalance (regions that wrongly *don't* touch) |
| **Ceiling (semantic)** | worker reads each PNG vs the checklist | empty zones, column imbalance, off-grid edges, footer-rule crossings | nothing the pixels don't show (fonts on another machine, interactivity) |

lumen is the deterministic floor; the vision checklist is the ceiling
it cannot reach. The echo failure was an *empty zone* — **not** a
collision — so a pure overlap detector would have passed it. Both layers
run; either can fail the gate. If `lumen` is not on PATH the gate
degrades to checklist-only and **says so** in the verdict — never skips.

## The two composition modes

**Standalone** — the gate as its own molecule, blocking the producer:

```
cs nucleate visual-qa --var artifact=presentation/deck.pdf \
  --var render_cmd="just render-deck"
cs tackle <vqa-id>; cs wait <vqa-id> &
# producer is done only after the gate passes
```

**Inline** — the producing worker runs the gate steps before
`cs complete`. Recommended when the same worker holds the source. The
formula text is the checklist of record either way.

## The binary adversarial checklist (per page, every box = YES)

**Balance & fill**
- [ ] No abnormal empty zone (no contiguous blank > ~20% of the page
      outside intended margins).
- [ ] Left/right columns have comparable heights.
- [ ] Main block vertically centred or top-aligned — never floating
      mid-page with a void above AND below.

**Alignment** (the echo failure class)
- [ ] Cards/lists/boxes share a grid: left edges aligned, gutters
      regular, **card tops and bottoms line up across columns**.
- [ ] Bullets/numbers vertically aligned with each other.
- [ ] Title, header band, footer on the SAME margins as the body.

**Overlap & overflow**
- [ ] No overlap (text/text, text/figure, label out of box, equation
      biting an edge).
- [ ] Nothing clipped by an edge or by pagination.
- [ ] **No block crosses the footer separator** or bleeds into
      footer/margin.
- [ ] SVG/diagrams sit inside their card with a margin.

**Legibility**
- [ ] Sufficient contrast; no un-rendered math (no raw `\frac` visible).
- [ ] Footer bibliography/credits present, not overflowing onto a
      colliding second line.

Any NO → diagnose the HTML/CSS/Markdown → correct → re-render → re-read.
Loop until all pages pass all boxes. Log each defect+fix in
`visual-iterations.md`.

## Rasterising — the mechanics

- PDF → `pdftoppm -png -r 100 deck.pdf page`
- HTML → Chrome headless `--print-to-pdf` first (xelatex/wkhtml drop
  emojis — CLAUDE.md markdown-rendering rule), then `pdftoppm`.
- **Live URL / deployed site** (client-side JS — e.g. a doc-site whose
  Mermaid renders in the browser) → drive a **headless browser** and
  screenshot:
  `chrome --headless --screenshot=page.png --window-size=1280,2000 <url>`,
  or the **`playwright-headless`** MCP (one isolated Chromium per
  session). Wait for the client-side render to settle before the shot.
- Then **READ each PNG** with vision. Confirming the file exists is not
  reading it.

### Mobile / narrow-viewport shots on macOS — the min-width trap

Checking a responsive layout at a phone width? On macOS, Chrome (headed
*and* headless via `--window-size`) enforces a **minimum window width of
~400–500 px**. Ask for `--window-size=375,812` (iPhone width) and you do
**not** get a 375 px-wide reflowed page — you get the page laid out at
the clamped ~500 px width and then **cropped** to 375 px. You are reading
a *rognée* desktop-ish layout, not the mobile layout, and the checklist
verdict is meaningless: real mobile breakpoints (stacked columns,
hamburger menu, larger tap targets) never engage.

To actually exercise a sub-500 px breakpoint, drive the DevTools device
metrics override instead of the OS window — i.e. the
**`playwright-headless`** MCP (or Playwright/Puppeteer directly), which
sets the *emulated* viewport independent of the host window:

```
# playwright-headless: browser_resize to 375×812 emulates the viewport,
# then browser_take_screenshot — the page reflows to the mobile layout.
```

`chrome --headless --window-size=375,2000 <url>` is fine for **≥500 px**
widths (tablet, small-desktop); below that it silently crops. If a shot
looks like a desktop layout squeezed into a phone frame, this is why —
switch to viewport emulation before reading the pixels.

> ⛔ **Headless only — never `playwright-extension`.** A fleet worker has
> no attached Chrome and no browser extension, so the
> `playwright-extension` MCP (which drives the *operator's* logged-in
> Chrome) can never respond — its first call hangs forever and the worker
> never reaches `cs evolve`. That server is operator-cockpit-only. A
> cosmon-docs deploy worker once hung 50+ min on a single
> "Calling playwright-extension…" while the deploy had already succeeded;
> only the visual-QA leg deadlocked (`task-20260617-6ae2`,
> [chronicle 2026-06-17](../lore/CHRONICLES.md)). Pilot-side `curl` QA
> (HTTP status / SSL / leak-token grep) is a fine fallback for non-visual
> gates, but it **cannot** confirm a client-side Mermaid render — that
> needs headless Chrome.
>
> **Now enforced structurally, not by memory.** Since `task-20260704-f153`,
> `cs tackle` strips the operator-bound browser MCP servers
> (`playwright-extension`, `claude-in-chrome`) from every headless worker's
> toolset at the spawn boundary — the worker `claude` is launched with
> `--disallowedTools 'mcp__playwright-extension mcp__claude-in-chrome'`
> (see [`cosmon_cli::tackle_env::build_claude_command`] and
> `OPERATOR_BOUND_BROWSER_MCPS`). A worker that reaches for one of these
> now gets a fast *tool-unavailable* refusal and picks another path,
> instead of freezing for hours on "Frosting…". `playwright-headless` is
> deliberately left in the toolset — it is the tool you want here.

## Artifact paths — do not lose the evidence

Write `visual-verdict.md`, `visual-iterations.md`, and the annotated
PNGs to the **molecule state directory** (`cs --json observe <id>` →
`molecule_dir`). Files in the worktree (`.worktrees/<id>/`) are
**destroyed** by `cs done`. The *corrected source* (HTML/CSS/Markdown)
lives in the versioned worktree and is committed normally; the verdict
records the commit SHA so the rendered state is reproducible.

## What the gate does NOT do

- It does not lock the filesystem. Fail-closed lives in the formula's
  discipline, not in a chmod (invariants §8b — *propose verification, do
  not impose it*). A worker can still skip it; the verdict artifact
  makes the skip observable.
- It does not fan out into child molecules. A FAIL loops in place
  (render→read→correct), exactly like a cargo gate loops until green.
- It does not judge content — only layout. Whether the slide *says* the
  right thing is `editorial-work`'s job.
