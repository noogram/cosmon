// SPDX-License-Identifier: AGPL-3.0-only

//! Protected reference inputs are read-only in the worker's worktree
//! (issue #94): an accidental write fails at once instead of surfacing only
//! at `cs done`.

use std::fs;

use cosmon_runtime::tackle_exec::mark_protected_read_only;

#[test]
fn protected_files_are_read_only_and_everything_else_stays_writable() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path();
    fs::create_dir_all(wt.join("ref/deep")).unwrap();
    fs::write(wt.join("ref/expected.csv"), "x,y\n").unwrap();
    fs::write(wt.join("ref/deep/golden.json"), "{}\n").unwrap();
    fs::write(wt.join("src.rs"), "fn main() {}\n").unwrap();

    let failures = mark_protected_read_only(wt, &["ref".to_owned(), "absent.csv".to_owned()]);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");

    // The write the incident described now fails where the worker makes it.
    assert!(fs::write(wt.join("ref/expected.csv"), "x,y\n1,2\n").is_err());
    assert!(fs::write(wt.join("ref/deep/golden.json"), "[]\n").is_err());
    // The directory stays writable, so the worktree can still be removed.
    assert!(!fs::metadata(wt.join("ref"))
        .unwrap()
        .permissions()
        .readonly());
    // Files outside the protected set are untouched.
    assert!(fs::write(wt.join("src.rs"), "fn main() { }\n").is_ok());
}
