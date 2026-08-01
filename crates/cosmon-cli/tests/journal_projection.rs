// SPDX-License-Identifier: AGPL-3.0-only

//! `cs events journal` — the per-molecule journal, end to end through the real
//! binary.
//!
//! The unit tests in `cosmon-state::journal` pin the fold. What they cannot
//! pin is the property the operator's contract turns on: that asking for a
//! molecule's journal **writes nothing**. That is a claim about a process, not
//! about a function, so it is measured here — a snapshot of the galaxy before
//! and after, in the spirit of ADR-166's residue test, with the one ambient
//! writer every `cs` invocation carries named rather than papered over.
//!
//! The scenario is deliberately the worst one: a molecule that was nucleated
//! and then refused on its first dispatch. It has no worker, no worktree, no
//! molecule directory and no artefacts. Everything an operator can learn about
//! it lives in the galaxy ledger, which is exactly what the journal projects.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MOLECULE: &str = "task-20260730-7a74";

/// A galaxy whose ledger holds one nucleation, one unrelated molecule's
/// nucleation, and one typed root-spawn refusal — and nothing else.
fn galaxy_with_a_refused_molecule() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join(".cosmon/state");
    fs::create_dir_all(&state).expect("create state dir");
    fs::write(
        state.join("events.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            r#"{"seq":1,"mol_seq":1,"timestamp":"2026-07-30T08:00:00Z","type":"molecule_nucleated","molecule_id":"task-20260730-7a74","formula_id":"task-work"}"#,
            r#"{"seq":2,"mol_seq":1,"timestamp":"2026-07-30T08:00:01Z","type":"molecule_nucleated","molecule_id":"task-20260730-9999","formula_id":"task-work"}"#,
            r#"{"seq":3,"type":"tackle_refused","molecule_id":"task-20260730-7a74","worker_id":null,"reason":"root-spawn-refused:demote-shares-repository-storage","detail":"re-run as uid 501"}"#,
        ),
    )
    .expect("write ledger");
    tmp
}

fn journal_cmd(root: &Path, json: bool) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env("COSMON_STATE_DIR", root.join(".cosmon/state"))
        .current_dir(root);
    if json {
        cmd.arg("--json");
    }
    cmd.args(["events", "journal", MOLECULE]);
    cmd.arg("--ops-dir").arg(root.join(".cosmon/state"));
    cmd
}

/// The ledger's own scan-cursor cache (`events.jsonl.seqidx`), which the
/// event-log writer rewrites beside `events.jsonl` on every append.
///
/// Exempted from the residue comparison for the same reason, and only the
/// same reason, as the ambient `operator_present` row: it is a consequence
/// of `main`'s presence emission, which happens before any subcommand is
/// dispatched. It is a cache of a fold over the ledger, holds nothing the
/// ledger does not, and is regenerated from it if deleted. A journal that
/// created a file of its own still fails the comparison — the exemption is
/// this one path, not the directory it lives in.
fn is_ledger_cache(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "seqidx")
}

/// Every path under `root`, with its bytes — the comparison a residue check
/// needs. Directories contribute their path with empty content; mtime is
/// deliberately absent, since it changes for innocent reasons.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            if path.is_dir() {
                out.push((rel, Vec::new()));
                stack.push(path);
            } else {
                let bytes = fs::read(&path).unwrap_or_default();
                out.push((rel, bytes));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn a_molecule_refused_on_its_first_dispatch_has_a_journal_that_says_why() {
    let tmp = galaxy_with_a_refused_molecule();
    let out = journal_cmd(tmp.path(), false)
        .output()
        .expect("run cs events journal");
    assert!(
        out.status.success(),
        "cs events journal failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("root-spawn-refused:demote-shares-repository-storage"),
        "the refusal is the one thing this molecule has to say; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("molecule_nucleated"),
        "the journal exists from nucleation, so the nucleation row is in it; stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("task-20260730-9999"),
        "a sibling molecule's row must not leak into this journal; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("1 blockage(s)"),
        "the blockage must be counted as such, not rendered as ordinary progress; \
         stdout:\n{stdout}"
    );
}

#[test]
fn the_json_form_emits_the_projected_ledger_rows_verbatim() {
    let tmp = galaxy_with_a_refused_molecule();
    let out = journal_cmd(tmp.path(), true)
        .output()
        .expect("run cs --json events journal");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each line is one ledger row"))
        .collect();
    assert_eq!(rows.len(), 2, "two rows name this molecule; got:\n{stdout}");
    assert_eq!(rows[0]["type"], "molecule_nucleated");
    assert_eq!(rows[1]["type"], "tackle_refused");
}

#[test]
fn projecting_a_journal_creates_nothing_and_appends_no_row_of_its_own() {
    // The clause that makes "exists from nucleation" safe: the view is
    // computed, never stored, so no dispatcher — privileged or not — leaves
    // anything behind by asking for it. A design that materialised the journal
    // on read would fail here, which is the point.
    //
    // The assertion is deliberately not "the galaxy is byte-identical", and
    // the difference matters. Every `cs` invocation appends one ambient
    // `operator_present` row from `main`, before any subcommand is dispatched
    // (delib-20260509-18df §D-B). A byte-identity claim would therefore be
    // false for a reason that has nothing to do with this command, and the
    // usual repair — asserting on some narrower path — is how ADR-166 got a
    // test that passed while the residue it was written for went unnoticed.
    //
    // So this pins the two things that are actually about the projection:
    // the set of paths is unchanged (nothing created, nothing removed), and
    // every row the ledger gained is an ambient presence row. A journal that
    // wrote anything of its own fails the second clause.
    let tmp = galaxy_with_a_refused_molecule();
    let before = snapshot(tmp.path());
    let ledger = tmp.path().join(".cosmon/state/events.jsonl");
    let ledger_before = fs::read_to_string(&ledger).expect("read ledger");

    for json in [false, true] {
        let out = journal_cmd(tmp.path(), json).output().expect("run");
        assert!(out.status.success());
    }
    // Also ask about a molecule the ledger has never heard of — the path most
    // likely to create something on the way to reporting nothing.
    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .env("COSMON_STATE_DIR", tmp.path().join(".cosmon/state"))
        .current_dir(tmp.path())
        .args(["events", "journal", "task-20260730-0000"])
        .arg("--ops-dir")
        .arg(tmp.path().join(".cosmon/state"))
        .output()
        .expect("run");
    assert!(out.status.success());

    let after = snapshot(tmp.path());
    let paths_before: Vec<&PathBuf> = before
        .iter()
        .map(|(p, _)| p)
        .filter(|p| !is_ledger_cache(p))
        .collect();
    let paths_after: Vec<&PathBuf> = after
        .iter()
        .map(|(p, _)| p)
        .filter(|p| !is_ledger_cache(p))
        .collect();
    assert_eq!(
        paths_before, paths_after,
        "projecting a journal must not create or remove anything"
    );

    let ledger_after = fs::read_to_string(&ledger).expect("read ledger");
    assert!(
        ledger_after.starts_with(&ledger_before),
        "the ledger must only ever be appended to, never rewritten"
    );
    for line in ledger_after[ledger_before.len()..]
        .lines()
        .filter(|l| !l.trim().is_empty())
    {
        let row: serde_json::Value = serde_json::from_str(line).expect("appended row parses");
        assert_eq!(
            row["type"], "operator_present",
            "the only row a journal projection may be followed by is the ambient \
             presence row every `cs` invocation writes; got: {line}"
        );
    }
}

#[test]
fn a_malformed_molecule_id_is_refused_rather_than_projected_as_empty() {
    // "The ledger says nothing about it" and "you asked for something that is
    // not a molecule id" must not render identically — the second is a typo an
    // operator should be told about, not an empty page.
    let tmp = galaxy_with_a_refused_molecule();
    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .env("COSMON_STATE_DIR", tmp.path().join(".cosmon/state"))
        .current_dir(tmp.path())
        .args(["events", "journal", "not-a-molecule"])
        .arg("--ops-dir")
        .arg(tmp.path().join(".cosmon/state"))
        .output()
        .expect("run");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("invalid molecule id"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
