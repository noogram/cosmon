// SPDX-License-Identifier: AGPL-3.0-only

//! Proptest harness for crash-resilience bisimulation.
//!
//! For any generated DAG and any crash point, the crashed+resumed run must
//! produce the same canonical event trace and the same terminal lifecycle
//! states as the uninterrupted run. 1000 cases.

use cosmon_crashtest::{canonicalize, gen_dag, run, DeterministicLlmStub, TempFleet};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]
    #[test]
    fn recovery_preserves_lifecycle_trace(
        dag_seed in any::<u64>(),
        crash_point in 1usize..20,
        llm_seed in any::<u64>(),
    ) {
        let dag = gen_dag(dag_seed, 5);
        let llm = DeterministicLlmStub::new(llm_seed);

        let baseline_fleet = TempFleet::new_tmpfs();
        let baseline = run(&baseline_fleet, &dag, &llm, None)
            .map_err(TestCaseError::fail)?;

        let crashed_fleet = TempFleet::new_tmpfs();
        let _partial = run(&crashed_fleet, &dag, &llm, Some(crash_point))
            .map_err(TestCaseError::fail)?;
        let resumed = run(&crashed_fleet, &dag, &llm, None)
            .map_err(TestCaseError::fail)?;

        prop_assert_eq!(canonicalize(&baseline.events), canonicalize(&resumed.events));
        prop_assert_eq!(baseline.terminal, resumed.terminal);
    }
}
