# ADR-065 — MSRV bump 1.82 → 1.88

**Status:** Accepted (2026-04-22)
**Scope:** `workspace.package.rust-version` in root `Cargo.toml`, the
narrative pin in `rust-toolchain.toml`, the `MSRV:` line in
`CLAUDE.md` §"Rust Rules (enforced by CI)".

**Parent deliberation:** `delib-20260422-c4a6`
— matrix-echo-tick v0 panel (forgemaster, tolnay, turing, …).
Per-persona responses under
`responses/`.

**Binds:**
- **Unblocks** ADR-064 (matrix-echo-tick isolation) and the
  `task-20260422-7fd6` matrix-echo-tick v0 crate, which both depend on
  `matrix-sdk = "0.16"` — the upstream that forces this bump.
- **Refines nothing.** This is a pure tooling-floor decision; no
  vocabulary, no invariant, no protocol changes.

---

## 1 · Context

### 1.1 · What forced the question

The parent deliberation `delib-20260422-c4a6` selected `matrix-sdk 0.16`
as the substrate for matrix-echo-tick v0 (the experimental Matrix
transport for pilot ↔ worker whisper, per ADR-038). matrix-sdk 0.16's
upstream `Cargo.toml` declares:

```
rust-version = "1.88"
edition = "2024"
```

Cosmon's workspace currently declares `rust-version = "1.82"`
(Cargo.toml:57), so `cargo add matrix-sdk@0.16` would fail the
`cargo check --workspace` gate with `package … cannot be built because
it requires rustc 1.88 or newer, while the currently active rustc
version is 1.82`.

### 1.2 · Why this gets its own ADR

MSRV is a **semver promise to downstream consumers** (Tolnay §"Semver
discipline"). Backdooring an MSRV bump through the matrix-echo-tick PR
would:

1. Couple an experimental transport landing to a workspace-wide floor
   change — two reversibility profiles in one commit.
2. Hide the decision from `docs/adr/` — the first place a future
   maintainer looks when asking "why did the floor move on 2026-04-22?"
3. Violate the cosmon Architectural Discipline §"stateless, composable,
   reversible" — a bump is not reversible if it is entangled with
   feature work.

The ADR cost is one page. The cost of implicit MSRV policy is a future
drift incident.

### 1.3 · What 1.88 buys us now

The bump is instrumentally motivated (matrix-sdk 0.16) but the
underlying capabilities are not specific to Matrix. 1.88 ships:

- **edition 2024** stabilized — `let … else` and expression-statement
  rules matter for worker codegen in `foundry-probe`.
- `async fn` in traits fully stabilized without the `#[allow]` dance —
  relevant for `ComputeReservoir` (ADR-062 §6.2) when it lands.
- `if let` chains stabilized — readability win in `cosmon-core` state
  machines.

These are carried as *benefits*, not as justifications. The
justification is matrix-sdk 0.16.

---

## 2 · Decision

Bump `workspace.package.rust-version` from `"1.82"` to `"1.88"`.
Update the narrative pins in `rust-toolchain.toml` and `CLAUDE.md` to
match.

`rust-toolchain.toml` keeps `channel = "stable"` — the MSRV is a floor,
not a pin. Day-to-day builds continue to float with stable (currently
1.94.1). CI uses `dtolnay/rust-toolchain@stable`, which always satisfies
`≥1.88`, so no workflow edits are required.

### 2.1 · Files changed in the landing PR

| File | Change |
|---|---|
| `Cargo.toml` (workspace root, line 57) | `rust-version = "1.88"` |
| `rust-toolchain.toml` (comment header) | note the bump + ADR-065 link |
| `CLAUDE.md` §"Rust Rules (enforced by CI)" | `MSRV: rust-version = "1.88"` |
| `docs/adr/065-msrv-bump-1-88.md` | this file |

No crate-level `rust-version` overrides exist today (all crates inherit
from workspace), so the single workspace edit propagates everywhere.

### 2.2 · Companion: `is_multiple_of` migration

Running `cargo clippy --workspace -- -D warnings` on current stable
(rustc 1.94.1) emits `clippy::manual_is_multiple_of` errors at seven
sites across the workspace — the lint was introduced in rustc 1.94 and
suggests the stabilized (since rustc 1.87) `u*::is_multiple_of()` API.
The suggestion is only actionable at MSRV ≥ 1.87, which is why the
clippy errors accumulated silently until this bump. The landing PR
applies the mechanical fix:

```
count % 3 == 0     →  count.is_multiple_of(3)
s.len() % 2 != 0   →  !s.len().is_multiple_of(2)
```

Sites touched:

| Crate | File:line |
|---|---|
| `cosmon-core` | `creativity.rs:515` |
| `cosmon-cli` | `cmd/notarize.rs:235` |
| `cosmon-notary` | `signature.rs:160` |
| `cosmon-runtime` | `lib.rs:890` |
| `schedulerd` | `config.rs:191,193,195` |
| `mailroom-voice-gridco` | `lib.rs:146` |
| `mailroom-voice-tts` | `lib.rs:200` |

Pure rewrite, no behavior change. Carried in the MSRV-bump PR because
the two are entangled: without 1.88 (≥1.87 in fact) the fix would not
compile; without the fix, clippy blocks the DoD. The ADR declares the
intent — a future clippy lint added upstream that requires a *newer*
MSRV to fix will be handled the same way (fix + note, one commit).

---

## 3 · Consequences

### 3.1 · For cosmon developers

None visible. The minimum toolchain for `cargo check --workspace` is now
1.88, but rustup users on `stable` are already on 1.94.1. A fresh clone
+ `rustup show` picks up `channel = "stable"` from
`rust-toolchain.toml` and installs ≥1.88 automatically.

### 3.2 · For cosmon downstream consumers

Any project that embeds cosmon crates via git or path dependencies now
needs rustc ≥1.88. Cosmon has **no published crates.io releases yet**
(ADR-043 Provider Abstraction §"publication deferred until v0.2"), so
the consumer surface is limited to:

- The `noogram` agent runtime (same monorepo, same toolchain).
- The `mailroom` and `showroom` galaxies (syzygie members, ADR-047)
  — both already build against stable rustc, verified on 2026-04-22.
- The `gitagent` / `gastown` experimental consumers — operator's own
  machines, already 1.94.1.

Public-crate consumers become a concern at the v0.2 crates.io cut; this
ADR is cited there.

### 3.3 · Reversibility

**High.** The bump is a single-number edit in three files. Rolling back
to 1.82 is a one-commit revert, conditional only on the matrix-echo-tick
feature branch not having landed *with* matrix-sdk 0.16 as an accepted
dependency. If matrix-echo-tick is subsequently retired (ADR-064 has an
explicit sunset clause), the MSRV bump may remain — edition 2024 and
the 1.88 stabilizations are intrinsically valuable.

### 3.4 · Risk surface

| Risk | Likelihood | Mitigation |
|---|---|---|
| CI fails on 1.88 (deprecated lint, edition-2024 syntax drift) | Low | `cargo check/test/clippy/fmt` run locally before merge; CI uses `@stable` which is well past 1.88 |
| Ecosystem crate below 1.88 breaks | Very low | `cargo tree` on cosmon-core + cosmon-cli shows no dependency below rustc 1.80 as of 2026-04-22 |
| Operator CI runner on pinned-old rust | N/A | No operator CI runner pins rustc; all use `dtolnay/rust-toolchain@stable` |

### 3.5 · What this ADR does NOT do

- Does **not** add `matrix-sdk` to any crate's `Cargo.toml` — that
  happens in `task-20260422-7fd6` (matrix-echo-tick v0).
- Does **not** change `edition = "2021"` in any crate — only the
  *floor* supports edition 2024 now; migration is a separate question
  (future ADR if and when we want to bump).
- Does **not** touch `cargo deny` config or the `supply-chain/`
  attestations.
- Does **not** publish anything to crates.io.

---

## 4 · Verification

The four standard gates were run on the feature branch before merge:

```
cargo check --workspace      # PASS
cargo test --workspace       # PASS
cargo clippy --workspace -- -D warnings  # PASS
cargo fmt --all -- --check   # PASS
```

Local rustc version: `rustc 1.94.1 (e408947bf 2026-03-25)` — well above
the new floor.

---

## 5 · References

- Parent synthesis §"MSRV: 1.85 or 1.88?" (D2 resolution):
  `.cosmon/state/fleets/default/molecules/delib-20260422-c4a6/synthesis.md`
- Tolnay's response §"Semver discipline":
  `.cosmon/state/fleets/default/molecules/delib-20260422-c4a6/responses/tolnay.md`
- Forgemaster's response §"MSRV conflict":
  `.cosmon/state/fleets/default/molecules/delib-20260422-c4a6/responses/forgemaster.md`
- matrix-sdk 0.16 Cargo.toml (upstream proof of 1.88 requirement):
  [github.com/matrix-org/matrix-rust-sdk](https://github.com/matrix-org/matrix-rust-sdk/blob/main/crates/matrix-sdk/Cargo.toml)
- [ADR-043](043-provider-abstraction.md) — publication policy (why the
  downstream blast radius is currently bounded).
- [ADR-047](047-event-log-protocol-v0.md) — syzygie alignment (why the
  sibling galaxies' toolchain state mattered here).
