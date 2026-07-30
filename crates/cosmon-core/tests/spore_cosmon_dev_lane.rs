// SPDX-License-Identifier: AGPL-3.0-only

//! The `cosmon-dev` spore's lane entry is MECHANICAL — and this test is what makes
//! that sentence checkable rather than aspirational.
//!
//! The operator's decision (D1 amendment 2 on `delib-20260729-1d4e`) has one
//! load-bearing asymmetry: **the operator may force the full lane; there must be no
//! knob that forces the fast lane.** A comment saying so is a comment. What is
//! enforced here is that `[spore.params.lane]` is a closed enum whose member set
//! does not contain `fast`, so `--var lane=fast` is refused at expansion by the same
//! code path that refuses any other non-member — and the refusal survives anybody
//! rewriting the prose around it.
//!
//! The measured reason this matters, quoted so a future editor meets it before
//! deleting the test: `risk = "normal"` was the DEFAULT, and it switched off exactly
//! the arm the #20 defect lived in. A criterion a tired pilot fills in by habit is
//! not a criterion.
//!
//! It also pins the position of `triage` in the DAG. The lane cannot be decided
//! before its judge exists, and the judge is the frozen red, so `triage` must sit
//! downstream of `reproduce` and upstream of `implement` — with no surviving
//! `reproduce -> implement` edge that would let the fix start without a lane.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cosmon_core::spore::{expand, Spore};

/// Read the shipped `spores/cosmon-dev/spore.toml` — the real manifest, not a
/// fixture. A fixture would drift from the file the missions actually germinate.
fn cosmon_dev_spore() -> Spore {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../spores/cosmon-dev/spore.toml")
        .canonicalize()
        .expect("spores/cosmon-dev/spore.toml is reachable from crates/cosmon-core");
    let text = std::fs::read_to_string(&path).expect("spore.toml is readable");
    Spore::parse(&text).expect("the shipped cosmon-dev manifest parses")
}

/// The three required params, so expansion gets past the schema and reaches the
/// clause under test.
fn required() -> BTreeMap<String, toml::Value> {
    [
        ("issue", "#21 --resident ignores COSMON_DEFAULT_ADAPTER"),
        ("affected_ref", "v0.2.2"),
        ("upstream_version", "0.2.2"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), toml::Value::String(v.to_string())))
    .collect()
}

/// `--var lane=full` germinates; `--var lane=fast` does not. One direction of a
/// door, which is the shape of every honest gate.
#[test]
fn lane_can_be_forced_to_full_and_never_to_fast() {
    let spore = cosmon_dev_spore();

    let mut widen = required();
    widen.insert("lane".to_string(), toml::Value::String("full".to_string()));
    expand(&spore, &widen).expect("the operator may always force the full lane");

    let mut force_fast = required();
    force_fast.insert("lane".to_string(), toml::Value::String("fast".to_string()));
    let err =
        expand(&spore, &force_fast).expect_err("there must be no knob that forces the fast lane");
    let rendered = err.to_string();
    assert!(
        rendered.contains("fast"),
        "the refusal must name the value it refused, got: {rendered}"
    );

    // And the asymmetry is in the schema, not in a downstream check that a later
    // edit could route around: `fast` is simply not a member.
    let lane = spore
        .params
        .get("lane")
        .expect("a `lane` param is declared");
    assert!(
        !lane.values.iter().any(|v| v == "fast"),
        "`fast` must not be a member of the lane enum: {:?}",
        lane.values
    );
    assert!(
        lane.values.iter().any(|v| v == "full"),
        "`full` must be a member of the lane enum: {:?}",
        lane.values
    );
}

/// The judge is the frozen red, so the lane is decided after `reproduce` and before
/// `implement`. An `implement` that could start without a lane would make the lane
/// advisory, which is the defect the lane exists to remove.
#[test]
fn triage_sits_between_the_frozen_red_and_the_fix() {
    let spore = cosmon_dev_spore();

    assert!(
        spore.nodes.iter().any(|n| n.id == "triage"),
        "the cosmon-dev DAG must carry a `triage` node"
    );

    let edge = |from: &str, to: &str| spore.edges.iter().any(|e| e.from == from && e.to == to);

    assert!(edge("reproduce", "triage"), "reproduce must feed triage");
    assert!(edge("triage", "implement"), "triage must feed implement");
    assert!(
        !edge("reproduce", "implement"),
        "a surviving reproduce -> implement edge lets the fix start with no lane"
    );

    // Expansion agrees: `implement` is blocked by `triage`, not by `reproduce`.
    let calls = expand(&spore, &required()).expect("the shipped manifest expands");
    let implement = calls
        .iter()
        .find(|c| c.alias == "implement")
        .expect("an `implement` call is germinated");
    assert!(
        implement.blocked_by.iter().any(|b| b == "triage"),
        "implement must be blocked by triage, got {:?}",
        implement.blocked_by
    );
}

/// The blast-radius ceiling and the protected path set are declared params a
/// predicate reads — not prose in a paragraph somebody has to re-read.
#[test]
fn the_predicate_reads_declared_bounds_not_prose() {
    let spore = cosmon_dev_spore();

    let max_files = spore
        .params
        .get("fast_lane_max_files")
        .expect("`fast_lane_max_files` is declared");
    assert_eq!(
        max_files.default.as_ref().and_then(toml::Value::as_integer),
        Some(5),
        "the documented default ceiling is 5 files"
    );

    let surface = spore
        .params
        .get("release_surface")
        .expect("`release_surface` is declared");
    let entries = surface
        .default
        .as_ref()
        .and_then(toml::Value::as_array)
        .expect("release_surface defaults to a list");
    assert!(
        !entries.is_empty(),
        "an empty release surface is a universal over an empty domain: it forbids nothing"
    );
    for needle in ["scripts/publish.sh", "spores/cosmon-dev/"] {
        assert!(
            entries
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|p| p == needle),
            "`{needle}` must be off-limits to a fast-lane patch"
        );
    }
}
