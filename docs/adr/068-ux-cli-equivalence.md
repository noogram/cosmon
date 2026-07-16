## ADR-068 — Invariant d'équivalence UX ↔ CLI : capability parity entre apps Mac/iOS et `cs` CLI

**Status:** proposed
**Date:** 2026-04-23
**Authoring task:** `task-20260423-b846`
**Parent principle (operator, 2026-04-23):**

> *"Notre UX doit être à équivalence avec notre CLI. On onboard tout type
> d'humain dans l'aventure, aussi bien les techniques qui savent manipuler
> un terminal, qu'un CEO d'entreprise qui a la vision high-level."*

**Sibling UX v2 molecules (applications of this invariant — not blocked
by ADR-068, but governed by it):**

- `task-20260423-cefe` — pilot apps Whispers/Inbox surfaces
- `task-20260423-de93` — apps Galaxies tab
- `task-20260423-21e0` — apps Session UI
- `task-20260423-2b7f` — apps Spark composer
- `task-20260423-f74e` — apps onboarding flow
- `task-20260423-d3ae` — Cluster views (carries the `cs peek` transcription work — see §Decision 2)
- `task-20260423-d9a4` — MarkdownRenderer (used by `cs help`/`man cs` Help tab — see §Decision 3)

**Related ADRs:**

- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — three regimes (Inert/Propelled/Autonomous); the equivalence invariant binds *every* regime
- [ADR-023](023-cockpit-hexagonal-read-surface.md) — cockpit as a hexagonal read surface; UX ports are siblings to the CLI port, not derived from it
- [ADR-028](028-cosmon-observability.md) — `cs peek` as the fractal observation portal (the canonical thing to transcribe)
- [ADR-061](061-pilot-session-and-causal-closure.md) — the pilot-session kind; cockpit cognition must be observable from the same referential, regardless of *which* cockpit (CLI or GUI) opened it

**Parent deliberation (UX v2):** `delib-20260423-becf` — wheat-paste / byte-identical cross-device synthesis (referenced in §Scope as §8k of the invariants series, proposed there).

---

### Context

Cosmon today ships two operator surfaces:

1. **The `cs` CLI** — a stateless one-binary command-line tool. Every
   capability of cosmon (nucleate, tackle, evolve, observe, peek, ensemble,
   help, …) is reachable by typing a verb in a terminal. This is the
   canonical surface; it is what every worker uses, what every
   `docs/guides/*.md` walks through, and what `man cs` documents.
2. **The native pilot apps** — `mac-pilot.app` (Mac) and `ios-pilot.app`
   (iOS/iPadOS), both built against the `cs-api` HTTP layer (ADR sibling
   `task-20260422-cs-api`). Today they expose three or four tabs
   (Session, Whispers, Inbox, Galaxies, Settings). They were grown
   feature-by-feature, each new tab solving a particular operator pain.

The two surfaces drifted. `cs spark` exists in CLI from day one; the
Whispers tab in mac-pilot got a "Transformer en spark" button only after
the operator complained. `cs peek` has its three navigation scales
(ville/immeuble/peau) in the terminal; no app has them. `cs help` and
`man cs` are reachable by typing one word in a terminal; the apps have
no Help tab at all. Inversely, `cs cluster bootstrap` does not exist
yet but its UI counterpart was already being sketched — in which case
the GUI would have shipped a capability the CLI cannot express.

This drift is not a bug list. It is a **category error**. The CLI and the
GUI are not different products that occasionally borrow features from
each other. They are two **observation ports** onto the same substrate
(`.cosmon/state/`), in the hexagonal sense (ADR-023). When the operator
opens a terminal or opens an app, they are not choosing a different
cosmon — they are choosing a different *grip* on the same cosmon. A grip
that reaches less far than another grip is a regression of the system,
not a property of the surface.

The deliberation that motivates this ADR (`delib-20260423-becf`)
introduced the **wheat-paste invariant** (proposed §8k): any artifact
displayed across surfaces — a spark, a molecule card, a peek view — must
render byte-identical, regardless of device, font scaling, or window
geometry. §8k governs *how things look* across surfaces. ADR-068
governs *what is reachable* across surfaces. Together they form a single
discipline: **two surfaces, one substrate, one truth.**

The operator's framing is sharper than the engineering one: the system
must onboard both the technical CLI native and the high-level GUI-only
CEO, and they must be able to **swap commands trivially** ("envoie-moi
le `cs xxx` qui fait ça" / "colle ça dans l'app, c'est l'équivalent").
This is the literate-programming analogue for user experience: two
faces of one source.

---

### Decision

#### §1. Capability parity per `cs` user-facing verb

For every user-facing `cs <verb>` (verbs that an operator types directly
— `nucleate`, `tackle`, `done`, `collapse`, `peek`, `ensemble`, `inbox`,
`spark`, `session`, `whisper`, `galaxies`, `cluster`, `help`, …; **not**
worker-only verbs like `cs evolve` and `cs complete`), the native pilot
apps (mac-pilot, ios-pilot) MUST expose:

- **(a) An equivalent UI element** — a button, tab, menu item, contextual
  action, or gesture — whose effect is observably identical to invoking
  the CLI verb. The UI form is free; only the resulting state transition
  is constrained. (E.g. `cs done <id>` may be exposed as a contextual
  swipe action on a molecule row, not necessarily as a literal "Done"
  button.)

- **(b) A "Reveal CLI" mode** — every action exposed in the UI MUST be
  able to display the equivalent `cs <verb> <args>` command that would
  produce the same effect. This is the analogue of Xcode's *Network
  inspector → "Copy as cURL"* and Chrome DevTools' *"Copy as fetch"*.
  An operator who triggers an action in the GUI can always copy a
  shareable command, paste it in a terminal or in a chat with a
  technical teammate, and reproduce the result.

- **(c) An "Import CLI" mode** — every UI surface that admits operator
  input MUST accept a pasted `cs <verb> <args>` string and execute it as
  if the operator had clicked the equivalent UI control. The CEO who
  receives a `cs spark "..."` from a technical colleague must be able
  to paste it into the app and have it run, without opening a terminal.

This is the **bidirectional revelation** principle (§8m below): every
operator-facing action exists in both forms simultaneously, and the
operator can move freely between them.

#### §2. `cs peek` and `cs ensemble` — three-scale byte-identical transcription

> *Vocabulary correction (task-20260428-5a35, verdict
> task-20260427-d604).* Earlier drafts of this ADR labelled the three zoom
> scales of `cs peek` as "fractal". C4 was falsified at that layer:
> ville / immeuble / peau call three structurally distinct rendering paths
> with no shared `render(scope)` primitive. The fractality at the zoom
> layer was literary, not structural. The transcription requirement below
> is unchanged — the GUI must reproduce the three scales byte-identically;
> the only correction is dropping the "fractal" label, which would mislead
> a future reader into expecting a recursive primitive that does not exist.

Two CLI commands are **observation portals** rather than state mutators:
`cs peek` (the three-scale fleet view, ADR-028) and `cs ensemble`
(the actionable backlog snapshot). Both have a privileged position in
the operator's daily life and both produce information-dense output that
the GUI surfaces must transcribe **integrally**, not summarize.

For `cs peek`:

- **Three scales identical to the TUI** — *ville* (full fleet, one row
  per molecule), *immeuble* (single molecule full-page with two
  neighbor cables `│`), *peau* (raw artefact text). The same content,
  the same density, the same column-aligned monospace rendering.
- **Same transitions** — keyboard `+` / `-` for stepwise zoom-in /
  zoom-out, `=` for snap-back-to-ville (desktop). Pinch-to-zoom on
  iOS/iPadOS as the native gesture equivalent. The intermediate
  half-and-half views from the TUI (left-half remains *ville*, right-half
  is already *immeuble*) MUST be reproduced — the transition itself
  carries information (chronicle 2026-04-22 *"entre deux échelles, le
  passage lui-même est une information"*).
- **Same scale-to-scale navigation** — clicking (or tapping) a galaxy row in
  *ville* descends into its *immeuble* scale; clicking a molecule in
  *immeuble* descends into its *peau* scale. A breadcrumb at the top
  (`cluster ▸ galaxy ▸ molecule ▸ artefact`) makes the position
  unambiguous.
- **Reveal CLI** (§1b) — a button at top-right shows the `cs peek
  --scale ... --focus ...` command that would produce the current
  state, copyable in one tap.
- **Monospace byte-identical** — width-fixed (120 cols TUI default,
  responsively centered on wider screens, but *never* word-wrapped or
  reflowed) per §8k wheat-paste. The same operator can read the same
  state from a 13" laptop terminal, a 27" desktop, and an iPhone, and
  the bytes are identical.

For `cs ensemble`:

- The same actionable backlog snapshot, with the same tag filtering
  (`temp:hot`, `temp:warm`, `temp:cold`, `temp:frozen`, `stream:*`).
- Per-row contextual actions for `cs tackle`, `cs done`, `cs collapse`
  (these are §1a; the contextual action's *Reveal CLI* surfaces the
  exact command).
- A "Reveal CLI" mode showing the equivalent `cs ensemble --tag ...
  --json` invocation, copyable.

#### §3. `cs help` / `man cs` — first-class Help tab

The CLI ships its own discovery surface (`cs --help`, `cs <verb> --help`,
`cs help guide`, `man cs`). The apps today have **no equivalent**. The
CEO who installs `mac-pilot.app` cannot discover the system's
vocabulary without opening a terminal — a violation of §1.

The decision:

- Add a **Help** tab to mac-pilot and ios-pilot.
- The tab content is the **same prose** as `cs help` and `man cs`,
  rendered through the MarkdownRenderer delivered by sibling task
  `task-20260423-d9a4`.
- The Help tab is **not** a separate documentation source. It is the
  canonical source rendered into a different surface. When `cs help`
  is updated, the apps reflect it on their next `cs-api` poll. There
  is no app-side fork.
- Each verb description in the Help tab carries a "Try it" affordance
  (depending on the verb's nature: a one-shot action button for safe
  verbs, a pre-filled composer for verbs taking input).

This satisfies §1 for the meta-verb `cs help` itself: the CEO can
discover cosmon's vocabulary without leaving the app, and the technical
operator can read the same prose from `man cs` in a terminal.

---

### Invariants (added to `docs/architectural-invariants.md`)

**§8l. Capability parity (UX ↔ CLI).** For every user-facing `cs
<verb>`, at least one observable path exists in the native pilot apps
(mac-pilot, ios-pilot) that produces the same state transition. For
every user-facing UI control in the apps, at least one `cs <verb>`
invocation produces the same state transition. The bijection is
maintained as the system evolves: a worker that adds a new CLI verb
also extends the apps; a worker that adds a new UI control also extends
the CLI. Drift is detectable by the audit guide
[`docs/guides/ux-cli-parity-audit.md`](../guides/ux-cli-parity-audit.md)
and any gap is a documented bead, never silent.

**§8m. Bidirectional revelation.** Every UI action exposes the
equivalent `cs <verb> <args>` command (the *Reveal CLI* mode). Every
CLI command can be pasted into a UI input that admits commands and
executed as if the operator had used the equivalent control (the
*Import CLI* mode). The two forms are not parallel surfaces with a
mapping table maintained by hand — they are projections of the same
typed action object, computed at the API boundary. The operator can
move freely between forms; the system can be onboarded technical-to-GUI
or GUI-to-technical with the same vocabulary.

These invariants do **not** add a new primitive. They constrain the
existing CLI/UI port pair — both ports speak to the same hexagonal
core (ADR-023) — by demanding their signatures match. §8l constrains
*what* the ports expose; §8m constrains *how* the operator can
translate between them.

---

### Coherence checklist

| # | Question | Answer |
|---|----------|--------|
| 1 | Stateless? | Yes — no new command, no new daemon, no new state store. The invariant constrains existing ports. |
| 2 | Idempotent? | N/A (no new command). |
| 3 | Regime-aware? | Yes — applies to all three regimes. Inert (browse pending), Propelled (tackle/peek/done), Autonomous (read-only observation). |
| 4 | Single perimeter? | Yes — no new perimeter, only a coverage discipline over existing ports. |
| 5 | Symmetric undo? | N/A — this ADR ratifies parity, not new operations. Each underlying verb keeps its own symmetric undo. |
| 6 | Runtime-compatible? | Yes — a future resident runtime exposes the same hexagonal port; the apps continue to be one of N adapters. |
| 7 | Worker/human boundary? | Respected — only **user-facing** verbs are subject to §8l. Worker-internal verbs (`cs evolve`, `cs complete`) are out of scope; workers stay CLI-only per §3e. |
| 8 | Write/read asymmetry? | Preserved — *Reveal CLI* is a pure projection of the action; *Import CLI* dispatches the same action object. No port writes state and returns a coupling report. |
| 9 | Merge-before-dispatch? | N/A — ADR-068 does not change dispatch semantics. |
| 10 | CLI-first for workers? | Reinforced — workers stay CLI-only. The apps target *humans*. |
| 11 | Scope-bounded? | Yes — bounded to user-facing verbs; worker verbs explicitly excluded. The audit guide enumerates the scope. |
| 12 | Self-similar? | Yes — composes at single-verb (one button = one command), at workflow (a sequence of UI clicks = a script of CLI calls), at fleet (multi-galaxy peek = `cs peek --all`). |
| 13 | Alphabet-Closure? | Yes — when a new verb lands in `cs`, its UI counterpart and the audit row land in the **same** PR. Non-negotiable. The CLAUDE.md addition codifies this. |

Nothing in this ADR contradicts an existing invariant. §8k (wheat-paste,
proposed in `delib-20260423-becf`) is the visual sibling of §8l/§8m:
§8k governs *how things look* identical across surfaces; §8l/§8m govern
*what is reachable* across surfaces. They compose without conflict.

---

### Consequences

#### Positive

- **One product, two grips.** The operator stops choosing a "lite" or
  "pro" version of cosmon. Every grip reaches the whole substrate.
  Onboarding a new operator no longer asks the question "are you a
  terminal person or an app person?" — both paths reach the same
  destination.
- **Vocabulary preserved across operator types.** A CEO and a CLI
  native discussing the same molecule can name actions identically.
  *"Tackle the inbox top hot"* means the same thing whether the
  speaker plans to type `cs tackle <id>` or to swipe in the app.
- **Documentation halves.** `cs help` and `man cs` are written once;
  the Help tab renders the same prose. Guides under `docs/guides/`
  describe the CLI canonically and add an "App equivalent" call-out
  per section, not a parallel app manual.
- **Sharing across operators is trivial.** The CEO who hits a wall
  asks the technical teammate, who answers `cs xxx`. The CEO pastes,
  the action runs, the loop closes. No screenshots, no
  "click-on-the-third-icon", no parallel vocabulary.
- **Audit detects regression.** A new verb without an app surface
  shows up in `docs/guides/ux-cli-parity-audit.md` as a row with
  ❌ in the UI column. The audit becomes a CI candidate (future
  work — see Open questions).

#### Neutral / accepted costs

- **Every CLI PR touches at least the audit table.** A worker who adds
  a CLI verb adds a row in `ux-cli-parity-audit.md` and either lands
  the UI counterpart in the same PR or files a bead (`temp:warm`)
  for it. This raises the per-PR scope slightly but eliminates a class
  of silent drift.
- **The apps gain Help tabs and a CLI composer.** Each is a small
  amount of work (the MarkdownRenderer of `task-20260423-d9a4` is
  already in flight; the *Import CLI* composer is one text input + a
  thin parser sharing the CLI's clap definitions via `cs-api`).
- **§1c "Import CLI" requires a parser.** The parser cannot be a full
  shell — it is restricted to `cs <verb> <args>` syntax, with the
  same flag schema as the CLI. The implementation reuses `clap`'s
  parser via the `cs-api` boundary; no shell escapes, no pipes, no
  redirection. Out-of-scope inputs are rejected with a clear error.

#### Negative (risks)

- **Bloat in the apps.** Every CLI verb gets a UI surface; the apps
  could grow into kitchen-sink interfaces. Mitigation: §1a leaves the
  UI form free — many verbs collapse into contextual menus or
  composer presets, not into top-level tabs. The audit guide
  prioritizes (v1 / v2) so the surface area is staged, not exploded
  at once.
- **Reveal-CLI as a spec-leak.** Every UI action that exposes a
  literal command commits the apps to that command's argument shape.
  Renaming a flag becomes a UX regression. Mitigation: the *Reveal
  CLI* output is computed from the same `clap` definitions the CLI
  uses; renames propagate automatically. The risk is real but
  bounded.
- **Operator confusion across forms.** A CEO who learns "swipe-right
  = done" may be surprised that `cs done` also requires `--reason`
  for collapsed molecules but not for completed ones. The Reveal-CLI
  mode mitigates by *showing* the asymmetry in the same UI flow.

---

### Alternatives considered

#### A. GUI-only (rejected)

Pursue a fully visual cosmon, dropping the CLI for end-users. Rejected
because the technical operator population is the actual builder of
cosmon and the daily user; removing the CLI breaks the substrate.
Workers (always programmatic) need the CLI by §3e; humans technical
enough to read this ADR overwhelmingly prefer it. The CLI is
non-negotiable.

#### B. CLI-only with thin GUI wrappers (rejected)

Keep the CLI canonical; the apps are read-only viewers that shell out
to `cs` for any state-changing action. Rejected because the
high-level GUI-only operator (the CEO) is excluded by construction:
they cannot install Cargo, cannot type a verb, and cannot read a
terminal output that wraps over 80 columns. The system that excludes
them is not a substrate; it is a tool.

#### C. Mobile-first responsive web (rejected)

Drop the native apps in favor of a web UI that works on any device.
Rejected because it leaves the cosmon-native ecosystem (one binary,
no daemon, JSON files on disk) and introduces a hosting story, an
auth story, and a latency story that defeat the wedge. The native
apps stay close to the substrate (they speak `cs-api` over Tailscale
to the operator's own machine); the web alternative would invert
that posture.

---

### Scope and non-scope

**In scope (this ADR).**

- Naming the invariant of capability parity (§8l) and bidirectional
  revelation (§8m).
- Listing the three categories of UI/CLI alignment: per-verb
  parity (§1), `cs peek`/`cs ensemble` integral transcription (§2),
  Help tab (§3).
- Producing the audit guide `docs/guides/ux-cli-parity-audit.md` that
  enumerates every user-facing verb's current state and gap.
- Updating CLAUDE.md to record the invariant in § Conventions.
- Updating `docs/architectural-invariants.md` with §8l and §8m.
- Crosslinking the in-flight UX v2 molecules (cefe / de93 / 21e0 /
  2b7f / f74e / d3ae / d9a4) as applications of the invariant.

**Out of scope (this ADR).**

- Writing UI code in mac-pilot or ios-pilot. The implementations
  belong to the sibling tasks listed in the front matter.
- Specifying the *Reveal CLI* and *Import CLI* parser implementations.
  The contract is fixed (parser shares clap definitions via cs-api);
  the code is the sibling tasks' responsibility.
- Rendering policy for `cs help` / `man cs` markdown. That belongs to
  `task-20260423-d9a4`.
- The `cs peek --all` cross-galaxy aggregation in the apps. That is
  scoped under `task-20260423-d3ae` (Cluster views), with the
  "Reveal CLI" addition routed there via `cs whisper`.
- A CI gate that fails when the audit table has untreated gaps. Open
  question (see below).
- A unified "terminal in app" pseudo-TTY view. Explicitly excluded
  by the scope-guard *"capability parity is conceptual, not visual"*
  — a UI action may take a different visual form from the terminal
  rendering as long as the effect is the same.

---

### Open questions (deferred to implementation siblings)

1. **Should §8l be CI-gated?** A CI check that parses the audit table
   and refuses a merge that adds a CLI verb without a `temp:warm` UI
   bead would mechanize the discipline. Proposed: yes, after the v1
   gap is closed. Opens a follow-up task (suggested formula:
   `parity-audit-lint`).

2. **What is the canonical *Import CLI* parser?** Two options: (a) a
   thin in-app parser sharing the CLI's clap schema via cs-api; (b)
   shipping the CLI binary inside the apps and shelling out. (a) is
   cleaner and cross-platform; (b) preserves bit-identical behavior.
   Recommended: (a). Decided in the sibling task that lands the
   composer.

3. **How does *Reveal CLI* handle authentication / scoping?** A `cs
   tackle` triggered from the GUI on the operator's own machine maps
   trivially. A `cs tackle` shown for a remote galaxy via `cs-api`
   needs an `--galaxy` flag the local CLI does not require. The
   reveal must show the **transferable** form, not the local form.
   Decided in the cs-api evolution that lands `--galaxy`.

4. **Help tab versioning.** When the apps connect to a `cs-api` whose
   version differs from the apps' bundled help cache, which one
   wins? Proposed: server (the substrate is canonical), with a
   "stale help" warning if the apps cannot reach the server.

---

### Acceptance

This ADR is **proposed**. Operator ratification is the explicit next
step. Until ratified:

- The audit guide (`docs/guides/ux-cli-parity-audit.md`) is published
  and the v1 priorities are open for the in-flight UX v2 siblings to
  consume.
- §8l and §8m land in `docs/architectural-invariants.md` as
  *(proposed — ADR-068)*.
- A line lands in CLAUDE.md § Conventions so any worker reading the
  conventions on a new task already knows the rule.
- Cross-references are added bidirectionally between this ADR and
  the in-flight UX v2 molecules' briefings.
- A whisper to `task-20260423-d3ae` carries the four-point
  enrichment for the Cluster Peek sub-view (monospace 120 cols,
  three scales with `+/-/=` and pinch, breadcrumb scale-to-scale
  navigation, Reveal-CLI button).

The motto, paraphrased from the operator: *deux portes d'entrée à la
même maison ; derrière les portes, exactement les mêmes pièces.*
