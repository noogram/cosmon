// SPDX-License-Identifier: AGPL-3.0-only

//! Spec-as-test for the P3 calibration probe: the tracked seed-corpus and the
//! `calibration-probe` formula.
//!
//! Two gates live here, both `include_str!`-compiled (no runtime I/O, so
//! cosmon-core stays a zero-I/O core):
//!
//! 1. **The tracked corpus is well-formed.** Every JSON entry under
//!    `evidence/calibration-corpus/entries/` deserializes into the Rust mirror
//!    [`cosmon_core::calibration::CorpusEntry`] and passes `validate()` — the
//!    labelled dataset cannot silently drift out of shape. `pack-4` is row 1
//!    (the `pack(4)` case), carries a tautological trap, and covers all four
//!    P1-P4 columns.
//! 2. **The formula keeps its load-bearing shape and discipline.** Three steps
//!    (`replay` → `classify-and-snapshot` → `diff-and-report`), and the
//!    judgment-vs-liveness discipline is asserted in the description so a
//!    refactor that quietly reframes it as a liveness gate reddens this test.

use cosmon_core::calibration::{CorpusEntry, JudgmentPathology};
use cosmon_core::formula::Formula;

const PACK_4: &str = include_str!("../../../evidence/calibration-corpus/entries/pack-4.json");
const SINGULAR_COV: &str =
    include_str!("../../../evidence/calibration-corpus/entries/singular-cov.json");
const CALIBRATION_TOML: &str =
    include_str!("../../../.cosmon/formulas/calibration-probe.formula.toml");

fn parse_entry(json: &str) -> CorpusEntry {
    serde_json::from_str(json).expect("corpus entry must deserialize into CorpusEntry")
}

#[test]
fn every_tracked_corpus_entry_is_valid() {
    for json in [PACK_4, SINGULAR_COV] {
        let entry = parse_entry(json);
        entry
            .validate()
            .expect("tracked corpus entry must pass Corpus validation");
    }
}

#[test]
fn pack_4_is_row_one_with_a_tautological_trap() {
    let entry = parse_entry(PACK_4);
    assert_eq!(entry.id, "pack-4", "the pack(4) case is the seed row");
    assert!(
        !entry.known_tautological_trap.trim().is_empty(),
        "the corpus's whole point is a KNOWN tautological trap per entry"
    );
    // The clean verdict must name the TRUE root (truncating division), not
    // parrot the reporter's stated-wrong 'overflow' bait.
    assert!(
        entry.clean_verdict.root.to_lowercase().contains("truncat"),
        "clean verdict must name the true root (truncating division), \
         not adopt the anchoring bait"
    );
}

#[test]
fn corpus_entries_cover_all_four_pathology_columns() {
    for json in [PACK_4, SINGULAR_COV] {
        let entry = parse_entry(json);
        for pathology in JudgmentPathology::ALL {
            let n = entry
                .pathology_traps
                .iter()
                .filter(|t| t.pathology == pathology)
                .count();
            assert_eq!(
                n,
                1,
                "entry `{}` must carry exactly one trap for {}",
                entry.id,
                pathology.code()
            );
        }
    }
}

#[test]
fn formula_parses_with_three_step_shape() {
    let formula = Formula::parse(CALIBRATION_TOML).expect("calibration-probe formula must parse");
    assert_eq!(formula.name.as_str(), "calibration-probe");
    assert_eq!(formula.id_prefix, "calib");
    assert!(
        !formula.freeze_on_last_step,
        "the probe is a sweep tick, not a persistent actor — must not freeze"
    );

    let step_ids: Vec<_> = formula.steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        step_ids,
        vec!["replay", "classify-and-snapshot", "diff-and-report"],
        "step shape is load-bearing — replay per adapter, classify against \
         the grid, diff against the stable baseline (the oracle-canary loop)"
    );
}

#[test]
fn formula_measures_judgment_not_liveness() {
    let formula = Formula::parse(CALIBRATION_TOML).expect("parse");
    let lowered = formula.description.to_lowercase();
    // The whole reason this probe is not oracle-canary: it must measure
    // judgment quality and explicitly disclaim liveness. If a refactor drops
    // that distinction, this test reddens.
    assert!(
        lowered.contains("judgment"),
        "description must frame the observable as judgment quality"
    );
    assert!(
        lowered.contains("liveness"),
        "description must explicitly contrast against liveness"
    );
    assert!(
        lowered.contains("not a certificate") || lowered.contains("lower bound"),
        "description must frame output as a re-measurable snapshot / lower \
         bound, never a certificate (Rice-flavored)"
    );
}

#[test]
fn formula_reuses_the_oracle_canary_loop_and_cites_it() {
    let formula = Formula::parse(CALIBRATION_TOML).expect("parse");
    let lowered = formula.description.to_lowercase();
    assert!(
        lowered.contains("oracle-canary"),
        "the loop is reused from oracle-canary — cite it (RÉUTILISE)"
    );
    assert!(
        lowered.contains("baseline"),
        "the diff-against-stable-baseline shape is the reused mechanism"
    );
}
