# `docs/specs/` — Mechanical specifications

This directory holds the formal TLA+ specifications that mechanically
verify the invariants stated in cosmon's ADRs.

## Files

| File                                | Role                                                                |
|-------------------------------------|---------------------------------------------------------------------|
| `CosmonRun.tla`                     | TLA+ module — fleet/molecule/event-log state machine for ADR-052 (I1..I10). |
| `CosmonRunXGalaxy.tla`              | Cross-galaxy extension (I11..I15) from delib-20260419-29f9. EXTENDS CosmonRun. I14 is the cross-galaxy Gödel sentence (parallel to I9). |
| `CosmonRunShare.tla`                | Cross-galaxy sharing discipline (I16..I19). EXTENDS CosmonRun + CosmonRunXGalaxy. Share atomic-with-Detect load-bearing note. No .cfg yet. |
| `CosmonRun_GovernanceGate.tla`      | Mycelial-gate composition layer (delib-20260516-5d97 #4). EXTENDS CosmonRun. Adds `CompleteGoverned(m)` with guard `IsGovernanceRelevant(m) => MycelialAdmits(m)` ; new invariant `GovernanceGateRespected`. INSTANCE edges to noogram `MycelialGate.tla` / `AttestorGraph.tla` declared as documentation (not activated at SANY time ; federation co-simulation is performed by `cs spec-audit --spec mycelial-gate` against the noogram NDJSON ledger). |
| `CosmonRun_GovernanceGate.cfg`      | Closed-environment fixture for the governance gate. 2 molecules (one relevant, one ordinary), federation oracle admits the relevant one. ~1k distinct states. |
| `CosmonRun_InBand.cfg`              | Closed-environment model — every safety + liveness property holds.  |
| `CosmonRun_OutOfBand.cfg`           | `BypassMerge` enabled — exhibits I9 as the Gödel counterexample.    |
| `CosmonRun_Crashes.cfg`             | `TmuxCrash` / `ProcessCrash` enabled — checks I6,I7,I9 + L2,L3.     |
| `CosmonRun_CrashesI3.cfg`           | Same as Crashes but checks I3 explicitly (fails — documents the frontier). |
| `CosmonRun_CrashesI4.cfg`           | Same as Crashes but checks I4 explicitly (fails — documents the frontier). |
| `CosmonRun_I9Counterexample.cfg`    | Minimal one-molecule witness for the I9 violation.                  |
| `CosmonRunXGalaxy_InBand.cfg`       | Closed cross-galaxy model — all five I11..I15 hold.                 |
| `CosmonRunXGalaxy_Adversarial.cfg`  | `ForgePeerReceipt` enabled — exhibits I14 as the cross-galaxy Gödel counterexample. |
| `CosmonDocHarness.tla`              | TLA+ meta-fleet for DOC-HARNESS mission (delib-20260519-a20b, B.4). Invariants I1 NoOrphanDoc, I2 DemoGateBeforeDoc, I3 RegistryTruth (with KebabRenameBait exclusion), I4 LyapunovDecreasing, TatouageShape. |
| `CosmonDocHarness.cfg`              | Tight model — liveness + safety. MaxIter=6, MaxArtifacts=10. |
| `CosmonDocHarness_Safety.cfg`       | Widened model — safety only, SYMMETRY on. MaxIter=12, MaxArtifacts=20. |
| `CosmonDocHarness.tlc-green`        | Sentinel sealed by `just tlc` (BLAKE3(.tla) + UTC timestamp) — downstream consumers (`cs config adapters`, `man cs LOOPS / ADAPTERS`) gate on its presence. |
| `tla2tools.jar`                     | TLA+ tooling (TLC model checker). v1.8.0 from tlaplus/tlaplus.      |
| `tlc-out-*.log`                     | Captured TLC outputs for each run (audit trail).                    |
| `VALIDATION-REPORT.md`              | Result interpretation + I9/I14 traces + frontier discussion.        |

## Installing TLC

`tla2tools.jar` is the single self-contained jar that ships TLC, the
SANY parser, and the pretty-printers. v1.8.0 is checked in here so the
validation is reproducible without network access.

To upgrade or refresh:

```bash
curl -sSL -o tla2tools.jar \
    https://github.com/tlaplus/tlaplus/releases/download/v1.8.0/tla2tools.jar
```

Java 11+ is required. macOS Homebrew users can install it with:

```bash
brew install openjdk@21
```

Then point `JAVA` at the binary explicitly (Homebrew installs it
keg-only on Apple Silicon):

```bash
JAVA=/opt/homebrew/opt/openjdk@21/bin/java
```

## Running the model checker

From this directory:

```bash
$JAVA -cp tla2tools.jar tlc2.TLC \
    -workers auto \
    -config CosmonRun_InBand.cfg \
    CosmonRun.tla
```

Substitute the `.cfg` to switch model. Each run writes its
output to stdout; capture into `tlc-out-<scenario>.log` if you need
the audit trail.

The largest model (`CosmonRun_Crashes.cfg`, 3 molecules, MaxSeqno=2)
explores ~300k distinct states in ~36 s on an Apple-Silicon laptop.
The minimal counterexample (`CosmonRun_I9Counterexample.cfg`) finishes
in well under a second. The cross-galaxy InBand model
(`CosmonRunXGalaxy_InBand.cfg`, 1 molecule × 2 galaxies, MaxSeqno=1)
explores ~1.2k distinct states in under a second; its Adversarial
counterpart produces the I14 counterexample in 2 steps.

## Why this exists

The ADR-052 invariants were stated in prose in the deliberation
`delib-20260419-d34b` synthesis. Prose can lie. TLC cannot — it either
exhibits a state where the invariant holds across the entire reachable
graph, or it produces a concrete counterexample trace.

`VALIDATION-REPORT.md` summarises what TLC found, including the I9
counterexample that mechanically confirms ADR-052's classification of
I9 as **out-of-band** — an invariant that holds only when the
environment is closed, exactly the structure of a Gödel sentence.

## Continuous verification (CI gate)

The CI gate runs TLC on every push or pull request that touches
`docs/specs/*.tla`, `docs/specs/*.cfg`, or `docs/specs/tla2tools.jar`.
The workflow is `.github/workflows/tla-verify.yml`; it spins up one
ubuntu-latest job per `.cfg`, in a matrix with `fail-fast: false` so
every verdict is visible even when one fails.

Each config carries an **expected verdict** — the manual procedure
above is mechanised as a contract:

| Config                              | Expected   | Violated invariant                   |
|-------------------------------------|------------|--------------------------------------|
| `CosmonRun_InBand.cfg`              | pass       | —                                    |
| `CosmonRun_Crashes.cfg`             | pass       | —                                    |
| `CosmonRun_OutOfBand.cfg`           | **fail**   | `I9_BranchMergedOnlyIfCompleted`     |
| `CosmonRun_CrashesI3.cfg`           | **fail**   | `I3_FleetMirrorsSession`             |
| `CosmonRun_CrashesI4.cfg`           | **fail**   | `I4_SessionImpliesLiveProcess`       |
| `CosmonRun_I9Counterexample.cfg`    | **fail**   | `I9_BranchMergedOnlyIfCompleted`     |
| `CosmonRunXGalaxy_InBand.cfg`       | pass       | —                                    |
| `CosmonRunXGalaxy_Adversarial.cfg`  | **fail**   | `I14_PeerCompletionHonest`           |

The gate fails in both directions:

- If a **pass** config starts reporting `Invariant … is violated.`,
  a previously proved property has broken and the change is rejected.
- If a **fail** config stops producing its expected counterexample
  (TLC emits `Model checking completed. No error has been found.`),
  the bypass action or frontier semantics was silently removed. The
  I9 counterexample is the **test-that-must-fail** — part of the
  Gödel-sentence contract. If I9 ever turns green, the closed-
  environment hypothesis is no longer honest and the gate refuses the
  change.

Pass detection grep: `Model checking completed. No error has been found.`
Fail detection grep: `Invariant <name> is violated.`

The vendored `tla2tools.jar` is checked into git, so the gate needs no
network access beyond cloning the repo and installing Java (Temurin 17
via `setup-java@v4`). TLC output for every config is uploaded as a job
artifact (`tlc-<config>`, 14-day retention) for after-the-fact audit.
