// SPDX-License-Identifier: AGPL-3.0-only

//! `cs init` never prints a shell command a reader can paste.
//!
//! # What was observed
//!
//! On 2026-09-07, the last line of a successful `cs init` was
//! `Symmetric undo: rm -rf <path>`, with the path interpolated raw. On a
//! project whose path contains a space, the printed line reads
//! `rm -rf /Users/someone/mon projet/.cosmon` — two arguments, not one.
//! A user who follows the instruction cosmon just gave them deletes
//! `/Users/someone/mon` recursively. The nested-galaxy refusal carried the
//! same shape.
//!
//! # What this file pins
//!
//! The *class*, not the wording: no rendered output of `cs init` — success
//! path or refusal path — contains an `rm -rf`. Quoting the path would have
//! made the printed command safe to paste and left the hazard one edit away;
//! the decision taken instead was to describe the action and let the reader
//! pick their own tool. A test asserting the exact new sentence would pass
//! again the day someone reintroduces a command elsewhere in the same
//! output, so the assertion is over the whole rendered text.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A `cs` invocation with the ambient cosmon session stripped, so the run is
/// hermetic: the suite is itself executed by a worker whose environment
/// carries a depth, an adapter and a model that would otherwise steer it.
fn cs(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd);
    for k in [
        "COSMON_PARENT_MOL_ID",
        "COSMON_MOL_DIR",
        "COSMON_DEFAULT_ADAPTER",
        "COSMON_DEFAULT_MODEL",
        "CB_SESSION_ROLE",
        "CB_DEPTH",
        "ANTHROPIC_MODEL",
        "COSMON_EGRESS_POLICY",
    ] {
        cmd.env_remove(k);
    }
    cmd
}

/// A directory whose name contains a space — the shape that turns one
/// printed argument into two.
fn spaced_dir(tmp: &Path) -> PathBuf {
    let dir = tmp.canonicalize().unwrap().join("mon projet");
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The whole rendered output of a command, both streams: a hazard printed on
/// stderr is read by exactly the same eyes as one printed on stdout.
fn rendered(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn successful_init_on_a_spaced_path_prints_no_shell_command() {
    let tmp = tempfile::tempdir().unwrap();
    let target = spaced_dir(tmp.path());

    let out = cs(&target).arg("init").output().unwrap();
    assert!(
        out.status.success(),
        "cs init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let text = rendered(&out);
    assert!(
        !text.contains("rm -rf"),
        "cs init printed a shell command on a path containing a space; \
         pasted into a shell it would delete a sibling directory:\n{text}"
    );
}

#[test]
fn nested_galaxy_refusal_on_a_spaced_path_prints_no_shell_command() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = spaced_dir(tmp.path());

    let out = cs(&parent).arg("init").output().unwrap();
    assert!(out.status.success());

    let child = parent.join("child");
    fs::create_dir_all(&child).unwrap();
    let out = cs(&child).arg("init").output().unwrap();
    assert!(
        !out.status.success(),
        "expected the nested-galaxy refusal, got success:\n{}",
        rendered(&out)
    );

    let text = rendered(&out);
    assert!(
        !text.contains("rm -rf"),
        "the nested-galaxy refusal handed the user a shell command:\n{text}"
    );
    assert!(
        text.contains(&parent.display().to_string()),
        "the refusal must name the ancestor galaxy it found:\n{text}"
    );
}

#[test]
fn the_output_names_the_exact_directory_that_was_created() {
    let tmp = tempfile::tempdir().unwrap();
    let target = spaced_dir(tmp.path());

    let out = cs(&target).arg("init").output().unwrap();
    assert!(out.status.success());
    let text = rendered(&out);

    let created = target.join(".cosmon");
    assert!(created.is_dir(), "cs init created no .cosmon/");
    assert!(
        text.contains(&created.display().to_string()),
        "the output must name the exact directory it created, so the \
         prose that replaced the command cannot drift into naming the \
         wrong place:\n{text}"
    );
}
