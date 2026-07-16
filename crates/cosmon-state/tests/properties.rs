// SPDX-License-Identifier: AGPL-3.0-only

//! Property-based invariants for cosmon-state event log (spec-suite L1).
//!
//! Pins:
//! * the log is strictly append-only — after emitting N events, `read_all`
//!   returns exactly N envelopes;
//! * sequence numbers are monotone and dense (each emit returns `prev.next()`);
//! * re-opening the writer resumes sequencing without gap.

use chrono::{DateTime, Utc};
use cosmon_core::event_v2::{EventV2, StuckReason};
use cosmon_core::federation::FederationLineage;
use cosmon_core::id::MoleculeId;
use cosmon_state::event_log::{read_all, EventLogWriter};
use proptest::prelude::*;
use std::path::PathBuf;
use tempfile::TempDir;

fn mol_id(i: u32) -> MoleculeId {
    MoleculeId::new(format!("t-20260401-{i:04x}")).unwrap()
}

/// A fixed UTC timestamp for nested-struct payloads.
///
/// The event log is line-framed; the property under test is *framing*
/// invariance, not clock behaviour. A literal timestamp keeps the
/// nested-struct arm deterministic (and avoids a wall-clock read the
/// strategy has no business making).
fn fixed_ts() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-05-19T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

/// Free-form text corpus that includes the JSONL-hostile bytes — double
/// quote, backslash, embedded newline/CR/tab — that a naive line-framed
/// writer would corrupt. `serde_json` MUST escape these so every envelope
/// stays exactly one physical line; the append-only length assertion is
/// what catches a regression that lets a raw newline reach the wire.
fn arb_reason() -> impl Strategy<Value = String> {
    // `any::<String>()` defaults to `\PC*` (no control chars), which would
    // silently skip the very bytes that stress line framing — so pin an
    // explicit class that keeps quotes, backslashes and vertical whitespace.
    prop::string::string_regex(r#"[a-zA-Z0-9_ "\\\n\r\t]{0,48}"#).unwrap()
}

/// Representative spread of `EventV2` payload *shapes*, one arm per shape,
/// so the append-only / monotone / reopen-resume properties are exercised
/// against the serialization surface rather than a single variant.
///
/// The umbrella `every_variant_roundtrips` (`event_v2.rs`) proves each
/// variant serialises; this proves the *log framing* survives each payload
/// shape. Shapes covered: unit-like enum reason, adversarial free-form
/// string, twin `i64`, `Vec<MoleculeId>` (grown for a large payload), and
/// a nested struct (`FederationLineage`). Fix for C10 review F3 — the old
/// strategy generated only `MoleculeCollapsed`, proving framing for 1 of
/// ~70 variants.
fn arb_event() -> impl Strategy<Value = EventV2> {
    prop_oneof![
        // string shape — free-form reason with JSONL-hostile bytes.
        (0u32..256u32, arb_reason()).prop_map(|(i, reason)| EventV2::MoleculeCollapsed {
            molecule_id: mol_id(i),
            reason,
            kind: None,
        }),
        // unit shape — fieldless enum payload.
        (0u32..256u32).prop_map(|i| EventV2::MoleculeStuck {
            molecule_id: mol_id(i),
            reason: StuckReason::BlockerFailed,
        }),
        // i64 shape — twin signed integers spanning the full range.
        (0u32..256u32, any::<i64>(), any::<i64>(), arb_reason()).prop_map(
            |(i, pre, post, decision)| EventV2::RuntimeReadDecideWrite {
                path: format!("/s/{i:04x}/state.json"),
                pre_read_mtime_ns: pre,
                post_write_mtime_ns: post,
                decision,
            }
        ),
        // vec shape — variable-length child list (large-payload dimension).
        proptest::collection::vec(0u32..256u32, 0..12).prop_map(|ids| EventV2::DecaySpliced {
            parent: mol_id(0),
            children: ids.into_iter().map(mol_id).collect(),
        }),
        // nested-struct shape — Some(FederationLineage { .. }).
        (0u32..256u32, arb_reason()).prop_map(|(i, galaxy)| EventV2::ChronicleAdded {
            molecule_id: Some(mol_id(i)),
            chronicle_path: "docs/lore/CHRONICLES.md".to_owned(),
            entry_anchor: Some("federation-machinery".to_owned()),
            cites_galaxies: vec!["smithy".to_owned()],
            federation_provenance: Some(FederationLineage {
                source_galaxy: galaxy,
                source_commit: "deadbeef".to_owned(),
                source_path: PathBuf::from("docs/lore/2026-05-19.md"),
                crossed_at: fixed_ts(),
            }),
        }),
    ]
}

proptest! {
    #[test]
    fn prop_append_only_lengthens(events in proptest::collection::vec(arb_event(), 0..8)) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.jsonl");
        let mut writer = EventLogWriter::open(&path).unwrap();
        let mut last_seq: Option<u64> = None;
        for ev in &events {
            let seq = writer.emit(ev.clone(), None).unwrap();
            let seq_u: u64 = seq.0;
            if let Some(prev) = last_seq {
                prop_assert_eq!(seq_u, prev + 1);
            }
            last_seq = Some(seq_u);
        }
        writer.sync().unwrap();
        drop(writer);

        let back = read_all(&path).unwrap();
        prop_assert_eq!(back.len(), events.len());
        // Content roundtrip: a framing corruption that preserves the line
        // count (e.g. a raw newline split one event into two half-parseable
        // lines) would still fail here.
        for (got, want) in back.iter().zip(events.iter()) {
            prop_assert_eq!(&got.event, want);
        }
    }

    #[test]
    fn prop_reopen_resumes_sequencing(
        head in proptest::collection::vec(arb_event(), 1..4),
        tail in proptest::collection::vec(arb_event(), 1..4),
    ) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.jsonl");

        let mut writer = EventLogWriter::open(&path).unwrap();
        let mut last: u64 = 0;
        for ev in &head {
            let s: u64 = writer.emit(ev.clone(), None).unwrap().0;
            last = s;
        }
        writer.sync().unwrap();
        drop(writer);

        // Re-open — next_seq must resume where we left off.
        let mut writer2 = EventLogWriter::open(&path).unwrap();
        let next: u64 = writer2.next_seq().0;
        prop_assert_eq!(next, last + 1);

        for ev in &tail {
            writer2.emit(ev.clone(), None).unwrap();
        }
        writer2.sync().unwrap();
        drop(writer2);

        let all = read_all(&path).unwrap();
        prop_assert_eq!(all.len(), head.len() + tail.len());
    }
}
