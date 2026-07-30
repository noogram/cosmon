// SPDX-License-Identifier: AGPL-3.0-only

//! A refused root dispatch must leave the galaxy and the config home exactly
//! as it found them — COSMON-DEV #20, reported against v0.4.0 by @jdthaler.
//!
//! # The defect
//!
//! `decide_root_spawn` refuses a root dispatcher, and it refused correctly. But
//! the refusal lived inside `spawn_claude_and_prompt`, seven thousand lines
//! into `cs tackle`, so it preceded the worker session and the cognitive probe
//! and **not** the filesystem provisioning. On a galaxy set up exactly as
//! `docs/guides/cosmon-mission-in-a-container.md` describes, one stray
//! `cs tackle` as root exited 1, spawned nothing, and still left behind
//! root-owned `.claude.json`, `settings.json`, `.worktrees/`, `.git/config`,
//! `.git/packed-refs`, `fleet.json` and `fleet.runtime.json`. The reporter then
//! isolated the consequence one variable at a time: after that single mistake
//! the DOCUMENTED non-root dispatch dies with `mkdir: Permission denied` on
//! `.worktrees/` and the molecule times out `pending`. The refusal that exists
//! to stop root from creating resources a worker must own was creating them.
//!
//! # Why the old test did not see it
//!
//! ADR-166 claimed "a refused dispatch leaves no trace on the filesystem — the
//! test that pins this asserts the worktree's owner is unchanged". The
//! worktree's owner *is* unchanged, because a refused dispatch never gets as
//! far as creating a worktree. What it created was the `.worktrees` PARENT,
//! which that assertion never looked at. The test measured the property next
//! door to the one that mattered.
//!
//! So this file does not assert about a path it names. It snapshots **every**
//! path under the galaxy root and under the Claude config home — with owner,
//! group and mode — runs the refused dispatch, and asserts the two snapshots
//! are identical. A residue nobody predicted fails it, which is the whole
//! difference from the assertion it replaces.
//!
//! # Why this runs as an ordinary user
//!
//! Only root can reproduce the *ownership* half, and every existing test of
//! this area is consequently `#[ignore]`d behind a root check — which is how
//! the defect shipped in a release with a green suite. The property under test
//! is **ordering**, not privilege: does the refusal precede the writes? That is
//! measurable by any uid once the decision's input is injectable, which is what
//! `cosmon_core::root_spawn_policy::effective_dispatch_uid` is for. The
//! injection is monotone — it can only substitute uid 0, and uid 0 is refused
//! unconditionally — so it can never permit a spawn the real uid would forbid.
//!
//! # Evidence it is not vacuous
//!
//! Against `cfa27a9` (the v0.4.0 trunk) this test fails on the residue
//! `cs tackle` leaves before reaching the deep gate. The two guards below —
//! the adapter shim on `PATH` and the assertion that the run really was refused
//! for the root reason — exist because without them it would pass on the broken
//! code for the wrong reason: the missing-prerequisite gate fires *before* the
//! root-spawn decision, so a reproduction run without an adapter on `PATH`
//! measures that gate and creates no residue at all. That is precisely the trap
//! the reporter fell into on his first attempt.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What is recorded for one path: owner, group, mode, and whether it is a dir.
///
/// Everything a root dispatcher can damage on a galaxy it must not touch, and
/// nothing that changes for innocent reasons — mtime is deliberately absent,
/// since a read can update `atime` on some mounts and the claim under test is
/// about creation and ownership.
type Snapshot = BTreeMap<PathBuf, (u32, u32, u32, bool)>;

/// Record every path beneath `root`, keyed relative to it.
///
/// Symlinks are recorded by their own metadata (`symlink_metadata`) and never
/// followed: a link's target may legitimately live outside the tree, and
/// following one would silently move the assertion onto a path this test never
/// claimed anything about.
fn snapshot(root: &Path) -> Snapshot {
    let mut out = Snapshot::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            out.insert(
                rel,
                (
                    meta.uid(),
                    meta.gid(),
                    meta.permissions().mode(),
                    meta.is_dir(),
                ),
            );
            if meta.is_dir() {
                stack.push(path);
            }
        }
    }
    out
}

/// Report the difference between two snapshots as lines a human can act on.
fn diff(before: &Snapshot, after: &Snapshot) -> Vec<String> {
    let mut lines = Vec::new();
    for (path, meta) in after {
        match before.get(path) {
            None => lines.push(format!("CREATED  {}", path.display())),
            Some(was) if was != meta => lines.push(format!(
                "CHANGED  {} (uid/gid/mode was {:?}, now {:?})",
                path.display(),
                was,
                meta,
            )),
            Some(_) => {}
        }
    }
    for path in before.keys() {
        if !after.contains_key(path) {
            lines.push(format!("REMOVED  {}", path.display()));
        }
    }
    lines
}

/// A `cs` invocation with the developer's ambient cosmon session stripped, so
/// the run is hermetic: no inherited worker depth, no molecule dir, no adapter
/// hammer, no simulation flag left over from a previous test.
fn cs(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd);
    for k in [
        "COSMON_PARENT_MOL_ID",
        "COSMON_MOL_DIR",
        "COSMON_DEFAULT_ADAPTER",
        "COSMON_SIMULATE_ROOT_DISPATCH",
        "COSMON_WORKER_UID",
        "CB_SESSION_ROLE",
        "CB_DEPTH",
        "ANTHROPIC_MODEL",
    ] {
        cmd.env_remove(k);
    }
    cmd
}

/// Write an executable no-op shim named `name` into `dir`.
///
/// The adapter shim is what gets the reproduction past the missing-prerequisite
/// gate — see the module header. It is never executed on the path under test:
/// the refusal now precedes everything, and on the broken code the run dies of
/// its own accord well before a worker exists.
fn shim(dir: &Path, name: &str) {
    let path = dir.join(name);
    fs::write(&path, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Pull the molecule id out of `cs nucleate --json`.
fn molecule_id(stdout: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(stdout)
        .expect("`cs nucleate --json` must emit json")
        .get("id")
        .and_then(|v| v.as_str())
        .expect("`cs nucleate --json` must carry the molecule id")
        .to_owned()
}

/// Whether the galaxy's `events.jsonl` carries a typed root-spawn refusal.
///
/// The machine-readable contract, checked rather than the prose: the container
/// repro harness keys on `type == "tackle_refused"` and a `reason` starting
/// `root-spawn-refused:`, and a remedy sentence can be reworded without the
/// token moving.
fn recorded_root_refusal(galaxy: &Path) -> bool {
    let Ok(events) = fs::read_to_string(galaxy.join(".cosmon/state/events.jsonl")) else {
        return false;
    };
    events.lines().any(|l| {
        serde_json::from_str::<serde_json::Value>(l).is_ok_and(|v| {
            v.get("type").and_then(|t| t.as_str()) == Some("tackle_refused")
                && v.get("reason")
                    .and_then(|r| r.as_str())
                    .is_some_and(|r| r.starts_with("root-spawn-refused:"))
        })
    })
}

/// The load-bearing assertion: a refused root dispatch **adds no path and
/// changes no ownership or mode** under the galaxy and the config home the
/// container guide tells an operator to use.
///
/// Stated as what is measured, not as something stronger. This snapshots the
/// path set with owner, group and mode — it does **not** compare file contents,
/// and the galaxy is deliberately not byte-identical afterwards: the refusal
/// appends its typed record to the fleet ledger, which is the one write the
/// refusal is allowed to make and which `refusal_recorded` checks for
/// separately. A name promising byte-identity would be a stronger claim than
/// the body carries, which is the defect class this whole test exists to close.
#[test]
fn a_refused_root_dispatch_adds_no_path_and_changes_no_ownership_or_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let galaxy = tmp.path().join("galaxy");
    let config_home = tmp.path().join("claude-config");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&galaxy).unwrap();
    fs::create_dir_all(&config_home).unwrap();
    fs::create_dir_all(&bin).unwrap();
    // Both tools `cs tackle`'s runtime-prerequisite gate demands for a
    // tmux-backed claude dispatch. Without them that gate fires *before* the
    // root-spawn decision and the run creates nothing — a green test measuring
    // the wrong refusal.
    shim(&bin, "claude");
    shim(&bin, "tmux");

    // A real repository, because `.worktrees/`, `.git/config` and
    // `.git/packed-refs` are three of the seven residues reported.
    for args in [
        vec!["init", "-q", "-b", "main", "."],
        vec!["config", "user.email", "test@noogram.org"],
        vec!["config", "user.name", "Noogram Test"],
        vec!["commit", "-q", "--allow-empty", "-m", "root"],
    ] {
        let ok = Command::new("git")
            .args(&args)
            .current_dir(&galaxy)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed");
    }

    assert!(cs(&galaxy).arg("init").status().unwrap().success());

    let nucleate = cs(&galaxy)
        .args([
            "--json",
            "nucleate",
            "task-work",
            "--kind",
            "task",
            "--var",
            "topic=a dispatch nobody is allowed to make",
        ])
        .output()
        .unwrap();
    assert!(
        nucleate.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&nucleate.stderr),
    );
    let mol_id = molecule_id(&nucleate.stdout);

    // The two trees the guide names, snapshotted whole.
    let galaxy_before = snapshot(&galaxy);
    let config_before = snapshot(&config_home);

    let refused = cs(&galaxy)
        // `--adapter claude` is the route ADR-165/166 recommend and the one
        // the reporter used. It matters here for a second reason: the default
        // `local` adapter refuses on its own model preflight, which would keep
        // the filesystem clean for a reason that has nothing to do with the
        // property under test.
        .args(["tackle", &mol_id, "--adapter", "claude"])
        .env("COSMON_SIMULATE_ROOT_DISPATCH", "1")
        .env("CLAUDE_CONFIG_DIR", &config_home)
        .env("COSMON_CONFIG_HOME", tmp.path().join("cosmon-config-home"))
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&refused.stderr).into_owned();

    // The property ADR-166 claims, asserted FIRST so it is the failure a
    // bisect reads. Against the un-hoisted gate this is what goes red, and it
    // names every path that should not exist.
    let galaxy_changes = diff(&galaxy_before, &snapshot(&galaxy));
    let config_changes = diff(&config_before, &snapshot(&config_home));
    assert!(
        galaxy_changes.is_empty() && config_changes.is_empty(),
        "a refused root dispatch left residue.\n\
         galaxy ({} entries):\n  {}\n\
         config home ({} entries):\n  {}\n\
         stderr: {stderr}",
        galaxy_changes.len(),
        galaxy_changes.join("\n  "),
        config_changes.len(),
        config_changes.join("\n  "),
    );

    // Guard against a vacuous pass, checked second because a clean filesystem
    // is exactly what an earlier gate also produces. The run must have been
    // refused, and refused for the ROOT reason — not for a missing adapter, a
    // missing tmux, an absent credential, or any other gate that would keep the
    // tree clean for a reason this test is not about.
    assert!(
        !refused.status.success(),
        "a root dispatch must be refused; it exited 0.\nstderr: {stderr}",
    );
    assert!(
        recorded_root_refusal(&galaxy),
        "the refusal must be the root-spawn one, not another gate that keeps \
         the tree clean for an unrelated reason.\nstderr: {stderr}",
    );
}

/// The refusal is recorded where an audit reads it, and carries the typed token
/// the container repro harness keys on.
///
/// Stated separately from the residue assertion because the two pull in
/// opposite directions: recording the refusal is itself a write, and the only
/// reason it is not residue is that the sinks are opened append-only. If a
/// future edit reaches for `create(true)` to make this test greener, the test
/// above goes red — which is the interlock, not a coincidence.
#[test]
fn the_refusal_is_recorded_with_its_typed_token() {
    let tmp = tempfile::tempdir().unwrap();
    let galaxy = tmp.path().join("galaxy");
    fs::create_dir_all(&galaxy).unwrap();

    assert!(cs(&galaxy).arg("init").status().unwrap().success());
    let nucleate = cs(&galaxy)
        .args([
            "--json",
            "nucleate",
            "task-work",
            "--kind",
            "task",
            "--var",
            "topic=a dispatch nobody is allowed to make",
        ])
        .output()
        .unwrap();
    assert!(nucleate.status.success());
    let mol_id = molecule_id(&nucleate.stdout);

    let refused = cs(&galaxy)
        .args(["tackle", &mol_id, "--adapter", "claude"])
        .env("COSMON_SIMULATE_ROOT_DISPATCH", "1")
        .output()
        .unwrap();
    assert!(!refused.status.success());

    let events = fs::read_to_string(galaxy.join(".cosmon/state/events.jsonl"))
        .expect("the galaxy's events.jsonl must exist after a nucleate");
    assert!(
        recorded_root_refusal(&galaxy),
        "the typed refusal must reach events.jsonl — the container repro \
         harness keys on `type == \"tackle_refused\"`.\nevents.jsonl:\n{events}",
    );
}
