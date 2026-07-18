# Q6 — Realized-model pre-mortem (Kahneman)

**The obituary.** Six months on, `cs peek` shows a realized-model column that
operators trust *more* than the intention column — it reads as "ground truth,
what actually ran." It is lying. It lies because the realized slot was built by
copying the intention fold (last-wins over an `Option<String>`), and a fold
optimized for a *resolved pin* silently fabricates a *record of execution*. The
honesty rule in `adapter_attribution.rs` guards intention. It was never ported
to realization. Three concrete deaths follow.

The realized slot must fold from a **disjoint** event source (`ModelObserved`
only) and never read `ModelSelected`. That single structural fact is what stops
all three fabrications; everything below is a corollary.

---

## (a) Silent adapter — ran, never reported its model

- **Rendered realized cell:** `-`  (honest floor: ran, did not report).
- **Test:** `assert_eq!(RealizedModel::fold(&[completed_no_observe]).cell(), "-");`
  paired with `assert_ne!(r.cell(), attribution.model.as_deref().unwrap_or("-"));`
  — realized must never *equal the intention id by construction*.
- **Fabrication risk:** inferring **realized == intention**. System 1's WYSIATI:
  the pin is *present* in the fold's field-of-view, so it fills the empty
  realized slot with the nearest plausible string. The operator cannot detect
  the substitution — it looks exactly like an honest observation.
- **Guard:** realization folds *only* `ModelObserved`. The intention id is not in
  scope inside the realized fold — you cannot copy a value you cannot see. This
  is the same "never infer from current config" discipline, moved one axis over:
  never infer realization from intention.
- **What the code must NEVER do here:** never `out.observed = self.model.clone()`
  as a fallback; never echo the pin.

## (b) Mid-session change — X, then Y (fallback / rate-limit / retune)

- **Rendered realized cell:** `gpt-4o→gpt-4o-mini`  (distinct observations,
  observation order preserved). One distinct value renders bare: `gpt-4o`.
- **Test:** `assert_eq!(RealizedModel::fold(&[observed("gpt-4o"), observed("gpt-4o-mini")]).cell(), "gpt-4o→gpt-4o-mini");`
- **Which wins:** *neither.* Both ran. "What actually ran" is a set-over-time, not
  a pin. Last-wins answers "what ran *last*"; first-wins answers "what ran
  *first*"; the promise was "what ran." Only the multi-valued answer keeps it.
- **Fabrication risk:** overwrite-collapse. Reusing the intention fold's
  last-wins (`out.model.clone_from(model)`) fabricates a single-model session
  that never occurred — the operator debugging a cost or quality anomaly on X
  sees only Y and concludes X was never involved.
- **Guard:** realized fold **accumulates distinct-in-order** (`push_if_new`),
  it does not overwrite. Note the asymmetry from `fold`'s existing
  `most_recent_selection_wins`: that is correct for intention and *wrong* here.
  The two folds must not share a code path.

## (c) Worker dead before any event — cold start / crash

- **Rendered realized cell:** `?`  (unknown: we cannot even claim it ran).
- **Test:** `assert_eq!(RealizedModel::fold(&[spawned_only]).cell(), "?");`
  and to prove the distinction from (a):
  `assert_eq!(RealizedModel::fold(&[spawned, completed]).cell(), "-");`
- **Distinguished from silent adapter:** yes, and the distinction is load-bearing.
  Silent = *ran to a terminal/completion event, said nothing* → `-` (the floor
  asserts "adapter default applied"). Dead = *no completion event at all* → `?`
  (we may not assert anything ran). The discriminant is the presence of a
  terminal/completion event, not the absence of `ModelObserved` (both have zero).
- **Fabrication risk:** rendering `-` for a crashed worker. In this module `-`
  is a *positive* claim ("the adapter's own default applied"). Applied to a
  process that died before dispatch, that claim is a fabricated completion —
  the worst lie of the three, because it invents an execution.
- **Guard:** the `-` floor is **gated on a terminal/completion event**. Absent
  one, the fold yields `Unknown` → `?`. Keep `-` and `?` as distinct glyphs;
  collapsing them re-imports the (a)/(c) conflation.

---

## Cross-check: does a two-slot struct survive?

`observed_model: Option<String>` beside `model: Option<String>`:

- **(a) survives** *iff* `observed_model` folds from the disjoint `ModelObserved`
  source. `None` beside `Some(pin)` renders `-`. The slot separation is
  necessary but not sufficient — a shared fold still fabricates.
- **(b) BREAKS.** `Option<String>` holds one value; `X→Y` is unrepresentable.
  Any single-Option realized slot is forced into overwrite-collapse. The slot
  must widen to an ordered distinct sequence (`Vec<String>` / small ordered set).
- **(c) BREAKS.** `None` conflates "silent" and "dead" — the same absence, two
  incompatible truths. `Option` cannot carry the run-liveness discriminant.

**Verdict:** the two-slot struct is a trap that passes exactly the one case you
test first. It needs a tri-state, not an `Option`:

```rust
enum Realized {
    Unknown,               // no completion event → "?"
    Silent,                // completed, no ModelObserved → "-"
    Observed(Vec<String>), // distinct, in observation order → "X" | "X→Y"
}
```

**The forced System 2 step** is one test: `assert_ne!(realized.cell(),
intention.model_id())` on the silent case. It is cheap, it costs real attention,
and it is the single assertion whose failure means the feature has started to
lie.
