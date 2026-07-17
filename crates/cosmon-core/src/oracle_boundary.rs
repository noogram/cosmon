// SPDX-License-Identifier: AGPL-3.0-only

//! Oracle-boundary **hardening** for a weak, high-variance local oracle
//! (C4 of `delib-20260705-7288`, ADR-075 provisioned to its real duty cycle).
//!
//! # Why this module exists
//!
//! [ADR-075](../../../docs/adr/075-oracle-boundary-cs-tackle.md) inscribed a
//! typed envelope around `cs tackle`: bounded input, finite output set, a
//! *deterministic backstop*, and a Rice-corollary refusal of oracle
//! self-attestation. The envelope was written against a **strong** oracle
//! (Claude), where the backstop is cold insurance that almost never fires.
//!
//! Running the same envelope against a **weak local oracle** (a 120 B model on
//! `ollama-g5`) changes the duty cycle, not the design (delib D5c, turing Q11):
//!
//! - The backstop moves from cold insurance to a **hot path** — it fires
//!   20–30 % of the time, not <1 %.
//! - Retries stop being independent. A wide-variance oracle that failed once
//!   may fail *the same way* every time; blind retry is then a non-halting
//!   energy sink. Failures must be **classified systematic-vs-stochastic
//!   before** the next attempt.
//! - The backstop now runs **out-of-distribution against its own tests** — a
//!   wide-variance oracle statistically probes every latent verifier gap the
//!   strong model never hit.
//!
//! # The five mechanisms, in leverage order
//!
//! 1. **Grammar-constrained decoding** — makes a malformed structured output
//!    *impossible*, not merely unlikely. This half lives in the provider
//!    adapter (`cosmon_provider::CompletionRequest::format`) because it is an
//!    on-the-wire concern (ollama's native `format` / JSON-schema); it is *not*
//!    in this module. Named here so the leverage ordering reads in one place.
//! 2. **Structural completion gate** — [`CompletionGate`]. Completion is
//!    decided by **decidable DoD proxies** (`cargo check/test/clippy/fmt`,
//!    artefacts exist, diff non-empty) — *never* by asking the oracle whether
//!    it is done (ADR-075 (d) + Rice).
//! 3. **Bounded schema-repair** — [`RepairBudget`] + [`classify_failure`].
//!    A failed structured output is repaired at most `k ≤ 2` times, and only
//!    while the failure signature keeps *changing* (stochastic). A recurring
//!    identical signature is **systematic** and retrying it never halts.
//! 4. **Turn/energy halting budget** — [`HaltingBudget`] + [`BudgetLedger`].
//!    The external halting guarantee: the loop stops after a finite number of
//!    turns *and* a finite energy spend, independent of whether the oracle ever
//!    emits its own stop signal.
//! 5. **Tool-call cycle detection** — [`detect_tool_call_cycle`]. A weak oracle
//!    that loops (`read A`, `read B`, `read A`, `read B`, …) burns budget
//!    without progress; a repeated fingerprint window is caught before the
//!    budget runs dry.
//!
//! # Zero I/O
//!
//! Like [`crate::oracle_canary`] and [`crate::model_budget`], every decision
//! here is **pure**. The seam that runs `cargo check`, counts tokens, or
//! records tool calls is the worker-loop shell (`cosmon-agent-harness`, the
//! `cs tackle` worker); this module only *decides* from the observations that
//! shell produced. That is the executable spec of the hardened boundary,
//! unit-testable without a live oracle.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Mechanism 2 — structural completion gate (Rice-honouring DoD)
// ---------------------------------------------------------------------------

/// A **decidable** Definition-of-Done proxy — one gate whose pass/fail is
/// computed by a non-oracle mechanism the runtime can reproduce offline.
///
/// The set is deliberately closed and every member is a *decidable* predicate
/// (ADR-075 (d)): a compiler exit code, a file's existence on disk, a diff's
/// line count. There is intentionally **no** `OracleSaysDone` member — asking
/// the oracle to witness its own completion is a Rice-blocked semantic
/// predicate, and the whole point of the gate is to refuse it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DodProxy {
    /// `cargo check --workspace` (or the galaxy's configured build gate) exited 0.
    Build,
    /// `cargo test --workspace` exited 0.
    Test,
    /// `cargo clippy --workspace -- -D warnings` exited 0.
    Lint,
    /// `cargo fmt --all -- --check` exited 0.
    Format,
    /// The molecule's declared output artefacts all exist on disk.
    ArtifactsExist,
    /// The worktree diff against the branch point is non-empty (the worker
    /// actually wrote something — a zero-diff "done" is a silent no-op).
    DiffNonEmpty,
}

impl DodProxy {
    /// The canonical full set of proxies a `task-work` completion must satisfy.
    ///
    /// A gate evaluated against fewer than these is **inconclusive**, not
    /// complete: a missing proxy is treated as unmet (fail-closed), so an
    /// oracle cannot earn a completion by simply not reporting the gate it
    /// would have failed.
    pub const CANONICAL: [DodProxy; 6] = [
        DodProxy::Build,
        DodProxy::Test,
        DodProxy::Lint,
        DodProxy::Format,
        DodProxy::ArtifactsExist,
        DodProxy::DiffNonEmpty,
    ];
}

/// One observed gate result: the proxy and whether the non-oracle mechanism
/// that ran it reported success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateOutcome {
    /// Which decidable proxy was evaluated.
    pub proxy: DodProxy,
    /// `true` iff the mechanism (compiler, filesystem check, diff) reported
    /// success. The oracle does not get to set this bit.
    pub passed: bool,
}

impl GateOutcome {
    /// Convenience constructor.
    #[must_use]
    pub fn new(proxy: DodProxy, passed: bool) -> Self {
        Self { proxy, passed }
    }
}

/// The verdict of a [`CompletionGate`] evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateVerdict {
    /// Every required proxy was observed and passed. Completion is admissible.
    Complete,
    /// One or more required proxies were unmet (failed *or* never reported).
    /// The unmet proxies are named so the worker can be told exactly what to
    /// fix, and the caller can distinguish "ran and failed" from "never ran".
    Incomplete {
        /// Required proxies that failed or were absent, sorted and de-duplicated.
        unmet: Vec<DodProxy>,
    },
}

impl GateVerdict {
    /// `true` iff completion is admissible.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        matches!(self, GateVerdict::Complete)
    }
}

/// The structural completion gate — mechanism 2.
///
/// A [`CompletionGate`] carries the *required* proxy set. [`evaluate`] folds a
/// slice of observed [`GateOutcome`]s against it and returns [`GateVerdict`].
/// It is fail-closed on both axes: a required proxy that failed **and** a
/// required proxy that was never observed both land in `unmet`.
///
/// [`evaluate`]: CompletionGate::evaluate
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionGate {
    required: Vec<DodProxy>,
}

impl CompletionGate {
    /// A gate requiring the full [`DodProxy::CANONICAL`] set.
    #[must_use]
    pub fn canonical() -> Self {
        Self {
            required: DodProxy::CANONICAL.to_vec(),
        }
    }

    /// A gate requiring exactly `required` (deduplicated). Used when a formula
    /// narrows the `DoD` — e.g. a docs-only molecule may drop [`DodProxy::Test`].
    /// A formula may **narrow** the required set but the seam never lets the
    /// oracle widen it (same append-only-at-invocation discipline as ADR-075).
    #[must_use]
    pub fn with_required(required: impl IntoIterator<Item = DodProxy>) -> Self {
        let mut r: Vec<DodProxy> = required.into_iter().collect();
        r.sort();
        r.dedup();
        Self { required: r }
    }

    /// The required proxy set.
    #[must_use]
    pub fn required(&self) -> &[DodProxy] {
        &self.required
    }

    /// Fold observed outcomes against the required set.
    ///
    /// The last observation for a given proxy wins (a re-run overrides an
    /// earlier result). A required proxy with no observation is unmet.
    #[must_use]
    pub fn evaluate(&self, observed: &[GateOutcome]) -> GateVerdict {
        let mut unmet = Vec::new();
        for &proxy in &self.required {
            // Last-observation-wins: fold right-to-left.
            let passed = observed
                .iter()
                .rev()
                .find(|o| o.proxy == proxy)
                .map(|o| o.passed);
            match passed {
                Some(true) => {}
                Some(false) | None => unmet.push(proxy),
            }
        }
        if unmet.is_empty() {
            GateVerdict::Complete
        } else {
            unmet.sort();
            unmet.dedup();
            GateVerdict::Incomplete { unmet }
        }
    }
}

// ---------------------------------------------------------------------------
// Mechanism 3 — systematic-vs-stochastic classification + bounded repair (k≤2)
// ---------------------------------------------------------------------------

/// A normalised fingerprint of a failure — the observable that decides whether
/// two failures are "the same".
///
/// The caller normalises the raw error (strip volatile spans: timestamps,
/// temp-file names, addresses) so that two genuinely-identical failures hash
/// equal and two genuinely-different ones do not. Getting this normalisation
/// right is the caller's job; this type only compares.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FailureSignature(pub String);

impl FailureSignature {
    /// Construct from any string-like error fingerprint.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl From<&str> for FailureSignature {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

/// Whether a failure is expected to recur under an identical retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureClass {
    /// The current signature was already seen in this repair episode. An
    /// identical retry will reproduce it — retrying is a non-halting energy
    /// sink. **Do not retry.**
    Systematic,
    /// The current signature is new. The failure may be variance; a bounded
    /// retry is admissible (subject to the [`RepairBudget`]).
    Stochastic,
}

/// Classify a failure by comparing its signature against the signatures already
/// seen in this repair episode.
///
/// This is the load-bearing "**classify before retry**" gate from the
/// deliberation: a recurring signature is [`FailureClass::Systematic`] and must
/// short-circuit the retry loop; a novel signature is
/// [`FailureClass::Stochastic`] and may be retried within budget.
#[must_use]
pub fn classify_failure(prior: &[FailureSignature], current: &FailureSignature) -> FailureClass {
    if prior.iter().any(|s| s == current) {
        FailureClass::Systematic
    } else {
        FailureClass::Stochastic
    }
}

/// The bounded schema-repair budget — mechanism 3.
///
/// `k_max` is the maximum number of *retries* (not counting the original
/// attempt). The deliberation fixes the real duty cycle at `k ≤ 2`: past two
/// repairs, a weak oracle is not converging and the turn/energy budget should
/// own the halt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairBudget {
    /// Maximum number of retries after the original attempt.
    pub k_max: u32,
}

impl RepairBudget {
    /// The `k ≤ 2` bound the deliberation converged on.
    pub const BOUNDED: Self = Self { k_max: 2 };
}

impl Default for RepairBudget {
    fn default() -> Self {
        Self::BOUNDED
    }
}

/// What to do after a structured-output failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepairDecision {
    /// The failure is stochastic and the budget is not exhausted — retry.
    Retry,
    /// The failure is systematic (identical signature recurred) — halt; an
    /// identical retry cannot make progress.
    HaltSystematic,
    /// The `k ≤ k_max` budget is exhausted — halt and hand off to the
    /// turn/energy budget or the operator.
    HaltBudgetExhausted,
}

/// Decide whether to attempt another repair.
///
/// `attempts_used` is the number of retries already spent (0 before the first
/// retry). `class` is the classification of the *current* failure. The order
/// of checks matters: a systematic failure halts even when budget remains
/// (retrying it is pointless), and the budget halts a stochastic failure the
/// moment `attempts_used` reaches `k_max`.
#[must_use]
pub fn repair_decision(
    attempts_used: u32,
    class: FailureClass,
    budget: RepairBudget,
) -> RepairDecision {
    match class {
        FailureClass::Systematic => RepairDecision::HaltSystematic,
        FailureClass::Stochastic if attempts_used < budget.k_max => RepairDecision::Retry,
        FailureClass::Stochastic => RepairDecision::HaltBudgetExhausted,
    }
}

// ---------------------------------------------------------------------------
// Mechanism 4 — turn/energy halting budget (external stop guarantee)
// ---------------------------------------------------------------------------

/// The external halting guarantee — mechanism 4.
///
/// A weak oracle cannot be trusted to emit its own stop signal reliably (it may
/// loop, or claim done while incomplete). The runtime therefore imposes a
/// two-axis ceiling: a maximum number of **turns** and a maximum **energy**
/// spend (tokens, or any monotone non-negative cost the caller accumulates).
/// Either ceiling being crossed halts the loop, regardless of the oracle's own
/// signal. This is the guarantee that the loop terminates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HaltingBudget {
    /// Hard ceiling on inference turns.
    pub max_turns: u32,
    /// Hard ceiling on cumulative energy (e.g. total tokens).
    pub max_energy: u64,
}

impl HaltingBudget {
    /// A conservative default: 30 turns (matching the harness `TurnBudget`)
    /// and a 2 M-token energy ceiling, generous for a single molecule yet
    /// finite enough to bound a runaway weak-oracle loop.
    pub const DEFAULT: Self = Self {
        max_turns: 30,
        max_energy: 2_000_000,
    };
}

impl Default for HaltingBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Running tally of what a worker loop has spent so far.
///
/// Monotone non-decreasing on both axes; charges saturate rather than wrap so a
/// pathological energy report can never roll the counter back under the
/// ceiling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetLedger {
    /// Turns taken so far.
    pub turns_spent: u32,
    /// Cumulative energy spent so far.
    pub energy_spent: u64,
}

/// The per-turn budget verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BudgetVerdict {
    /// Both ceilings hold — the loop may continue.
    Continue,
    /// The turn ceiling was reached.
    HaltTurns,
    /// The energy ceiling was reached.
    HaltEnergy,
}

impl BudgetLedger {
    /// Record one more turn.
    pub fn charge_turn(&mut self) {
        self.turns_spent = self.turns_spent.saturating_add(1);
    }

    /// Record `amount` more energy.
    pub fn charge_energy(&mut self, amount: u64) {
        self.energy_spent = self.energy_spent.saturating_add(amount);
    }

    /// Check both ceilings. Turns are checked first (cheaper signal, and the
    /// axis the harness already enforces), then energy.
    #[must_use]
    pub fn verdict(&self, budget: HaltingBudget) -> BudgetVerdict {
        if self.turns_spent >= budget.max_turns {
            BudgetVerdict::HaltTurns
        } else if self.energy_spent >= budget.max_energy {
            BudgetVerdict::HaltEnergy
        } else {
            BudgetVerdict::Continue
        }
    }
}

// ---------------------------------------------------------------------------
// Mechanism 5 — tool-call cycle detection
// ---------------------------------------------------------------------------

/// A collapsed fingerprint of one tool call: `(name, args)` reduced to a single
/// comparable token.
///
/// The caller builds it from the tool name plus a stable hash/normalisation of
/// the arguments, so that two calls that do the same thing compare equal. The
/// cycle detector never inspects the arguments themselves — only whether two
/// fingerprints match.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ToolCallFingerprint(pub String);

impl ToolCallFingerprint {
    /// Build a fingerprint from a tool name and an arguments fingerprint.
    ///
    /// The two are joined with a NUL separator so `("ab", "c")` and
    /// `("a", "bc")` cannot collide.
    #[must_use]
    pub fn of(name: &str, args_fingerprint: &str) -> Self {
        Self(format!("{name}\u{0}{args_fingerprint}"))
    }
}

/// A detected cycle in the tool-call log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CycleReport {
    /// The length of the repeating block (the period). `1` means the same
    /// single call repeats; `2` means an `A,B,A,B` alternation, etc.
    pub period: usize,
    /// How many consecutive copies of the block were observed at the tail.
    pub repeats: usize,
}

/// The default number of identical consecutive blocks that constitutes a cycle.
///
/// Two consecutive identical blocks can be legitimate (read a file, act, read
/// it again to confirm). Three identical consecutive blocks is a stuck loop:
/// the oracle is not using the results it already has.
pub const DEFAULT_CYCLE_REPEATS: usize = 3;

/// Detect a cycle at the **tail** of a tool-call log.
///
/// Scans for the smallest period `p ≥ 1` such that the last `min_repeats · p`
/// fingerprints consist of `min_repeats` identical consecutive blocks of length
/// `p`. Returns the shortest such cycle (the tightest loop), or `None` if the
/// tail is not cyclic.
///
/// Only the tail is inspected: a worker that looped earlier but has since made
/// progress is not stuck *now*. `min_repeats` must be `≥ 2` (a period repeated
/// zero or one times is not a cycle); a `min_repeats < 2` is clamped to 2.
#[must_use]
pub fn detect_tool_call_cycle(
    log: &[ToolCallFingerprint],
    min_repeats: usize,
) -> Option<CycleReport> {
    let repeats = min_repeats.max(2);
    let n = log.len();
    // Need at least `repeats` copies of a period-1 block to say anything.
    if n < repeats {
        return None;
    }
    // Shortest period first → report the tightest loop.
    let max_period = n / repeats;
    for period in 1..=max_period {
        let block_len = period * repeats;
        let tail = &log[n - block_len..];
        // Compare each of the `repeats` blocks against the first block.
        let first = &tail[..period];
        let all_equal = (1..repeats).all(|k| &tail[k * period..(k + 1) * period] == first);
        if all_equal {
            return Some(CycleReport { period, repeats });
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Composed per-turn guard — budget + cycle, the two that fire inside the loop
// ---------------------------------------------------------------------------

/// A per-turn halt reason returned by [`TurnGuard::assess`].
///
/// These are the two hardening mechanisms that fire *during* the loop (budget
/// and cycle). The completion gate (mechanism 2) fires at the *end* and the
/// repair classifier (mechanism 3) fires on a *failure* — both are separate
/// call sites with their own return types, so they are not folded in here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopHalt {
    /// The turn ceiling was reached.
    BudgetTurns,
    /// The energy ceiling was reached.
    BudgetEnergy,
    /// The tool-call log went cyclic.
    Cycle(CycleReport),
}

/// The composed per-turn guard the worker-spawn seam calls once per turn.
///
/// It folds the two in-loop mechanisms — the [`HaltingBudget`] and the
/// [`detect_tool_call_cycle`] scan — into a single verdict. Budget is checked
/// first (the definitive external guarantee); a cycle is a tighter, earlier
/// signal that fires before the budget would.
#[derive(Debug, Clone, Copy)]
pub struct TurnGuard {
    /// The turn/energy ceilings.
    pub budget: HaltingBudget,
    /// How many identical consecutive blocks constitute a cycle.
    pub cycle_repeats: usize,
}

impl TurnGuard {
    /// A guard with the default budget and cycle threshold.
    #[must_use]
    pub fn new() -> Self {
        Self {
            budget: HaltingBudget::DEFAULT,
            cycle_repeats: DEFAULT_CYCLE_REPEATS,
        }
    }

    /// Assess the loop state. Returns `None` to continue, or the halt reason.
    ///
    /// `ledger` is the running spend; `tool_log` is the full tool-call
    /// fingerprint history (the detector inspects only its tail).
    #[must_use]
    pub fn assess(
        &self,
        ledger: &BudgetLedger,
        tool_log: &[ToolCallFingerprint],
    ) -> Option<LoopHalt> {
        match ledger.verdict(self.budget) {
            BudgetVerdict::HaltTurns => return Some(LoopHalt::BudgetTurns),
            BudgetVerdict::HaltEnergy => return Some(LoopHalt::BudgetEnergy),
            BudgetVerdict::Continue => {}
        }
        detect_tool_call_cycle(tool_log, self.cycle_repeats).map(LoopHalt::Cycle)
    }
}

impl Default for TurnGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::{prop_assert, prop_assert_eq};

    // --- Mechanism 2: completion gate ------------------------------------

    fn all_pass() -> Vec<GateOutcome> {
        DodProxy::CANONICAL
            .iter()
            .map(|&p| GateOutcome::new(p, true))
            .collect()
    }

    #[test]
    fn gate_complete_when_all_canonical_pass() {
        let gate = CompletionGate::canonical();
        assert_eq!(gate.evaluate(&all_pass()), GateVerdict::Complete);
        assert!(gate.evaluate(&all_pass()).is_complete());
    }

    #[test]
    fn gate_incomplete_names_failed_proxy() {
        let gate = CompletionGate::canonical();
        let mut obs = all_pass();
        obs[1] = GateOutcome::new(DodProxy::Test, false);
        let v = gate.evaluate(&obs);
        assert_eq!(
            v,
            GateVerdict::Incomplete {
                unmet: vec![DodProxy::Test]
            }
        );
    }

    #[test]
    fn gate_fail_closed_on_missing_proxy() {
        // A proxy never observed is unmet — the oracle cannot earn completion
        // by simply not reporting the gate it would fail.
        let gate = CompletionGate::canonical();
        let partial: Vec<GateOutcome> = DodProxy::CANONICAL
            .iter()
            .filter(|&&p| p != DodProxy::Lint)
            .map(|&p| GateOutcome::new(p, true))
            .collect();
        assert_eq!(
            gate.evaluate(&partial),
            GateVerdict::Incomplete {
                unmet: vec![DodProxy::Lint]
            }
        );
    }

    #[test]
    fn gate_last_observation_wins() {
        // A re-run that now passes overrides an earlier failure.
        let gate = CompletionGate::with_required([DodProxy::Build]);
        let obs = vec![
            GateOutcome::new(DodProxy::Build, false),
            GateOutcome::new(DodProxy::Build, true),
        ];
        assert_eq!(gate.evaluate(&obs), GateVerdict::Complete);
    }

    #[test]
    fn gate_narrowing_drops_a_proxy() {
        let gate = CompletionGate::with_required([DodProxy::Build, DodProxy::Format]);
        let obs = vec![
            GateOutcome::new(DodProxy::Build, true),
            GateOutcome::new(DodProxy::Format, true),
        ];
        assert_eq!(gate.evaluate(&obs), GateVerdict::Complete);
    }

    // --- Mechanism 3: classify + bounded repair --------------------------

    #[test]
    fn novel_signature_is_stochastic() {
        let prior = vec![FailureSignature::from("E0277 missing trait")];
        let cur = FailureSignature::from("E0308 mismatched types");
        assert_eq!(classify_failure(&prior, &cur), FailureClass::Stochastic);
    }

    #[test]
    fn recurring_signature_is_systematic() {
        let sig = FailureSignature::from("E0277 missing trait");
        let prior = vec![sig.clone()];
        assert_eq!(classify_failure(&prior, &sig), FailureClass::Systematic);
    }

    #[test]
    fn systematic_halts_even_with_budget_left() {
        assert_eq!(
            repair_decision(0, FailureClass::Systematic, RepairBudget::BOUNDED),
            RepairDecision::HaltSystematic
        );
    }

    #[test]
    fn stochastic_retries_within_budget_then_halts() {
        let b = RepairBudget::BOUNDED; // k_max = 2
        assert_eq!(
            repair_decision(0, FailureClass::Stochastic, b),
            RepairDecision::Retry
        );
        assert_eq!(
            repair_decision(1, FailureClass::Stochastic, b),
            RepairDecision::Retry
        );
        assert_eq!(
            repair_decision(2, FailureClass::Stochastic, b),
            RepairDecision::HaltBudgetExhausted
        );
    }

    // --- Mechanism 4: halting budget -------------------------------------

    #[test]
    fn budget_continues_below_ceilings() {
        let b = HaltingBudget {
            max_turns: 3,
            max_energy: 100,
        };
        let l = BudgetLedger {
            turns_spent: 2,
            energy_spent: 99,
        };
        assert_eq!(l.verdict(b), BudgetVerdict::Continue);
    }

    #[test]
    fn budget_halts_on_turn_ceiling() {
        let b = HaltingBudget {
            max_turns: 3,
            max_energy: 1_000,
        };
        let mut l = BudgetLedger::default();
        for _ in 0..3 {
            l.charge_turn();
        }
        assert_eq!(l.verdict(b), BudgetVerdict::HaltTurns);
    }

    #[test]
    fn budget_halts_on_energy_ceiling() {
        let b = HaltingBudget {
            max_turns: 100,
            max_energy: 500,
        };
        let mut l = BudgetLedger::default();
        l.charge_energy(500);
        assert_eq!(l.verdict(b), BudgetVerdict::HaltEnergy);
    }

    #[test]
    fn charges_saturate_not_wrap() {
        let mut l = BudgetLedger {
            turns_spent: u32::MAX,
            energy_spent: u64::MAX,
        };
        l.charge_turn();
        l.charge_energy(1);
        assert_eq!(l.turns_spent, u32::MAX);
        assert_eq!(l.energy_spent, u64::MAX);
    }

    // --- Mechanism 5: cycle detection ------------------------------------

    fn fp(s: &str) -> ToolCallFingerprint {
        ToolCallFingerprint(s.to_owned())
    }

    #[test]
    fn no_cycle_in_varied_log() {
        let log = vec![fp("a"), fp("b"), fp("c"), fp("d")];
        assert_eq!(detect_tool_call_cycle(&log, 3), None);
    }

    #[test]
    fn detects_period_one_stuck_call() {
        let log = vec![fp("x"), fp("a"), fp("a"), fp("a")];
        assert_eq!(
            detect_tool_call_cycle(&log, 3),
            Some(CycleReport {
                period: 1,
                repeats: 3
            })
        );
    }

    #[test]
    fn detects_period_two_alternation() {
        let log = vec![
            fp("z"),
            fp("a"),
            fp("b"),
            fp("a"),
            fp("b"),
            fp("a"),
            fp("b"),
        ];
        assert_eq!(
            detect_tool_call_cycle(&log, 3),
            Some(CycleReport {
                period: 2,
                repeats: 3
            })
        );
    }

    #[test]
    fn two_repeats_is_not_a_cycle_by_default() {
        // A,B,A,B is a confirm-loop, not a stuck-loop, at the default threshold.
        let log = vec![fp("a"), fp("b"), fp("a"), fp("b")];
        assert_eq!(detect_tool_call_cycle(&log, DEFAULT_CYCLE_REPEATS), None);
    }

    #[test]
    fn reports_shortest_period() {
        // "a,a,a,a,a,a" is period-1 repeated 6× — must report the tightest (1).
        let log: Vec<_> = std::iter::repeat_with(|| fp("a")).take(6).collect();
        let r = detect_tool_call_cycle(&log, 3).unwrap();
        assert_eq!(r.period, 1);
    }

    #[test]
    fn fingerprint_of_is_collision_free() {
        assert_ne!(
            ToolCallFingerprint::of("ab", "c"),
            ToolCallFingerprint::of("a", "bc")
        );
    }

    // --- Composed guard --------------------------------------------------

    #[test]
    fn turn_guard_budget_beats_cycle() {
        let guard = TurnGuard {
            budget: HaltingBudget {
                max_turns: 1,
                max_energy: 1_000_000,
            },
            cycle_repeats: 3,
        };
        let mut ledger = BudgetLedger::default();
        ledger.charge_turn();
        let log = vec![fp("a"), fp("a"), fp("a")];
        // Budget is checked first — turns exhausted dominates.
        assert_eq!(guard.assess(&ledger, &log), Some(LoopHalt::BudgetTurns));
    }

    #[test]
    fn turn_guard_flags_cycle_when_budget_ok() {
        let guard = TurnGuard::new();
        let ledger = BudgetLedger {
            turns_spent: 1,
            energy_spent: 10,
        };
        let log = vec![fp("a"), fp("a"), fp("a")];
        assert_eq!(
            guard.assess(&ledger, &log),
            Some(LoopHalt::Cycle(CycleReport {
                period: 1,
                repeats: 3
            }))
        );
    }

    #[test]
    fn turn_guard_continues_when_healthy() {
        let guard = TurnGuard::new();
        let ledger = BudgetLedger {
            turns_spent: 1,
            energy_spent: 10,
        };
        let log = vec![fp("a"), fp("b"), fp("c")];
        assert_eq!(guard.assess(&ledger, &log), None);
    }

    proptest::proptest! {
        /// A repair loop driven by `repair_decision` always halts within
        /// `k_max + 1` attempts, whatever the failure classes.
        #[test]
        fn repair_loop_always_halts(k_max in 0u32..8, systematic_at in 0usize..12) {
            let budget = RepairBudget { k_max };
            let mut attempts = 0u32;
            let mut halted = false;
            for i in 0..(k_max + 4) {
                let class = if (i as usize) == systematic_at {
                    FailureClass::Systematic
                } else {
                    FailureClass::Stochastic
                };
                match repair_decision(attempts, class, budget) {
                    RepairDecision::Retry => attempts += 1,
                    RepairDecision::HaltSystematic | RepairDecision::HaltBudgetExhausted => {
                        halted = true;
                        break;
                    }
                }
            }
            prop_assert!(halted);
            prop_assert!(attempts <= k_max);
        }

        /// Cycle detection never reports a cycle whose blocks are not in fact
        /// equal — reconstruct the tail and check.
        #[test]
        fn detected_cycle_blocks_are_equal(
            tokens in proptest::collection::vec("[a-c]", 0..20usize)
        ) {
            let log: Vec<_> = tokens.iter().map(|t| fp(t)).collect();
            if let Some(report) = detect_tool_call_cycle(&log, 3) {
                let block_len = report.period * report.repeats;
                let tail = &log[log.len() - block_len..];
                let first = &tail[..report.period];
                for k in 1..report.repeats {
                    prop_assert_eq!(&tail[k * report.period..(k + 1) * report.period], first);
                }
            }
        }
    }
}
