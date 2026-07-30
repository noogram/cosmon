// SPDX-License-Identifier: AGPL-3.0-only

//! Real file collisions, measured on a real filesystem.
//!
//! # Why this file exists
//!
//! Operator decision D3 on `delib-20260729-1d4e`, in its governing sentence:
//!
//! > *The seal must always say two things: what it proves and what it does not
//! > model. A proof of the model must never be presented as a proof of the real
//! > environment.*
//!
//! `spores/cosmon-dev/spore.tla` declares, under WHAT IS NOT MODELLED,
//! *filesystem races / worktree lifecycle*. Its `NoResourceCollision` property
//! is therefore an argument about **strings**: `ArtifactPath(r) == r`, so the
//! conjunct `m # n => ArtifactPath(m) # ArtifactPath(n)` is the injectivity of
//! the identity function. That is sound, cheap, and worth keeping — and it is
//! not evidence that two nodes get two directories on the machine you are
//! standing on.
//!
//! So D3's last clause: *cover real file collisions SEPARATELY, by an
//! executable test*. That is this file. Everything here does I/O in a tempdir
//! — deliberately, because a second lexical argument would only restate the
//! seal in Rust.
//!
//! # The witness
//!
//! `("Route", "route")`. Both pass `validate_node_id` (its alphabet includes
//! uppercase ASCII). Both are distinct strings, so the seal reports no
//! collision. On APFS (macOS default) and NTFS they are ONE directory, and the
//! two nodes' `verdict.json` overwrite each other — a gate reading a sibling's
//! verdict as its own, which is exactly the class the seal is quoted as ruling
//! out.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use cosmon_core::spore::{
    expand, inject_run_outputs, node_output_dir, run_dir, NucleateCall, RefusedOutputHome, Spore,
    OUTPUT_DIR_VAR,
};

/// A spore shaped like `cosmon-dev`'s fan-in: several gates writing a
/// `verdict.json` each, under one shared run home.
const SPORE: &str = r#"
[spore]
name = "collision-probe"

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "intake"
kind = "fixed"
formula = "work"
[spore.node.vars]
topic = "Emit ${output_dir}/verdict.json"

[[spore.node]]
id = "route"
kind = "fixed"
formula = "work"
[spore.node.vars]
topic = "Emit ${output_dir}/verdict.json"

[[spore.node]]
id = "release"
kind = "fixed"
formula = "work"
[spore.node.vars]
topic = "Emit ${output_dir}/verdict.json"

[[spore.edge]]
from = "intake"
to = "release"
type = "feeds"

[[spore.edge]]
from = "route"
to = "release"
type = "verifies"
"#;

fn expanded() -> Vec<NucleateCall> {
    let spore = Spore::parse(SPORE).expect("probe spore parses");
    expand(&spore, &BTreeMap::new()).expect("probe spore expands")
}

/// Does this filesystem fold case? Measured, never assumed from `cfg!(macos)`:
/// APFS can be created case-sensitive, and a Linux CI box can mount a
/// case-insensitive volume. A diagnosis is a datum, so we probe.
fn filesystem_folds_case(root: &Path) -> bool {
    let lower = root.join("case-probe");
    fs::create_dir_all(&lower).expect("probe dir");
    root.join("CASE-PROBE").exists()
}

/// Every node writes its OWN `verdict.json`, and after the writes every node
/// reads back the bytes IT wrote. This is `NoResourceCollision` restated as a
/// measurement rather than a theorem: the files are really created, on the
/// filesystem this test is running on.
#[test]
fn every_node_keeps_its_own_verdict_file_on_a_real_filesystem() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run = run_dir(tmp.path(), "germ-real-1");

    let mut calls = expanded();
    inject_run_outputs(&mut calls, &run).expect("benign aliases germinate");

    for call in &calls {
        let out = Path::new(call.vars.get(OUTPUT_DIR_VAR).expect("output_dir handed"));
        fs::create_dir_all(out).expect("node output dir");
        fs::write(
            out.join("verdict.json"),
            format!("{{\"gate\":\"{}\"}}", call.alias),
        )
        .expect("node verdict");
    }

    for call in &calls {
        let out = Path::new(call.vars.get(OUTPUT_DIR_VAR).expect("output_dir handed"));
        let got = fs::read_to_string(out.join("verdict.json")).expect("verdict readable");
        assert_eq!(
            got,
            format!("{{\"gate\":\"{}\"}}", call.alias),
            "node {} read back a verdict it did not write — a real file collision",
            call.alias
        );
    }
}

/// The witness, executed. Two aliases distinct as strings; one directory once
/// the filesystem folds them.
///
/// The assertion is CONDITIONAL on the probe and that is the honest shape: on a
/// case-sensitive volume there is no collision to observe, and pretending to
/// observe one would be the same overclaim this test exists to correct. What is
/// unconditional is the refusal below — a spore is a portable moule, and a
/// germination that succeeds on Linux while aliasing on a reviewer's Mac is
/// worse than one that refuses on both.
#[test]
fn case_folding_really_collapses_two_node_homes_into_one() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run = run_dir(tmp.path(), "germ-real-2");
    fs::create_dir_all(&run).expect("run home");

    let upper = node_output_dir(&run, "Route").expect("valid alias");
    let lower = node_output_dir(&run, "route").expect("valid alias");
    assert_ne!(
        upper, lower,
        "the two homes are distinct PATHS — the seal's claim"
    );

    fs::create_dir_all(&upper).expect("first home");
    fs::write(upper.join("verdict.json"), r#"{"gate":"Route"}"#).expect("first verdict");
    fs::create_dir_all(&lower).expect("second home");
    fs::write(lower.join("verdict.json"), r#"{"gate":"route"}"#).expect("second verdict");

    let first_back = fs::read_to_string(upper.join("verdict.json")).expect("first readable");

    if filesystem_folds_case(tmp.path()) {
        assert_eq!(
            first_back, r#"{"gate":"route"}"#,
            "on a case-folding filesystem the second node MUST have overwritten the first; \
             if it did not, this test's witness is stale and the guard below is unjustified"
        );
    } else {
        assert_eq!(
            first_back, r#"{"gate":"Route"}"#,
            "on a case-sensitive filesystem the two homes stay distinct"
        );
    }
}

/// And therefore: germination refuses the pair outright, on every platform.
///
/// This is the executable half of D3. The seal keeps its property and its
/// declared scope; the pair that its scope does not reach never reaches a
/// worker.
#[test]
fn germination_refuses_the_pair_the_seal_cannot_see() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run = run_dir(tmp.path(), "germ-real-3");

    let mut calls = expanded();
    calls[1].alias = "Route".to_string();
    calls[2].alias = "route".to_string();

    let err = inject_run_outputs(&mut calls, &run).expect_err("must refuse");
    assert_eq!(
        err,
        RefusedOutputHome::CaseAliased {
            first: "Route".to_string(),
            second: "route".to_string(),
        }
    );
    assert!(
        !run.exists(),
        "a refused germination leaves no durable trace"
    );
}
