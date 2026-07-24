// SPDX-License-Identifier: AGPL-3.0-only

//! **Real** containment of a germination's per-node output homes (ADR-161).
//!
//! # The gap this closes (COSMON-DEV #21 defect B2, iteration 2)
//!
//! [`node_output_dir`](cosmon_core::spore::node_output_dir) and the
//! `forbidden_gate_output` detector wired in `cs spore run` are **lexical**: they
//! do path arithmetic on strings, deliberately, so the zero-I/O core keeps its
//! property. That closes every *string* attack (an absolute alias, a `..`, a
//! two-component alias, a Unicode dot lookalike) — and closes none of the
//! *filesystem* ones.
//!
//! An adversarial reviewer reproduced the survivor: once the run home exists, a
//! symlink
//!
//! ```text
//! <run_home>/intake -> /tmp/outside
//! ```
//!
//! makes the lexically-inside path `<run_home>/intake/verdict.md` **resolve** to
//! `/tmp/outside/verdict.md`. Every lexical guard says "inside"; `realpath` says
//! "outside"; the worker writes outside. Run isolation — the entire point of the
//! home — is gone, and the write lands wherever the symlink points, including
//! the tracked tree the convention exists to protect.
//!
//! # What this module does instead
//!
//! It *creates* each node's output directory itself, with no-follow semantics,
//! and then *canonicalizes* what it created and asserts the real path is strictly
//! under the real run home:
//!
//! - [`std::fs::create_dir`] (not `create_dir_all`) never follows a final
//!   symlink: on a planted symlink it fails `AlreadyExists`, and the guard then
//!   inspects the entry with [`std::fs::symlink_metadata`] — which does not
//!   follow either — and refuses on a symlink outright, and refuses anything
//!   that is not a directory (a pre-planted regular file would otherwise be
//!   accepted and wedge the worker on its first write);
//! - the surviving directory is [`std::fs::canonicalize`]d and compared against
//!   the canonicalized run home, so an escape through the last component is
//!   caught;
//! - the **run home itself** is canonicalized and required to live under the
//!   canonicalized state root, so a symlink planted on a *parent* component —
//!   `<state>/spore-runs -> /outside`, a fixed name needing no id guessing —
//!   cannot silently relocate every node home. `create_dir_all` follows symlinks
//!   on every component, so without this the run home is a *relocated reference
//!   frame* and every per-node check passes against it (COSMON-DEV #20 defect
//!   ND2, iteration 4);
//! - two aliases that resolve to the **same** directory are refused. On a
//!   case-insensitive filesystem (APFS, i.e. every macOS dev box) `intake` and
//!   `Intake` name one directory, and `create_dir`'s benign `AlreadyExists` would
//!   otherwise read as idempotence rather than as the `NoResourceCollision`
//!   violation it is.
//!
//! Defence in depth: the lexical grammar upstream stays exactly as it is. This is
//! the second, filesystem-aware line, and it is the one that runs immediately
//! before a worker is handed the path.
//!
//! # What it does not claim
//!
//! A residual TOCTOU window remains: an attacker who can write inside the run
//! home *after* this pass could still swap a verified directory for a symlink
//! before the worker writes into it. Closing that needs `openat`/`O_NOFOLLOW`
//! handles held open across the germination, which the current germinate seam
//! does not thread. What this pass does close is the reproduced attack — a link
//! planted *before* germination, which every lexical guard accepted — and it
//! narrows the window from "the whole run" to "between provisioning and the
//! worker's first write". Stated plainly rather than implied away.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Why a node's output home is refused after the filesystem has been consulted.
///
/// Distinct from the lexical
/// [`ForbiddenOutput`](cosmon_core::spore::ForbiddenOutput): these are verdicts
/// only `realpath`/`lstat` can reach, so they name the *filesystem* fault rather
/// than a path-arithmetic one.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ContainmentBreach {
    /// The entry at the node's output path is a symlink. Refused without even
    /// asking where it points: a run home whose children are links is not a run
    /// home, and following it to decide would be a TOCTOU race.
    #[error(
        "node \"{alias}\" output home {} is a symlink; a germination output home must be a real \
         directory inside the run home (ADR-161)", path.display()
    )]
    Symlink {
        /// The offending germination alias.
        alias: String,
        /// The path that turned out to be a symlink.
        path: PathBuf,
    },
    /// The node's output path resolves outside the run home. This is the
    /// reproduced escape: lexically inside, really outside.
    #[error(
        "node \"{alias}\" output home resolves to {} which is outside the run home {} \
         (escaped-run-home, ADR-161)", resolved.display(), run_home.display()
    )]
    EscapedRunHome {
        /// The offending germination alias.
        alias: String,
        /// Where the path really resolves to.
        resolved: PathBuf,
        /// The canonicalized run home it had to stay under.
        run_home: PathBuf,
    },
    /// The entry at the node's output path exists but is not a directory — a
    /// pre-planted regular file, a fifo, a device node, or a hard link to a file
    /// outside the run home.
    ///
    /// Not an escape (the worker's writes into it fail), but without this check
    /// `create_dir` returns `AlreadyExists`, `lstat` says "not a symlink" and
    /// `canonicalize` says "inside", so the germination is **accepted** and the
    /// worker wedges on its first write. A loud refusal is the whole point.
    #[error(
        "node \"{alias}\" output home {} exists but is not a directory; a germination output \
         home must be a real directory inside the run home (ADR-161)", path.display()
    )]
    NotADirectory {
        /// The offending germination alias.
        alias: String,
        /// The path occupied by a non-directory.
        path: PathBuf,
    },
    /// Two distinct germination aliases resolve to the *same* directory.
    ///
    /// Reachable without any adversary on a case-insensitive filesystem: node
    /// ids `intake` and `Intake` both pass the lexical grammar and both provision
    /// `<run_home>/intake`. Sharing one output home between two nodes is a
    /// `NoResourceCollision` violation, and `create_dir`'s `AlreadyExists` cannot
    /// tell it apart from benign idempotence — only comparing canonical paths can.
    #[error(
        "node \"{alias}\" output home resolves to {}, already claimed by node \"{other}\"; \
         two germination nodes must not share one output home (NoResourceCollision, ADR-161)",
        resolved.display()
    )]
    AliasCollision {
        /// The alias that arrived second.
        alias: String,
        /// The alias that already owns the directory.
        other: String,
        /// The single directory both aliases resolve to.
        resolved: PathBuf,
    },
    /// The **run home** itself resolves outside the state store, so the reference
    /// frame every per-node check is measured against is already relocated.
    ///
    /// Planted as a symlink on any component at or above the run home —
    /// `<state>/spore-runs -> /outside` needs no germination-id entropy to guess.
    /// `create_dir_all` follows symlinks on every component, so the run home is
    /// minted outside and each node home then passes "inside the run home"
    /// truthfully while every ADR-161 output lands outside the state store
    /// (COSMON-DEV #20 defect ND2, iteration 4).
    #[error(
        "run home {} resolves outside the state store {} (escaped-state-root, ADR-161); \
         a symlinked component at or above the run home relocates every node output",
        run_home.display(), state_root.display()
    )]
    EscapedStateRoot {
        /// Where the run home really resolves to.
        run_home: PathBuf,
        /// The canonicalized state store it had to stay under.
        state_root: PathBuf,
    },
    /// The filesystem refused the operation for an ordinary reason (permissions,
    /// a missing run home, a non-directory in the way). Surfaced rather than
    /// swallowed: a germination whose homes cannot be created must not proceed.
    #[error("node \"{alias}\" output home {} could not be provisioned: {source}", path.display())]
    Io {
        /// The offending germination alias.
        alias: String,
        /// The path being provisioned.
        path: PathBuf,
        /// The underlying io error.
        #[source]
        source: std::sync::Arc<std::io::Error>,
    },
}

/// Create every node's output directory under `run_home` and prove — against the
/// filesystem, not against the string — that each one really lives inside it,
/// and that the run home itself really lives inside `state_root`.
///
/// `nodes` pairs each germination alias with the output path the lexical pass
/// already composed for it. The lexical pass stays authoritative for *shape*;
/// this pass is authoritative for *reality*.
///
/// `state_root` is the state store the run home must stay under (`.cosmon/state`).
/// It is checked *first*: the run home is the reference frame for every per-node
/// comparison, and an unchecked frame can itself have been relocated by a symlink
/// on a parent component (defect ND2). Checking the frame is what makes the
/// per-node verdicts mean what they say.
///
/// All-or-nothing by construction: the first breach returns, and the caller
/// refuses the germination as a whole. A partially-provisioned run is never
/// handed to a worker.
///
/// # Errors
///
/// Returns a [`ContainmentBreach`] when the run home escapes the state root, or
/// when a node's home is a symlink, is not a directory, collides with another
/// node's home, resolves outside the canonicalized run home, or cannot be
/// provisioned at all.
pub fn provision_contained_node_dirs(
    state_root: &Path,
    run_home: &Path,
    nodes: &[(String, PathBuf)],
) -> Result<(), ContainmentBreach> {
    let io = |alias: &str, path: &Path, e: std::io::Error| ContainmentBreach::Io {
        alias: alias.to_owned(),
        path: path.to_path_buf(),
        source: std::sync::Arc::new(e),
    };

    // The run home itself is the reference frame; canonicalize it once so a
    // symlinked *ancestor* (e.g. macOS `/tmp -> /private/tmp`) is not mistaken
    // for an escape.
    let real_run_home = run_home
        .canonicalize()
        .map_err(|e| io("<run home>", run_home, e))?;

    // ...and prove the frame itself is where it claims to be. Without this the
    // module's own guarantee is empty: a symlink at `<state>/spore-runs` moves
    // the whole home and every node then passes "inside the run home".
    let real_state_root = state_root
        .canonicalize()
        .map_err(|e| io("<state root>", state_root, e))?;
    if !real_run_home.starts_with(&real_state_root) {
        return Err(ContainmentBreach::EscapedStateRoot {
            run_home: real_run_home,
            state_root: real_state_root,
        });
    }

    // Canonical home -> the alias that claimed it, so two ids that name one
    // directory (case-insensitive filesystems) are a refusal, not idempotence.
    let mut claimed: HashMap<PathBuf, String> = HashMap::new();

    for (alias, path) in nodes {
        // `create_dir` does not follow a final symlink and does not create
        // parents: a planted link fails here rather than being written through.
        match std::fs::create_dir(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io(alias, path, e)),
        }

        // `symlink_metadata` is `lstat`: it describes the entry, not its target.
        let entry = std::fs::symlink_metadata(path).map_err(|e| io(alias, path, e))?;
        if entry.file_type().is_symlink() {
            return Err(ContainmentBreach::Symlink {
                alias: alias.clone(),
                path: path.clone(),
            });
        }
        // `AlreadyExists` above is only benign when what already exists is a
        // directory. A pre-planted regular file passes both the lstat and the
        // canonicalize check and turns a refusal into a wedged worker.
        if !entry.file_type().is_dir() {
            return Err(ContainmentBreach::NotADirectory {
                alias: alias.clone(),
                path: path.clone(),
            });
        }

        let resolved = path.canonicalize().map_err(|e| io(alias, path, e))?;
        if resolved == real_run_home || !resolved.starts_with(&real_run_home) {
            return Err(ContainmentBreach::EscapedRunHome {
                alias: alias.clone(),
                resolved,
                run_home: real_run_home.clone(),
            });
        }
        if let Some(other) = claimed.insert(resolved.clone(), alias.clone()) {
            if &other != alias {
                return Err(ContainmentBreach::AliasCollision {
                    alias: alias.clone(),
                    other,
                    resolved,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_root(tmp: &Path) -> PathBuf {
        let root = tmp.join("state");
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn run_home(tmp: &Path) -> PathBuf {
        let home = state_root(tmp).join("spore-runs/germ-1");
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    /// COSMON-DEV #21 defect B2, iteration 2 — the reviewer's reproduced escape,
    /// frozen as a red-first regression.
    ///
    /// The alias `intake` passes every lexical guard: one ordinary component, no
    /// `..`, no absolute prefix, and `<run_home>/intake` starts with the run home
    /// as a string. But the entry planted there is a symlink to a directory
    /// outside the run home, so `<run_home>/intake/verdict.md` *resolves* outside
    /// it. A lexical-only guard accepts this; the filesystem-aware guard must
    /// refuse.
    #[test]
    fn a_symlinked_node_home_pointing_outside_the_run_home_is_refused() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = run_home(tmp.path());
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        let node = home.join("intake");
        std::os::unix::fs::symlink(&outside, &node).unwrap();

        // Sanity: the attack is real — the path the worker would be handed
        // really does resolve outside the run home.
        assert!(
            !node
                .join("verdict.md")
                .parent()
                .unwrap()
                .canonicalize()
                .unwrap()
                .starts_with(home.canonicalize().unwrap()),
            "precondition: the planted symlink must actually escape",
        );

        let breach = provision_contained_node_dirs(
            &state_root(tmp.path()),
            &home,
            &[("intake".to_owned(), node.clone())],
        )
        .expect_err("a symlinked node home that escapes the run home must be refused");
        match breach {
            ContainmentBreach::Symlink { ref alias, .. } => assert_eq!(alias, "intake"),
            ContainmentBreach::EscapedRunHome { ref alias, .. } => assert_eq!(alias, "intake"),
            other => panic!("expected a containment breach, got {other:?}"),
        }
    }

    /// The same escape planted one level up: the *run home itself* is fine, but
    /// a node home is a symlink to a sibling run's directory. Still outside this
    /// run — `NoResourceCollision` is exactly what is being protected.
    #[test]
    fn a_node_home_symlinked_to_another_run_is_refused() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = run_home(tmp.path());
        let other = tmp.path().join("state/spore-runs/germ-2/intake");
        std::fs::create_dir_all(&other).unwrap();

        let node = home.join("intake");
        std::os::unix::fs::symlink(&other, &node).unwrap();

        assert!(
            provision_contained_node_dirs(
                &state_root(tmp.path()),
                &home,
                &[("intake".to_owned(), node)]
            )
            .is_err(),
            "a node home aliasing another germination's output must be refused",
        );
    }

    /// A refusal is all-or-nothing at the germination level: the caller aborts,
    /// so a benign sibling listed *after* the hostile one is never handed a home
    /// the run then proceeds on.
    #[test]
    fn one_breach_refuses_the_whole_germination() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = run_home(tmp.path());
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let hostile = home.join("intake");
        std::os::unix::fs::symlink(&outside, &hostile).unwrap();

        let green = home.join("green");
        let err = provision_contained_node_dirs(
            &state_root(tmp.path()),
            &home,
            &[
                ("intake".to_owned(), hostile),
                ("green".to_owned(), green.clone()),
            ],
        );
        assert!(err.is_err());
        assert!(
            !green.exists(),
            "a refused germination must not have provisioned the surviving nodes"
        );
    }

    /// The benign path the convention actually produces still works, and is
    /// idempotent — a second germination pass over an already-created home is
    /// not an error.
    #[test]
    fn real_directories_under_the_run_home_are_provisioned_and_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = run_home(tmp.path());
        let nodes = vec![
            ("intake".to_owned(), home.join("intake")),
            ("green".to_owned(), home.join("green")),
        ];
        let root = state_root(tmp.path());
        provision_contained_node_dirs(&root, &home, &nodes)
            .expect("benign nodes must be provisioned");
        for (_, p) in &nodes {
            assert!(p.is_dir(), "{} must exist as a real directory", p.display());
        }
        provision_contained_node_dirs(&root, &home, &nodes).expect("provisioning is idempotent");
    }

    /// A run home that does not exist is a refusal, not a silent `create_dir_all`
    /// — the seal gate owns when the run home comes into being (defect B3), and
    /// this guard must never pre-empt it.
    #[test]
    fn a_missing_run_home_is_refused_not_created() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = state_root(tmp.path()).join("spore-runs/germ-never");
        let err = provision_contained_node_dirs(
            &state_root(tmp.path()),
            &home,
            &[("intake".to_owned(), home.join("intake"))],
        )
        .expect_err("a missing run home must refuse");
        assert!(matches!(err, ContainmentBreach::Io { .. }));
        assert!(!home.exists(), "the guard must not create the run home");
    }

    /// COSMON-DEV #20 defect ND2(a), iteration 4 — the reviewer's reproduced
    /// **parent-component** escape, frozen red-first.
    ///
    /// The attacker never touches a node home and never guesses a germination id.
    /// They plant one symlink at the fixed name `<state>/spore-runs`, and the
    /// ordinary `create_dir_all(run_dir)` the shell already performs mints the run
    /// home *outside the state store*. Every per-node check then passes —
    /// truthfully, against a relocated frame — while every ADR-161 output lands
    /// where F9 says it must never land. Only checking the frame catches it.
    #[test]
    fn a_symlinked_parent_component_relocating_the_run_home_is_refused() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = state_root(tmp.path());
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        // The whole attack: one link on a predictable, non-entropic component.
        std::os::unix::fs::symlink(&outside, root.join("spore-runs")).unwrap();
        let home = root.join("spore-runs/germ-1");
        // `create_dir_all` follows symlinks on every component — this is what
        // `cs spore run` does, unmodified.
        std::fs::create_dir_all(&home).unwrap();

        // Sanity: the run home really is outside the state store now.
        assert!(
            home.canonicalize()
                .unwrap()
                .starts_with(outside.canonicalize().unwrap()),
            "precondition: the planted parent symlink must actually relocate the run home",
        );

        let err = provision_contained_node_dirs(
            &root,
            &home,
            &[("intake".to_owned(), home.join("intake"))],
        )
        .expect_err("a run home relocated outside the state store must be refused");
        assert!(
            matches!(err, ContainmentBreach::EscapedStateRoot { .. }),
            "expected an escaped-state-root refusal, got {err:?}"
        );
    }

    /// COSMON-DEV #20 defect ND2(b) — a pre-planted **regular file** at the node
    /// alias must be a loud refusal, not an accepted home the worker then wedges
    /// on. `create_dir` says `AlreadyExists`, `lstat` says "not a symlink" and
    /// `canonicalize` says "inside"; only the file-type check refuses.
    #[test]
    fn a_regular_file_planted_at_a_node_home_is_refused() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = run_home(tmp.path());
        let node = home.join("intake");
        std::fs::write(&node, b"not a directory").unwrap();

        let err = provision_contained_node_dirs(
            &state_root(tmp.path()),
            &home,
            &[("intake".to_owned(), node)],
        )
        .expect_err("a non-directory at a node home must be refused");
        assert!(
            matches!(err, ContainmentBreach::NotADirectory { .. }),
            "expected a not-a-directory refusal, got {err:?}"
        );
    }

    /// COSMON-DEV #20 defect ND2(c) — two aliases that resolve to one directory
    /// are a `NoResourceCollision` violation, not idempotence.
    ///
    /// Written with two aliases pointing at the *same* path so it pins the
    /// invariant on every filesystem; on a case-insensitive one (APFS) the ids
    /// `intake` / `Intake` reach it without any adversary at all.
    #[test]
    fn two_aliases_sharing_one_output_home_are_refused() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = run_home(tmp.path());
        let shared = home.join("intake");

        let err = provision_contained_node_dirs(
            &state_root(tmp.path()),
            &home,
            &[
                ("intake".to_owned(), shared.clone()),
                ("Intake".to_owned(), shared),
            ],
        )
        .expect_err("two nodes sharing one output home must be refused");
        match err {
            ContainmentBreach::AliasCollision {
                ref alias,
                ref other,
                ..
            } => {
                assert_eq!(alias, "Intake");
                assert_eq!(other, "intake");
            }
            other => panic!("expected an alias collision, got {other:?}"),
        }
    }
}
