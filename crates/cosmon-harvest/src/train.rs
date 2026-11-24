// SPDX-License-Identifier: AGPL-3.0-only

//! Train landing: the one point where an operator-built train moves the base
//! branch (issue #213, ADR-190 D3, increment D1).
//!
//! # What a train is, and whose it is
//!
//! An operator tool builds a train off-trunk: one `git merge --no-ff` per
//! completed member, each carrying exactly the message `cs done
//! --print-merge-message` prints for that member, merging the member's pinned
//! branch head. It gates the train once. Building and gating stay
//! operator-side (ADR-190 §6). Moving the base does not: an operator script
//! that fast-forwards `main` itself bypasses the trunk lock, the harvest
//! authority check and the shell-surface predicate. [`land`] is that move.
//!
//! # The two phases, and why the lock covers only the first
//!
//! [`land`] takes the trunk lock once, runs every landing check, moves the
//! base by a compare-and-swap fast-forward, and releases the lock before it
//! returns. Closing each member — `cs done <id> --expect-head <head>`, which
//! stays the only writer of `merged_at` for train members (ADR-190 §3
//! "Closure") — is the caller's second phase. It must run after the lock is
//! gone: `cs done` takes the trunk lock itself, and `flock` taken through a
//! separate open file description does not nest, so a `cs done` child under a
//! parent still holding trunk would block forever (ADR-190 §3 "Lock order").
//!
//! # Rerun after a kill
//!
//! A kill between the fast-forward and the closures leaves the base at the
//! train tip with some members unclosed — or past it, because a closure in a
//! galaxy that tracks harvest state commits that state to the base, and the
//! released lock lets another pilot land in between. A rerun with the same
//! arguments finds the tip on the base's first-parent chain, re-checks the
//! train's shape, skips the move
//! and the shell-surface predicate (the base already moved, so refusing it
//! protects nothing), and hands back only the members whose `merged_at` is
//! still empty. A member is therefore never closed twice by this verb.
//!
//! # The decision is pure
//!
//! The shape and carrier rules are functions of plain data. The surrounding
//! layer reads the train chain and ancestry from Git.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::StateStore;

use crate::paths::{require_project_identity, resolve_config_from_context};
use crate::HarvestContext;

/// One member of a train as the operator names it: the molecule and the
/// branch head the train merged and the gate verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainMember {
    /// The member molecule.
    pub molecule: MoleculeId,
    /// The pinned head, as given (a full or unambiguous abbreviated id).
    pub head: String,
}

impl TrainMember {
    /// Parse the `<mol_id>=<head>` spelling of `cs land-train --member`.
    ///
    /// # Errors
    ///
    /// When the text has no `=`, either side is empty, or the molecule id is
    /// not a valid id.
    pub fn parse(spec: &str) -> anyhow::Result<Self> {
        let (molecule, head) = parse_pin(spec, "--member")?;
        Ok(Self { molecule, head })
    }
}

/// A removed molecule and the pinned head used to find its first own commit.
#[derive(Debug, Clone)]
pub struct ExcludedHead {
    molecule: MoleculeId,
    head: String,
}

impl ExcludedHead {
    /// Parse the `<mol_id>=<head>` spelling of `cs land-train --exclude`.
    ///
    /// # Errors
    ///
    /// When the text has no `=`, either side is empty, or the molecule id is
    /// invalid.
    pub fn parse(spec: &str) -> anyhow::Result<Self> {
        let (molecule, head) = parse_pin(spec, "--exclude")?;
        Ok(Self { molecule, head })
    }
}

fn parse_pin(spec: &str, flag: &str) -> anyhow::Result<(MoleculeId, String)> {
    let invalid =
        || anyhow::anyhow!("{flag} `{spec}`: expected `<molecule id>=<pinned branch head>`");
    let (mol, head) = spec.split_once('=').ok_or_else(invalid)?;
    let (mol, head) = (mol.trim(), head.trim());
    if mol.is_empty() || head.is_empty() {
        return Err(invalid());
    }
    Ok((MoleculeId::new(mol)?, head.to_owned()))
}

/// What the operator asks [`land`] to do.
#[derive(Debug, Clone)]
pub struct LandTrainRequest {
    /// The base-branch commit the train was built on and gated against.
    pub train_base: String,
    /// The train's tip: the commit the base branch fast-forwards to.
    pub train_tip: String,
    /// The members, in the order their merges sit on the train's
    /// first-parent chain (oldest first).
    pub members: Vec<TrainMember>,
    /// Removed members' pinned heads. The train's members must exclude the
    /// first own commit of each such head from their ancestry.
    pub exclusions: Vec<ExcludedHead>,
}

/// Whether [`land`] moved the base or found it already moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    /// The base branch was fast-forwarded from the train base to the tip.
    Landed,
    /// The tip was already on the base branch's first-parent chain (a rerun
    /// after a kill between the fast-forward and the closures, possibly with
    /// closures or other landings since); nothing was moved.
    AlreadyLanded,
}

/// A member whose closure is still owed, with its head resolved to a full id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingClosure {
    /// The member molecule.
    pub molecule: MoleculeId,
    /// The pinned head, as a full object id: what `--expect-head` must name.
    pub head: String,
}

/// The result of the locked phase. The trunk lock is already released.
#[derive(Debug, Clone)]
pub struct LandingOutcome {
    /// The base branch the train landed on.
    pub base_branch: String,
    /// The train base, as a full object id.
    pub train_base: String,
    /// The train tip, as a full object id.
    pub train_tip: String,
    /// Whether this invocation moved the base.
    pub landing: Landing,
    /// Members to close, in order, with `cs done --expect-head <head>`.
    pub to_close: Vec<PendingClosure>,
    /// Members already closed by an earlier run; they are not closed again.
    pub already_closed: Vec<MoleculeId>,
    /// Attribution warnings from rendering the members' merge messages.
    pub warnings: Vec<String>,
}

/// One first-parent commit of a landing range, as the pure check reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainCommit {
    /// The commit's object id.
    pub oid: String,
    /// Its parents, first parent first.
    pub parents: Vec<String>,
    /// Its raw message, as git stores it.
    pub message: String,
}

/// What one member merge must look like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedMerge {
    /// The member molecule.
    pub molecule: MoleculeId,
    /// Its pinned head, a full object id: the merge's second parent.
    pub head: String,
    /// The message `cs done --print-merge-message` renders for it.
    pub message: String,
}

/// Decide whether `chain` — the first-parent commits of `train_base..tip`,
/// oldest first — is exactly one `--no-ff` member merge per expected member,
/// in order, on top of `train_base`.
///
/// Each commit must have exactly two parents; its first parent must be the
/// train base (for the oldest) or the previous commit, so the chain reaches
/// the base by first parents alone; its second parent must be the member's
/// pinned head; and its message must be the member's rendered message (see
/// [`merge_message_matches`]). A fast-forwarded member, an operator commit and
/// a train-level merge each break one of these and are named by id.
///
/// # Errors
///
/// A sentence naming the first offending commit or the member count mismatch.
pub fn check_train_chain(
    train_base: &str,
    chain: &[ChainCommit],
    expected: &[ExpectedMerge],
) -> Result<(), String> {
    let mut previous = train_base;
    for (i, commit) in chain.iter().enumerate() {
        let first = commit.parents.first().map(String::as_str);
        if first != Some(previous) {
            return Err(format!(
                "commit {} on the train's first-parent chain does not have {previous} as its \
                 first parent, so the train tip does not reach the train base by first-parent \
                 merges alone",
                commit.oid
            ));
        }
        if commit.parents.len() != 2 {
            return Err(format!(
                "commit {} on the train's first-parent chain is not a member merge: it has {} \
                 parent(s), and a member lands only through a `git merge --no-ff` of its pinned \
                 head. A fast-forwarded member, an operator commit and a squash all look like \
                 this; rebuild the train with one `--no-ff` merge per member",
                commit.oid,
                commit.parents.len()
            ));
        }
        let Some(member) = expected.get(i) else {
            return Err(format!(
                "commit {} is merge number {} on the train's first-parent chain, but only {} \
                 member(s) were named; every first-parent commit must be a named member's merge",
                commit.oid,
                i + 1,
                expected.len()
            ));
        };
        if commit.parents[1] != member.head {
            return Err(format!(
                "commit {} is in the position of member {}, but its second parent is {} and the \
                 member's pinned head is {}. Name the members in the order the train merged them, \
                 with the heads the train merged",
                commit.oid, member.molecule, commit.parents[1], member.head
            ));
        }
        if !merge_message_matches(&commit.message, &member.message) {
            return Err(format!(
                "commit {} merges member {} but its message is not the one `cs done` would \
                 write. Build each member merge with `git merge --no-ff -F <file>` where the \
                 file holds the output of `cs done {} --print-merge-message`.\n  expected:\n{}\n  \
                 found:\n{}",
                commit.oid,
                member.molecule,
                member.molecule,
                indent(&member.message),
                indent(&commit.message)
            ));
        }
        previous = &commit.oid;
    }
    if chain.len() != expected.len() {
        return Err(format!(
            "the train holds {} member merge(s) on its first-parent chain but {} member(s) were \
             named; a named member with no merge in the train would be merged by its own \
             `cs done`, outside the train",
            chain.len(),
            expected.len()
        ));
    }
    Ok(())
}

/// Whether `actual` is the rendered message `expected`, allowing only trailer
/// lines appended after it.
///
/// `cs done --print-merge-message` prints the text *before* any
/// `prepare-commit-msg`, `commit-msg` or sign-off hook runs, and such a hook
/// runs for `cs done`'s own merge as well — this repository's
/// `prepare-commit-msg` appends `Signed-off-by:` after a blank line. So the
/// rendered text must be a byte-exact prefix of the stored message, and every
/// non-blank line after it must be a trailer line (`Token: value`): blank
/// lines are tolerated, prose is not.
#[must_use]
pub fn merge_message_matches(actual: &str, expected: &str) -> bool {
    let expected = expected.trim_end_matches('\n');
    let Some(rest) = actual.strip_prefix(expected) else {
        return false;
    };
    let rest = rest.trim_end_matches('\n');
    if rest.is_empty() {
        return true;
    }
    let Some(rest) = rest.strip_prefix('\n') else {
        return false;
    };
    rest.lines()
        .filter(|l| !l.trim().is_empty())
        .all(is_trailer_line)
}

fn is_trailer_line(line: &str) -> bool {
    let Some((token, value)) = line.split_once(": ") else {
        return false;
    };
    !token.is_empty()
        && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !value.trim().is_empty()
}

fn indent(text: &str) -> String {
    text.trim_end_matches('\n')
        .lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run every landing check under the trunk lock and, when they all pass, move
/// the base branch to the train tip by a compare-and-swap fast-forward. The
/// lock is released before this returns; closing the members is the caller's.
///
/// Checks, all before the base moves (any failure moves nothing and names the
/// offending commit or member):
///
/// 1. `[harvest_authority] required` is false — this increment refuses a
///    train when sealed authority is required.
/// 2. The base branch equals the train base (else "moved after the gate"), or
///    already has the tip on its first-parent chain (a rerun: no move, no
///    surface check).
/// 3. [`check_train_chain`] over `train_base..tip`.
/// 4. Every member is Completed, has no other base branch, and — unless
///    already closed — its branch is still at its pinned head.
/// 5. Each excluded ID is absent from the member list. Its head resolves to
///    a commit outside the ancestry of every pinned member head.
/// 6. The shell-surface predicate over `train_base..tip` (ADR-190 §7): a
///    train that changes a trusted-shell-surface path lands only when the
///    repository's grant already covers the surface the landing leaves on
///    disk. This function never grants trust.
/// 7. When the base is checked out in the main checkout, local changes must
///    not overlap the files the train changes.
///
/// # Errors
///
/// Every refusal above, and git or store failures.
pub fn land(ctx: &HarvestContext, req: &LandTrainRequest) -> anyhow::Result<LandingOutcome> {
    require_project_identity(ctx)?;
    anyhow::ensure!(
        !req.members.is_empty(),
        "cs land-train refused: no member named; pass one `--member <id>=<head>` per member merge"
    );
    for (i, m) in req.members.iter().enumerate() {
        if req.members[..i].iter().any(|o| o.molecule == m.molecule) {
            anyhow::bail!(
                "cs land-train refused: member {} is named twice",
                m.molecule
            );
        }
    }

    let state_dir = ctx.state_dir();
    let store = FileStore::new(&state_dir);
    // The guards `cs done` runs first, run here per member: the base moves
    // before any member's `cs done` does, so `cs done`'s own copies would come
    // too late to stop a worker from landing its own branch.
    for m in &req.members {
        crate::self_harvest::refuse_worker_self_harvest(
            &m.molecule,
            &crate::pilot_gesture::process_env,
        )?;
        crate::pilot_gesture::refuse_unleased_pilot_gesture(
            &state_dir,
            &m.molecule,
            "cs land-train",
            &crate::pilot_gesture::process_env,
        )?;
    }

    let repo_root = crate::transaction::find_repo_root(ctx)?;
    let project_cfg = cosmon_filestore::load_project_config(&resolve_config_from_context(ctx))?;
    if project_cfg.harvest_authority.is_required() {
        anyhow::bail!(
            "cs land-train refused: `[harvest_authority] required = true` in this galaxy, and \
             train landing does not yet carry sealed harvest authority (ADR-190 §4 D1). Nothing \
             was moved or closed. Close the members one at a time with `cs done` instead."
        );
    }
    let base_branch = crate::base_branch::resolve_with_source(
        &repo_root,
        None,
        project_cfg.project.trunk_branch.as_deref(),
    )
    .branch;

    let train_base = resolve_commit(&repo_root, &req.train_base, "--train-base")?;
    let train_tip = resolve_commit(&repo_root, &req.train_tip, "--train-tip")?;
    let mut expected = Vec::with_capacity(req.members.len());
    let mut warnings = Vec::new();
    for m in &req.members {
        let head = resolve_commit(&repo_root, &m.head, &format!("--member {}", m.molecule))?;
        let mol = store.load_molecule(&m.molecule)?;
        let (message, w) =
            crate::transaction::completion_merge_message(&store, &state_dir, &mol, &project_cfg);
        warnings.extend(w);
        expected.push(ExpectedMerge {
            molecule: m.molecule.clone(),
            head,
            message,
        });
    }

    let guard = store
        .acquire_trunk_lock(&format!("cs land-train {}", short(&train_tip)))
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let base_ref = format!("refs/heads/{base_branch}");
    let current = resolve_commit(&repo_root, &base_ref, "the base branch")?;
    let landing = if on_first_parent_chain(&repo_root, &train_tip, &current)? {
        Landing::AlreadyLanded
    } else if current == train_base {
        Landing::Landed
    } else {
        anyhow::bail!(
            "cs land-train refused: {base_branch} moved after the gate. It is at {current}, and \
             the train was built and gated on {train_base}. Nothing was moved or closed; rebuild \
             the train on the current {base_branch} and gate it again."
        );
    };

    let chain = first_parent_chain(&repo_root, &train_base, &train_tip)?;
    check_train_chain(&train_base, &chain, &expected).map_err(|why| {
        anyhow::anyhow!("cs land-train refused: {why}. Nothing was moved or closed.")
    })?;

    let mut to_close = Vec::new();
    let mut already_closed = Vec::new();
    for member in &expected {
        let mol = store.load_molecule(&member.molecule)?;
        if mol.status != MoleculeStatus::Completed {
            anyhow::bail!(
                "cs land-train refused: member {} is {}, not completed. Only a Completed molecule's \
                 branch may land (ADR-052 §I9). Nothing was moved or closed.",
                member.molecule,
                mol.status
            );
        }
        if let Some(own) = mol
            .base_branch
            .as_deref()
            .filter(|b| !b.trim().is_empty() && b.trim() != base_branch)
        {
            anyhow::bail!(
                "cs land-train refused: member {} integrates into `{own}`, not `{base_branch}`. \
                 Nothing was moved or closed.",
                member.molecule
            );
        }
        if mol.merged_at.is_some() {
            if landing == Landing::Landed {
                anyhow::bail!(
                    "cs land-train refused: member {} is already closed (merged_at is set) but \
                     the train has not landed. Rebuild the train without it. Nothing was moved \
                     or closed.",
                    member.molecule
                );
            }
            already_closed.push(member.molecule.clone());
            continue;
        }
        let branch = format!("refs/heads/feat/{}", member.molecule);
        match rev_parse(&repo_root, &branch)? {
            Some(actual) if actual == member.head => {}
            Some(actual) => anyhow::bail!(
                "cs land-train refused: member {}'s branch feat/{} is at {actual}, not its pinned \
                 head {}. Something was added to or removed from the branch after it was pinned. \
                 Nothing was moved or closed; rebuild and gate the train with the new head.",
                member.molecule,
                member.molecule,
                member.head
            ),
            None => anyhow::bail!(
                "cs land-train refused: member {}'s branch feat/{} does not exist, so its pinned \
                 head {} cannot be checked or closed. Nothing was moved or closed.",
                member.molecule,
                member.molecule,
                member.head
            ),
        }
        to_close.push(PendingClosure {
            molecule: member.molecule.clone(),
            head: member.head.clone(),
        });
    }

    check_exclusions(&repo_root, &train_base, &expected, &req.exclusions)?;

    if landing == Landing::Landed {
        check_shell_surface(&repo_root, &base_branch, &train_base, &train_tip)?;
        fast_forward(&repo_root, &base_branch, &train_base, &train_tip)?;
    }
    drop(guard);

    Ok(LandingOutcome {
        base_branch,
        train_base,
        train_tip,
        landing,
        to_close,
        already_closed,
        warnings,
    })
}

fn check_exclusions(
    repo_root: &Path,
    train_base: &str,
    members: &[ExpectedMerge],
    exclusions: &[ExcludedHead],
) -> anyhow::Result<()> {
    for excluded in exclusions {
        let id = &excluded.molecule;
        if members.iter().any(|member| member.molecule == *id) {
            anyhow::bail!(
                "cs land-train refused: excluded molecule {id} is also listed as a train member. \
                 Nothing was moved or closed."
            );
        }
        let head = resolve_commit(
            repo_root,
            &excluded.head,
            &format!("excluded head for {id}"),
        )?;
        let Some(first_commit) = first_own_commit(repo_root, train_base, &head, id)? else {
            continue;
        };
        for member in members {
            let carries = read_carrier_ancestry(repo_root, member, id, &first_commit)?;
            check_carrier(&member.molecule, id, &first_commit, carries)?;
        }
    }
    Ok(())
}

/// Refuse a member when its ancestry includes the excluded first own commit.
fn check_carrier(
    member: &MoleculeId,
    excluded: &MoleculeId,
    first_commit: &str,
    carries: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !carries,
        "cs land-train refused: member {member} carries excluded member {excluded} at first own \
         commit {first_commit}. Nothing was moved or closed."
    );
    Ok(())
}

/// Read ancestry, preserving the distinction between false and a Git error.
fn read_carrier_ancestry(
    repo_root: &Path,
    member: &ExpectedMerge,
    excluded: &MoleculeId,
    first_commit: &str,
) -> anyhow::Result<bool> {
    let out = git(repo_root)
        .args(["merge-base", "--is-ancestor", first_commit, &member.head])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => anyhow::bail!(
            "cs land-train refused: could not check member {} for excluded member {excluded} at \
             first own commit {first_commit}: git merge-base exited with {}: {}. Nothing was \
             moved or closed.",
            member.molecule,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// The oldest first-parent commit of `head` outside the train base's ancestry.
fn first_own_commit(
    repo_root: &Path,
    train_base: &str,
    head: &str,
    excluded: &MoleculeId,
) -> anyhow::Result<Option<String>> {
    let range = format!("{train_base}..{head}");
    let out = git(repo_root)
        .args(["rev-list", "--first-parent", &range])
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "cs land-train refused: could not read own commits for excluded member {excluded} at \
         head {head}: git rev-list exited with {}: {}. Nothing was moved or closed.",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    // Git lists the range newest first. An empty range has no commit to exclude.
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .last()
        .map(str::to_owned))
}

fn short(oid: &str) -> &str {
    oid.get(..10).unwrap_or(oid)
}

fn git(repo_root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo_root).env("LC_ALL", "C");
    cmd
}

/// Whether `commit` is `head` or one of the commits reached from `head` by
/// first parents alone — that is, whether a fast-forward to `commit` is part
/// of `head`'s own history rather than something merged in from a side.
fn on_first_parent_chain(repo_root: &Path, commit: &str, head: &str) -> anyhow::Result<bool> {
    if commit == head {
        return Ok(true);
    }
    let ancestor = git(repo_root)
        .args(["merge-base", "--is-ancestor", commit, head])
        .status()?;
    if !ancestor.success() {
        return Ok(false);
    }
    let out = git(repo_root)
        .args(["rev-list", "--first-parent", "--parents"])
        .arg(format!("{commit}..{head}"))
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "git rev-list {commit}..{head} failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    // Newest first; the oldest first-parent commit not reachable from
    // `commit` must have `commit` itself as its first parent.
    let listing = String::from_utf8_lossy(&out.stdout);
    let oldest_first_parent = listing
        .lines()
        .last()
        .and_then(|line| line.split_whitespace().nth(1));
    Ok(oldest_first_parent == Some(commit))
}

/// `git rev-parse --verify --quiet <rev>^{commit}`, `None` when it does not
/// resolve.
fn rev_parse(repo_root: &Path, rev: &str) -> anyhow::Result<Option<String>> {
    let out = git(repo_root)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{rev}^{{commit}}"))
        .output()?;
    let oid = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    Ok((out.status.success() && !oid.is_empty()).then_some(oid))
}

fn resolve_commit(repo_root: &Path, rev: &str, what: &str) -> anyhow::Result<String> {
    rev_parse(repo_root, rev)?.ok_or_else(|| {
        anyhow::anyhow!(
            "cs land-train refused: {what} `{rev}` does not resolve to a single commit in {}. \
             Nothing was moved or closed.",
            repo_root.display()
        )
    })
}

/// The first-parent commits of `base..tip`, oldest first, with parents and
/// raw messages.
fn first_parent_chain(repo_root: &Path, base: &str, tip: &str) -> anyhow::Result<Vec<ChainCommit>> {
    let out = git(repo_root)
        .args(["rev-list", "--first-parent", "--reverse", "--parents"])
        .arg(format!("{base}..{tip}"))
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "git rev-list {base}..{tip} failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let mut chain = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut ids = line.split_whitespace().map(str::to_owned);
        let Some(oid) = ids.next() else { continue };
        let parents: Vec<String> = ids.collect();
        let raw = git(repo_root).args(["cat-file", "commit", &oid]).output()?;
        anyhow::ensure!(
            raw.status.success(),
            "git cat-file commit {oid} failed: {}",
            String::from_utf8_lossy(&raw.stderr).trim()
        );
        let raw = String::from_utf8_lossy(&raw.stdout).into_owned();
        let message = raw
            .split_once("\n\n")
            .map_or_else(String::new, |(_, m)| m.to_owned());
        chain.push(ChainCommit {
            oid,
            parents,
            message,
        });
    }
    Ok(chain)
}

/// The #109 shell-surface predicate over `train_base..tip` (ADR-190 §7).
///
/// The surface is a property of a tree, and neither tree is on disk yet — the
/// checkout still holds the train base, or something else entirely. So both
/// trees are extracted into scratch directories: the changed paths that belong
/// to either side's surface are the train's surface changes, and the grant is
/// evaluated against the tip's surface, which is what the landing leaves on
/// disk.
fn check_shell_surface(
    repo_root: &Path,
    base_branch: &str,
    train_base: &str,
    tip: &str,
) -> anyhow::Result<()> {
    let out = git(repo_root)
        .args(["diff", "-z", "--name-only", train_base, tip])
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "git diff {train_base} {tip} failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let changed: Vec<PathBuf> = String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    let base_tree = extract_tree(repo_root, train_base)?;
    let tip_tree = extract_tree(repo_root, tip)?;
    let paths = crate::transaction::shell_surface_changes(
        &changed,
        &crate::trust::surface_paths_in(base_tree.path()),
        &crate::trust::surface_paths_in(tip_tree.path()),
    );
    if paths.is_empty() {
        return Ok(());
    }
    if crate::trust::status_of_content(repo_root, tip_tree.path())
        == crate::trust::TrustStatus::Trusted
    {
        return Ok(());
    }
    let root = repo_root.display();
    anyhow::bail!(
        "cs land-train refused: the local base {base_branch} is still at {train_base}. The train \
         changes the repository's trusted shell surface ({}), and no trust grant covers the train \
         tip. Nothing was moved or closed.\n\n\
         If the train already landed upstream, switch to {base_branch}, run `git pull --ff-only`, \
         and rerun the same `cs land-train` command to close the members.\n\n\
         To land the train locally, review the change:\n\n    \
         $ git -C {root} diff {train_base} {tip} -- {}\n\n\
         Grant trust from a checkout of the train tip, then switch back and rerun:\n\n    \
         $ git -C {root} switch --detach {tip}\n    \
         $ cs trust\n    \
         $ git -C {root} switch {base_branch}",
        paths.join(", "),
        paths.join(" ")
    )
}

/// `git archive <rev> | tar -x` into a fresh scratch directory: the tree as a
/// clone would see it, with no untracked or ignored file.
fn extract_tree(repo_root: &Path, rev: &str) -> anyhow::Result<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    let mut archive = git(repo_root)
        .args(["archive", "--format=tar", rev])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = archive
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("git archive {rev}: no stdout"))?;
    let tar = Command::new("tar")
        .arg("-x")
        .arg("-f")
        .arg("-")
        .arg("-C")
        .arg(dir.path())
        .stdin(stdout)
        .output()?;
    let archived = archive.wait_with_output()?;
    anyhow::ensure!(
        archived.status.success(),
        "git archive {rev} failed: {}",
        String::from_utf8_lossy(&archived.stderr).trim()
    );
    anyhow::ensure!(
        tar.status.success(),
        "extracting {rev} for the shell-surface check failed: {}",
        String::from_utf8_lossy(&tar.stderr).trim()
    );
    Ok(dir)
}

/// The paths of linked worktrees (other than `repo_root`) that have
/// `base_branch` checked out.
fn other_checkouts_of(repo_root: &Path, base_branch: &str) -> anyhow::Result<Vec<PathBuf>> {
    let out = git(repo_root)
        .args(["worktree", "list", "--porcelain"])
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "git worktree list failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let here = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_owned());
    let wanted = format!("branch refs/heads/{base_branch}");
    let mut found = Vec::new();
    let mut current: Option<PathBuf> = None;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(path));
        } else if line == wanted {
            if let Some(path) = current.as_ref() {
                let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
                if canon != here {
                    found.push(path.clone());
                }
            }
        }
    }
    Ok(found)
}

/// Move `base_branch` from `train_base` to `tip` by compare-and-swap, and the
/// main checkout's index and tree with it when the branch is checked out
/// there.
///
/// The tree moves first, with `git read-tree -m -u <train_base> <tip>` — the
/// two-tree fast-forward `git merge --ff-only` itself performs, which refuses
/// when a local change overlaps a file the train changes. Then the ref moves
/// with `git update-ref <ref> <tip> <train_base>`, which fails unless the ref
/// still holds the train base. If that compare-and-swap fails, the tree is
/// moved back, so a failure leaves both where they were.
fn fast_forward(
    repo_root: &Path,
    base_branch: &str,
    train_base: &str,
    tip: &str,
) -> anyhow::Result<()> {
    let base_ref = format!("refs/heads/{base_branch}");
    let others = other_checkouts_of(repo_root, base_branch)?;
    if let Some(other) = others.first() {
        anyhow::bail!(
            "cs land-train refused: {base_branch} is checked out in the worktree {}, whose \
             index and files a fast-forward from {} would leave behind. Nothing was moved or \
             closed; switch that worktree to another branch and rerun.",
            other.display(),
            repo_root.display()
        );
    }
    let head = git(repo_root)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()?;
    let checked_out_here =
        head.status.success() && String::from_utf8_lossy(&head.stdout).trim() == base_ref;

    if checked_out_here {
        let moved = git(repo_root)
            .args(["read-tree", "-m", "-u", train_base, tip])
            .output()?;
        if !moved.status.success() {
            anyhow::bail!(
                "cs land-train refused: the base checkout {} has local changes that overlap \
                 files the train changes:\n{}\nNothing was moved or closed; commit or stash \
                 them, then rerun.",
                repo_root.display(),
                String::from_utf8_lossy(&moved.stderr).trim()
            );
        }
    }
    let cas = git(repo_root)
        .args([
            "update-ref",
            "-m",
            &format!("cs land-train: fast-forward to train tip {}", short(tip)),
            &base_ref,
            tip,
            train_base,
        ])
        .output()?;
    if !cas.status.success() {
        let restored = !checked_out_here
            || git(repo_root)
                .args(["read-tree", "-m", "-u", tip, train_base])
                .output()
                .is_ok_and(|o| o.status.success());
        anyhow::bail!(
            "cs land-train refused: the compare-and-swap of {base_ref} from {train_base} to {tip} \
             failed, so {base_branch} moved after the gate: {}{}",
            String::from_utf8_lossy(&cas.stderr).trim(),
            if restored {
                ". Nothing was moved or closed."
            } else {
                ". The base checkout's index and files could not be moved back to the train \
                 base; run `git read-tree -m -u` from the train tip to the train base by hand."
            }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mol(id: &str) -> MoleculeId {
        MoleculeId::new(id).unwrap()
    }

    fn merge(oid: &str, first: &str, second: &str, message: &str) -> ChainCommit {
        ChainCommit {
            oid: oid.to_owned(),
            parents: vec![first.to_owned(), second.to_owned()],
            message: message.to_owned(),
        }
    }

    fn expected(id: &str, head: &str) -> ExpectedMerge {
        ExpectedMerge {
            molecule: mol(id),
            head: head.to_owned(),
            message: format!("Merge branch 'feat/{id}'\n\nMol-Id: {id}\n"),
        }
    }

    fn two() -> Vec<ExpectedMerge> {
        vec![
            expected("task-20260101-aaaa", "ha"),
            expected("task-20260101-bbbb", "hb"),
        ]
    }

    #[test]
    fn a_carrier_refusal_names_both_members_and_the_carried_commit() {
        let member = mol("task-20260101-aaaa");
        let excluded = mol("task-20260101-bbbb");
        let commit = "3bc0ffc";
        let refusal = check_carrier(&member, &excluded, commit, true)
            .unwrap_err()
            .to_string();
        for needle in [member.as_str(), excluded.as_str(), commit] {
            assert!(refusal.contains(needle), "{refusal}");
        }
    }

    #[test]
    fn a_member_without_the_excluded_commit_passes_the_carrier_check() {
        let member = mol("task-20260101-aaaa");
        let excluded = mol("task-20260101-bbbb");
        assert!(check_carrier(&member, &excluded, "3bc0ffc", false).is_ok());
    }

    /// The shape two serial `cs done` merges leave is accepted.
    #[test]
    fn two_member_merges_in_order_are_a_train() {
        let e = two();
        let chain = vec![
            merge("m1", "base", "ha", &e[0].message),
            merge("m2", "m1", "hb", &e[1].message),
        ];
        assert_eq!(check_train_chain("base", &chain, &e), Ok(()));
    }

    /// A fast-forwarded member is a one-parent commit on the chain.
    #[test]
    fn a_one_parent_commit_on_the_chain_is_refused_by_id() {
        let e = two();
        let chain = vec![
            merge("m1", "base", "ha", &e[0].message),
            ChainCommit {
                oid: "c2".to_owned(),
                parents: vec!["m1".to_owned()],
                message: "feat: b\n".to_owned(),
            },
        ];
        let why = check_train_chain("base", &chain, &e).unwrap_err();
        assert!(why.contains("c2") && why.contains("1 parent"), "{why}");
    }

    /// Members named in the wrong order do not match the merges' second
    /// parents.
    #[test]
    fn members_named_out_of_order_are_refused() {
        let e = two();
        let chain = vec![
            merge("m1", "base", "hb", &e[1].message),
            merge("m2", "m1", "ha", &e[0].message),
        ];
        assert!(check_train_chain("base", &chain, &e).is_err());
    }

    /// A named member with no merge in the train is refused: its own `cs done`
    /// would otherwise merge it outside the train.
    #[test]
    fn a_named_member_without_a_merge_is_refused() {
        let e = two();
        let chain = vec![merge("m1", "base", "ha", &e[0].message)];
        let why = check_train_chain("base", &chain, &e).unwrap_err();
        assert!(
            why.contains("1 member merge") && why.contains("2 member"),
            "{why}"
        );
    }

    /// A chain whose oldest commit does not sit on the train base does not
    /// reach it by first parents.
    #[test]
    fn a_chain_not_rooted_on_the_train_base_is_refused() {
        let e = two();
        let chain = vec![
            merge("m1", "elsewhere", "ha", &e[0].message),
            merge("m2", "m1", "hb", &e[1].message),
        ];
        assert!(check_train_chain("base", &chain, &e)
            .unwrap_err()
            .contains("first parent"));
    }

    /// The rendered text must be a byte-exact prefix; only trailer lines may
    /// follow it, as a sign-off hook would append to `cs done`'s own merge.
    #[test]
    fn the_message_rule_allows_appended_trailers_only() {
        let rendered = "Merge branch 'feat/x'\n\nMol-Id: x\n";
        assert!(merge_message_matches(rendered, rendered));
        assert!(merge_message_matches(
            "Merge branch 'feat/x'\n\nMol-Id: x\nSigned-off-by: A <a@example.org>\n",
            rendered
        ));
        assert!(merge_message_matches(
            "Merge branch 'feat/x'\n\nMol-Id: x\n\nSigned-off-by: A <a@example.org>\n",
            rendered
        ));
        assert!(!merge_message_matches(
            "Merge branch 'feat/x'\n\nMol-Id: xy\n",
            rendered
        ));
        assert!(!merge_message_matches(
            "Merge branch 'feat/x' into train\n\nMol-Id: x\n",
            rendered
        ));
        assert!(!merge_message_matches(
            "Merge branch 'feat/x'\n\nMol-Id: x\n\nextra prose\n",
            rendered
        ));
        assert!(!merge_message_matches("Merge branch 'feat/x'\n", rendered));
    }

    fn git_in(repo: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// The compare-and-swap is the last guard: when the base no longer holds
    /// the train base, `update-ref` fails, the checked-out tree is moved back,
    /// and the error says the base moved. Exercised directly, because under
    /// `land` the pre-check refuses first.
    #[test]
    fn a_failed_compare_and_swap_moves_the_tree_back() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git_in(repo, &["init", "-q", "-b", "main"]);
        git_in(repo, &["config", "user.email", "t@example.com"]);
        git_in(repo, &["config", "user.name", "T"]);
        git_in(repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git_in(repo, &["add", "base.txt"]);
        git_in(repo, &["commit", "-qm", "base"]);
        let train_base = git_in(repo, &["rev-parse", "HEAD"]);
        git_in(repo, &["checkout", "-qb", "train"]);
        std::fs::write(repo.join("train.txt"), "train\n").unwrap();
        git_in(repo, &["add", "train.txt"]);
        git_in(repo, &["commit", "-qm", "train"]);
        let tip = git_in(repo, &["rev-parse", "HEAD"]);
        git_in(repo, &["checkout", "-q", "main"]);
        // Another writer moves main after the gate, without changing the tree.
        git_in(repo, &["commit", "-q", "--allow-empty", "-m", "moved"]);
        let moved = git_in(repo, &["rev-parse", "main"]);

        let err = fast_forward(repo, "main", &train_base, &tip)
            .unwrap_err()
            .to_string();
        assert!(err.contains("compare-and-swap"), "{err}");
        assert!(err.contains("Nothing was moved"), "{err}");
        assert_eq!(git_in(repo, &["rev-parse", "main"]), moved);
        assert_eq!(git_in(repo, &["status", "--porcelain"]), "");
        assert!(!repo.join("train.txt").exists(), "the tree is moved back");
    }

    /// `--member` parsing.
    #[test]
    fn a_member_spec_is_id_equals_head() {
        let m = TrainMember::parse("task-20260101-aaaa=abc123").unwrap();
        assert_eq!(m.molecule, mol("task-20260101-aaaa"));
        assert_eq!(m.head, "abc123");
        assert!(TrainMember::parse("task-20260101-aaaa").is_err());
        assert!(TrainMember::parse("=abc").is_err());
    }
}
