# Upstream-contract gating — capability typestate at the galaxy boundary

**Status:** playbook (prose, non-normative)
**Origin:** showroom `idea-20260508-aad4`, migrated cosmon-ward as
`task-20260625-28e4` (CLAUDE.md feedback flow — *le réacteur apprend de ce
qu'il brûle*). Triggering case: showroom modules gated by sibling-galaxy
contracts not yet frozen (e.g. M-v *"Play it"*, `delib-bfdb §IS-4`).
**Scope:** any cosmon-managed galaxy that consumes a contract owned by a
**sibling** galaxy while that contract is still in flight. Not a crate, not a
trait, not a feature flag. A *shape*, ported per galaxy — same discipline as
the *session-primitive* shape (shared algebra, not shared
runtime).

---

## Thèse

When galaxy **A** must call into a contract owned by sibling galaxy **B**, and
that contract is **not yet frozen**, the consuming code in A faces a real but
*temporary* hazard: the upstream shape can still change under it. The naive
guard is a feature flag inside A — `if play_it_ready { … }` — a runtime boolean
that scatters across the call sites and can be flipped to `true` by anyone,
for any reason, with no proof that B actually froze anything.

The pattern says: **lift "is the upstream contract frozen?" out of A's runtime
and into A's type system as a capability typestate at the boundary.** A
function that needs the not-yet-frozen capability does not take a `bool`; it
takes a witness — `Frozen<PlayIt>` — that can only be minted by validating the
*cited* upstream contract version against a pinned baseline. No witness, no
call. The check happens **once**, at the membrane between the two galaxies, and
the compiler propagates the consequence everywhere the capability is touched.

The boundary is between **galaxies**, not inside one. This is the whole point:
a feature flag answers *"is A ready to use this?"*; a capability typestate
answers *"has B frozen what A depends on?"* — and pins the answer to a fact
that lives in B, surfaced in A by citation.

---

## 1. The problem, concretely

Showroom's *"Play it"* module consumes a contract owned by a sibling galaxy.
At authoring time that contract is in flight — `delib-bfdb` has not closed
`§IS-4`. Showroom still needs to write the module so the rest of the setlist
compiles. Two bad answers and one good one:

### Failure mode 1 — the scattered feature flag

```rust
// showroom, anti-pattern
if config.play_it_ready {
    play_it::render(track)?;        // call site 1
}
// …200 lines later…
if config.play_it_ready {           // call site 2 — same flag, copied
    play_it::seek(track, pos)?;
}
```

The flag is a runtime boolean owned by **A**. Three pathologies:

1. **It scatters.** Every call site re-asks the same question; one forgotten
   `if` and an unfrozen contract is called in production.
2. **It is unproven.** `play_it_ready = true` is a config edit. Nothing ties it
   to B actually having frozen the contract — it asserts readiness, it does not
   *witness* it.
3. **It is the wrong question.** `play_it_ready` reads as *"is showroom
   ready"*. The real question is *"did the sibling galaxy freeze `§IS-4`"* —
   a fact that does not live in showroom at all.

### Failure mode 2 — silent stub

A's worker, told only *"make the setlist compile"*, stubs `play_it` to a no-op
and moves on. Now the gate is invisible: nothing records that a capability is
withheld pending an upstream freeze, and the next worker re-discovers the hole
from scratch. (This is the generic *brief drops what it does not enumerate*
failure — see cosmon CLAUDE.md, the bootstrap-brief hole.)

### The good answer — capability typestate at the boundary

Make the not-yet-frozen capability *uncallable* without a witness, and make the
witness mintable only by validating the cited upstream version:

```rust
// showroom, the pattern
pub struct PlayIt;                  // the capability marker (B's contract)

/// Proof that the sibling-galaxy contract behind `C` is frozen at a
/// version showroom has pinned. Cannot be constructed except through
/// `Frozen::witness`, which validates the *cited* upstream version.
pub struct Frozen<C> {
    contract_version: ContractVersion,
    _capability: PhantomData<C>,
}

impl Frozen<PlayIt> {
    /// The single mint. `cited` is the version showroom read from the
    /// sibling galaxy's frozen contract (by citation — see §3). Returns
    /// `None` while the contract is still in flight.
    pub fn witness(cited: ContractVersion) -> Option<Self> {
        (cited >= PLAY_IT_PINNED_BASELINE).then_some(Frozen {
            contract_version: cited,
            _capability: PhantomData,
        })
    }
}

// The capability-requiring API takes the witness, never a bool:
impl PlayItModule {
    pub fn render(&self, track: &Track, _frozen: &Frozen<PlayIt>) -> Result<()> {
        // unreachable unless a Frozen<PlayIt> was minted upstream
    }
}
```

Now `render` and `seek` and every other call site take `&Frozen<PlayIt>`.
There is exactly **one** place that can produce it (`Frozen::witness`), exactly
**one** check (`cited >= PLAY_IT_PINNED_BASELINE`), and the compiler refuses
every call site that has not threaded the witness through. *"I called an
unfrozen contract"* is no longer a runtime regression — it is a compile error,
in the same family as `Molecule<Pending>` having no `.evolve()`
(`cosmon-core/src/molecule.rs`) and `Molecule<Completed>` reaching `Merged`
only by presenting `MergeEvidence`.

---

## 2. Why typestate beats the flag — point by point

| | Feature flag (`bool`) | Capability typestate (`Frozen<C>`) |
|---|---|---|
| Where the check lives | every call site | one mint, at the boundary |
| What it proves | nothing — an assertion | the cited upstream version ≥ pinned baseline |
| Forgotten guard | silent prod call on unfrozen contract | compile error (no witness in scope) |
| Question it answers | *is A ready?* | *did B freeze the contract A cites?* |
| Removal when frozen | hunt every `if`, hope you got them all | delete the parameter; compiler lists every site |
| Survives compaction | lives in a config file + tribal memory | lives in the type signature, self-documenting |

The last row is the sleeper benefit. When B finally freezes the contract, the
gate is retired by **deleting the `Frozen<C>` parameter** — and the compiler
hands you the exhaustive list of call sites to clean up. A feature flag leaves
you grepping and guessing; a typestate makes the cleanup mechanical and total.

---

## 3. The cross-galaxy nuance — the witness is local, the fact is cited

Cosmon galaxies align **by citation, not by shared infrastructure** (the
syzygie protocol — [`../guides/syzygie.md`](../guides/syzygie.md)). They do
**not** share a crate, so `Frozen<PlayIt>` is **not** a type imported from the
sibling galaxy. That would couple the two repos and break the syzygie rule.

The split is precise:

- **The fact lives in B.** *"`§IS-4` is frozen at version X"* is a statement B
  makes, in B's chronicle / ADR, when B closes its deliberation. B owns it.
- **The witness lives in A.** `Frozen<PlayIt>` is A's local type. `PLAY_IT_
  PINNED_BASELINE` is the version A has read from B's frozen contract and
  pinned into A's source — *by citation*. A's `Frozen::witness` validates the
  cited version it was handed against that pin.

So the typestate is the *shape*, instantiated once per consuming galaxy — the
same way the *session-primitive* shape is one algebra with a
per-galaxy instantiation each. What travels between galaxies is the
contract **version** (a fact, surfaced by a syzygie citation: `inherit` /
`adapt(diff)` / `refuse`), never a Rust type. Silence on the upstream side is a
bug the `chronicle-lint` patrol already catches; this pattern is what the
*downstream* side does with the answer once it lands.

A useful test: if you find yourself wanting to `use sibling_galaxy::Contract`,
stop — you are reaching for shared infrastructure where the protocol asks for a
cited fact. The witness is yours; only the version number crosses the boundary.

---

## 4. Lifecycle of a gate

```
B's contract in flight ─────────────────────────────────────────────►  frozen
        │                                                                  │
        │  A pins PLAY_IT_PINNED_BASELINE (cited, current best guess)      │
        ▼                                                                  ▼
A: Frozen::witness(cited) → None        A: Frozen::witness(cited) → Some(_)
   every capability call uncallable        capability calls compile
        │                                                                  │
        └── gate visible in the type signature ───────────────────────────┘
                                                                           │
                                              B freezes → A deletes the
                                              Frozen<C> parameter; compiler
                                              enumerates every call site to
                                              un-thread. Gate retired, total.
```

The gate is **born visible** (the parameter is in the signature from day one),
**stays honest** (no witness while `cited < baseline`), and **dies mechanical**
(delete the parameter, follow the compiler). No grep, no tribal memory, no
silent stub.

---

## 5. When to use it — and when not

**Use it when** all three hold:

1. A consumes a capability whose **contract is owned by a sibling galaxy**.
2. That contract is **not yet frozen** (in flight in B's deliberation/ADR).
3. A must still **build now** — the rest of A's work cannot wait for B.

**Do not use it when:**

- The contract is **internal to A**. Then it is just typestate; the
  "boundary" framing adds nothing — use a plain sealed marker.
- The contract is **already frozen**. No gate is needed; call it directly and
  pin the version in the normal dependency way.
- The thing you are gating is a genuine **runtime toggle** (operator turns a
  feature on/off at will). That *is* a feature flag — and that is fine. The
  pattern is for *"blocked until an upstream freeze"*, not *"switchable by
  preference"*. The distinguishing question: *can the answer flip back to
  "no" after it was "yes"?* A frozen contract does not un-freeze; a feature
  toggle does. Permanent ⇒ typestate; reversible ⇒ flag.

The discriminator is whether the gate encodes a **one-way upstream freeze
event** or a **reversible local preference**. Typestate is for the former; it
is exactly wrong for the latter.

---

## 6. Relation to the rest of cosmon

- **Syzygie** ([`../guides/syzygie.md`](../guides/syzygie.md)) — supplies the
  channel by which the *contract version* (the fact) crosses from B to A. This
  pattern is the downstream consumer of a syzygie citation.
- **Session-primitive** — the sibling pattern in form: one algebra/shape,
  instantiated per galaxy, never a shared crate. Same "shape not runtime"
  discipline; its playbook is maintained outside this tree.
- **Molecule typestate** (`cosmon-core/src/molecule.rs`) — the in-tree proof
  that cosmon already lifts lifecycle facts into the type system
  (`Molecule<Pending>` has no `.evolve()`; `Completed → Merged` requires
  `MergeEvidence`). `Frozen<C>` is the same idiom (`sealed` marker +
  `PhantomData` + witness-by-construction) applied at the galaxy membrane
  instead of the molecule lifecycle.
- **Cosmon-ward feedback flow** (CLAUDE.md) — this doc exists *because*
  showroom surfaced a recurring shape back to cosmon as a typed molecule
  rather than silently patching one module. The pattern is the named successor
  to that observation.
