# ADR-018: Pluggable Surface Templates

## Status
**Proposed / Blocked** (2026-04-09)

**Blocked on three concrete template requests.** Do not implement
until the prerequisite is cleared. See *Blocking Condition* below.

## Context

[ADR-017](017-host-native-projection.md) froze the set of surface
rendering modes at three hard-coded variants — `attributed`,
`host-native`, `none`. Each is a distinct code path in
`crates/cosmon-surface/src/render.rs` and
`crates/cosmon-surface/src/github.rs`.

During the `delib-20260409-f4e1` deliberation the panel asked the
natural follow-up: *should users be able to supply their own
templates?* The idea has obvious appeal — a team could drop a
`.cosmon/templates/issue.md.hbs` file and customise their GitHub
Issue body without touching cosmon source. It also has the equally
obvious danger of producing a configuration surface larger than the
thing being configured.

Two competing pressures:

1. **Yes, make it pluggable.** Rendering is mechanical. Templates are
   a well-understood extension mechanism (Handlebars, Tera, MiniJinja
   all exist). Users have idiosyncratic ticket formats their
   organisations demand, and hard-coding three modes forces them to
   either fork cosmon or abandon the projection.
2. **No, resist the urge.** Every new template engine is a new
   transitive dependency, a new learning curve, a new class of
   runtime errors ("template rendering failed at line 42"), and a
   new maintenance surface. The three modes in ADR-017 cover the
   two real audiences (shared repos / cosmon-owned). Adding a
   fourth knob without evidence it is needed is premature
   generality — the Cosmon thesis explicitly warns against this
   (see founding-thesis Part I, Minimum Action principle).

The panel landed on a deferral, not a rejection. Templates may be
the right answer *eventually*, but only once we have evidence that
the current three modes are genuinely insufficient. Evidence means
concrete requests from real users with real surfaces, not
hypothetical *"wouldn't it be nice if…"* speculation.

## Decision

### Blocking condition

This ADR moves from **Proposed/Blocked** to **Accepted** only when
**three concrete template requests** have been logged. A "concrete
template request" is defined as:

1. **A named requestor** — a specific operator, repo, or downstream
   project. *"Someone might want…"* does not count.
2. **A worked example of the desired output** — the actual markdown
   (or JSON, or whatever) the requestor wants rendered, not an
   abstract description of the feature.
3. **An explanation of why `attributed`, `host-native`, and `none`
   all fail** — the requestor must show that none of the existing
   three modes produces an acceptable result, not merely that a
   custom template would be *nicer*.

Requests are logged as 💡 idea molecules with the tag
`surface-template-request` so they are greppable. When three
independent requests exist that clear the bar, this ADR is
re-opened, the proposal below is refined against the actual
requests, and the ADR is either accepted, amended, or explicitly
rejected with the evidence in hand.

### Why this gate

Three is the smallest number that distinguishes *"one person has
an unusual need"* from *"a pattern exists"*. Two could be a
coincidence. Three is a trend. The gate exists because the cost of
shipping a template engine is near-permanent (we will never remove
it once operators rely on it) while the cost of *not* shipping it
is a small number of operators choosing a different workflow, which
is recoverable.

### Tentative proposal (non-binding, for reference only)

If and when the ADR unblocks, the baseline design we would start
from is:

- A new `SurfaceKind::Template { engine, path }` variant, where
  `engine` names a supported template language and `path` points at
  a template file relative to the project root.
- Template context is the same `MoleculeData` / `Fleet` /
  `FormulaMap` already passed to the existing renderers — no new
  data plumbing.
- Only **one** engine is bundled initially. MiniJinja is the
  current frontrunner because it is small (single crate, no
  C deps), sandboxed by default, and already familiar to operators
  from other tooling. This is a tentative preference, not a
  decision.
- The backref invariant from ADR-017 still applies. Any template
  must emit the cosmon marker. The renderer will reject templates
  that fail to emit a valid marker at render time (verified by
  regex on the output).
- Host-native vs attributed becomes a property the template
  *inherits* via context variables (`branding = "host-native"`),
  not a template-selection axis. Templates can branch on it or
  ignore it.

Every one of these points is provisional and will almost certainly
change once real requests force the design to confront real
output shapes.

## Consequences

**Positive (of the deferral):**

- Zero added dependencies today. The rendering code stays
  auditable as pure Rust functions.
- Operators who genuinely need custom templates can still fork the
  rendering module; the fork is small enough to maintain
  out-of-tree. This is a legitimate escape hatch.
- The three-request gate forces the design to be grounded in real
  output examples before any code is written.

**Negative (of the deferral):**

- Operators with a single unusual need are told "file an idea
  molecule and wait". This is real friction, even if the
  alternative is worse.
- The deferral is load-bearing for the branding work in ADR-017 —
  if someone ignores the gate and ships templates anyway, the
  branding enum becomes subsumed by template selection and the
  whole model loses its simplicity.

**Neutral:**

- No code changes result from this ADR in its current state. It
  exists to mark the boundary and capture the deferral rationale
  so the next person who proposes templates finds the history.

## References

- [ADR-017: Host-Native Projection and Surface Rendering Invariants](017-host-native-projection.md)
  — the immediate parent. This ADR exists to answer *"okay but
  what about templates?"*, which ADR-017 explicitly punts on.
- `delib-20260409-f4e1` — deliberation where the deferral was
  proposed. Synthesis #7 includes the "three concrete requests"
  gate.
- `founding-thesis.md` Part I, **Minimum Action** — the principle
  that "do nothing" is a valid, often optimal, engineering choice.
- `crates/cosmon-surface/src/config.rs` — the `SurfaceKind` and
  `Branding` enums where `Template` would eventually land.
