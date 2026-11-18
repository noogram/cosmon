// SPDX-License-Identifier: AGPL-3.0-only

//! ADR-191 through the compiled binary: a project declares where its
//! external build directories live, and `cs purge --worktrees` reclaims the
//! per-molecule ones of terminal molecules — through a worktree's `target`
//! link, or as an orphan once the worktree is gone.
//!
//! Every fixture lives under one canonicalized temporary directory: the
//! repository, the state directory and the declared bases are siblings, so a
//! base is never an ancestor of the repository and nothing outside the
//! temporary directory is read or written. Each "refused, untouched" case
//! carries a positive control in the same run — a correctly linked directory
//! that *is* reclaimed — so a refusal is shown to be specific, not the
//! feature being absent.
//!
//! The numbers in the test names are the ADR's §4 acceptance criteria.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};

use serde_json::Value;

const M: &str = "task-20261118-aa01";
const OTHER: &str = "task-20261118-aa02";

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    base1: PathBuf,
    base2: PathBuf,
    home: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fx() -> Fx {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Canonical once: reports print resolved paths (`/private/var/...` on
    // macOS), and the assertions compare against them.
    let root = fs::canonicalize(tmp.path()).unwrap();
    let repo = root.join("repo");
    let state = root.join("state");
    let base1 = root.join("vol-a/builds");
    let base2 = root.join("vol-b/builds");
    let home = root.join("home");
    for d in [&repo, &state, &base1, &base2, &home] {
        fs::create_dir_all(d).unwrap();
    }
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "maintainers@noogram.org"]);
    git(&repo, &["config", "user.name", "Noogram"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join(".gitignore"), ".worktrees/\ntarget\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "test: seed"]);
    Fx {
        _tmp: tmp,
        root,
        repo,
        state,
        base1,
        base2,
        home,
    }
}

impl Fx {
    /// Write `[worktree_reclaim]` with one `[[external]]` entry per
    /// `(base, suffix)`.
    fn declare(&self, entries: &[(&Path, &str)]) {
        let mut toml = String::from("[worktree_reclaim]\nevict = [\"target\"]\n");
        for (base, suffix) in entries {
            toml.push_str(&format!(
                "\n[[worktree_reclaim.external]]\nroot = \"target\"\nbase = {}\nsuffix = {}\n",
                toml_str(&base.display().to_string()),
                toml_str(suffix)
            ));
        }
        fs::write(self.state.join("config.toml"), toml).unwrap();
    }

    fn declare_raw(&self, toml: &str) {
        fs::write(self.state.join("config.toml"), toml).unwrap();
    }

    /// Save a molecule record with `status` in this fleet's store.
    fn molecule(&self, id: &str, status: &str) {
        let dir = self.state.join("fleets/default/molecules").join(id);
        fs::create_dir_all(&dir).unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let mol = serde_json::json!({
            "id": id,
            "fleet_id": "default",
            "formula_id": "task-work",
            "status": status,
            "created_at": now,
            "updated_at": now,
            "total_steps": 2,
            "current_step": 2,
            "variables": {},
            "completed_steps": [],
            "links": [],
            "typed_links": [],
            "escalations": [],
            "briefing_seals": [],
            "bootstrap_seals": [],
            "tags": [],
        });
        let data: cosmon_state::MoleculeData = serde_json::from_value(mol).unwrap();
        let store = cosmon_filestore::FileStore::new(&self.state);
        cosmon_state::StateStore::save_molecule(&store, &data.id, &data).unwrap();
    }

    /// A molecule record that exists but cannot be decoded: status Unknown.
    fn unreadable_molecule(&self, id: &str) {
        let dir = self.state.join("fleets/default/molecules").join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("state.json"), "{ not json").unwrap();
    }

    /// A registered worktree at `.worktrees/<id>`.
    fn worktree(&self, id: &str) -> PathBuf {
        let wt = self.repo.join(".worktrees").join(id);
        self.worktree_at(id, &wt);
        wt
    }

    /// A registered worktree of molecule `id` at an arbitrary path whose
    /// last component is `id`.
    fn worktree_at(&self, id: &str, wt: &Path) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let branch = format!("feat/{id}-{n}");
        git(
            &self.repo,
            &["worktree", "add", "-qb", &branch, &wt.to_string_lossy()],
        );
    }

    fn purge(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cs"))
            .env_remove("COSMON_PARENT_MOL_ID")
            .env_remove("COSMON_MOL_DIR")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.home)
            .current_dir(&self.repo)
            .arg("--config")
            .arg(&self.state)
            .args(["purge", "--worktrees"])
            .args(extra)
            .output()
            .expect("spawn cs")
    }

    /// `cs --json purge --worktrees <extra>`, asserted successful, as JSON.
    fn purge_json(&self, extra: &[&str]) -> Value {
        let out = Command::new(env!("CARGO_BIN_EXE_cs"))
            .env_remove("COSMON_PARENT_MOL_ID")
            .env_remove("COSMON_MOL_DIR")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.home)
            .current_dir(&self.repo)
            .arg("--config")
            .arg(&self.state)
            .arg("--json")
            .args(["purge", "--worktrees"])
            .args(extra)
            .output()
            .expect("spawn cs");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "exit {:?}\nstdout:\n{stdout}\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        // The worktree pass prints its JSON document last.
        let line = stdout
            .lines()
            .rev()
            .find(|l| l.trim_start().starts_with('{') && l.contains("\"selected\""))
            .unwrap_or_else(|| panic!("no worktree-pass JSON in:\n{stdout}"));
        serde_json::from_str(line).unwrap()
    }
}

/// A TOML basic string: Rust's `{:?}` writes NUL as `\0`, which TOML does
/// not accept.
fn toml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A per-molecule build directory: lock anchor, payload, a cache tag.
fn build_dir(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir.join("debug/deps")).unwrap();
    fs::write(dir.join("debug/.cargo-lock"), "").unwrap();
    fs::write(dir.join("debug/deps/payload.o"), "rebuildable bytes").unwrap();
    fs::write(dir.join("CACHEDIR.TAG"), "tag").unwrap();
    dir.to_path_buf()
}

fn payload_present(dir: &Path) -> bool {
    dir.join("debug/deps/payload.o").is_file()
}

/// Assert the reclaimed shape: payload gone, anchor and its parent kept.
fn assert_reclaimed(dir: &Path, json: &Value) {
    assert!(!payload_present(dir), "payload should be gone: {json:#}");
    assert!(!dir.join("CACHEDIR.TAG").exists(), "{json:#}");
    assert!(dir.join("debug/.cargo-lock").is_file(), "anchor kept");
    assert!(dir.is_dir(), "the per-molecule directory itself stays");
}

fn ext(json: &Value) -> &Value {
    &json["external"]
}

/// The external item (selected or withheld) whose path is `dir`.
fn item<'a>(json: &'a Value, list: &str, dir: &Path) -> Option<&'a Value> {
    ext(json)[list]
        .as_array()
        .and_then(|a| a.iter().find(|i| i["path"] == dir.display().to_string()))
}

fn withheld_reasons(json: &Value, dir: &Path) -> String {
    item(json, "withheld", dir)
        .unwrap_or_else(|| panic!("{} not withheld: {json:#}", dir.display()))["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap().to_owned())
        .collect::<Vec<_>>()
        .join(" | ")
}

fn assert_selected(json: &Value, dir: &Path) {
    assert!(
        item(json, "selected", dir).is_some(),
        "{} not selected: {json:#}",
        dir.display()
    );
}

/// A correctly linked, Completed molecule in `base`: the positive control
/// every refusal below is measured against.
fn control(f: &Fx, base: &Path, suffix: &str) -> PathBuf {
    let id = "task-20261118-c0c0";
    f.molecule(id, "completed");
    let wt = f.worktree(id);
    let dir = build_dir(&base.join(format!("{id}{suffix}")));
    symlink(&dir, wt.join("target")).unwrap();
    dir
}

fn hold_lock(f: &Fx, anchor: &Path, tag: &str) -> Child {
    let ready = f.root.join(format!("holder-{tag}.ready"));
    let child = Command::new("python3")
        .arg("-c")
        .arg(
            "import fcntl,sys,time\n\
             f=open(sys.argv[1],'r+')\n\
             fcntl.flock(f, fcntl.LOCK_EX|fcntl.LOCK_NB)\n\
             open(sys.argv[2],'w').write('1')\n\
             time.sleep(60)\n",
        )
        .arg(anchor)
        .arg(&ready)
        .spawn()
        .expect("python3");
    let start = std::time::Instant::now();
    while !ready.exists() && start.elapsed() < std::time::Duration::from_secs(10) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(ready.exists(), "holder never took the lock");
    child
}

// ---------------------------------------------------------------------------
// The positive path
// ---------------------------------------------------------------------------

/// 21 and 17 — a link to `<base2>/<m>-wt`, `m` Completed, flock acquired:
/// the payload goes, the report names `<base2>` and the measured size, and
/// the link survives.
#[test]
fn adr191_21_linked_root_under_second_base_with_suffix_is_reclaimed() {
    let f = fx();
    f.declare(&[(&f.base1, "-wt"), (&f.base2, "-wt")]);
    f.molecule(M, "completed");
    let wt = f.worktree(M);
    let dir = build_dir(&f.base2.join(format!("{M}-wt")));
    symlink(&dir, wt.join("target")).unwrap();

    let dry = f.purge_json(&[]);
    let sel = item(&dry, "selected", &dir).unwrap_or_else(|| panic!("{dry:#}"));
    assert_eq!(sel["base"], f.base2.display().to_string(), "{dry:#}");
    assert_eq!(sel["kind"], "linked", "{dry:#}");
    assert!(
        sel["bytes"].as_u64().unwrap() > 0,
        "size measured on the resolved directory: {dry:#}"
    );
    assert!(payload_present(&dir), "a dry run removes nothing");

    let wet = f.purge_json(&["--allow-unharvested"]);
    assert_reclaimed(&dir, &wet);
    // 17 — the link belongs to the worktree and is never removed.
    assert!(fs::symlink_metadata(wt.join("target"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read_link(wt.join("target")).unwrap(), dir);
    assert!(wet["preserved"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == &Value::from(dir.join("debug/.cargo-lock").display().to_string())));
}

/// 19 — the dry run and the absence of `--allow-unharvested` remove
/// nothing, while naming what an execution would take.
#[test]
fn adr191_19_dry_run_and_missing_gesture_remove_nothing() {
    let f = fx();
    f.declare(&[(&f.base1, "")]);
    let dir = control(&f, &f.base1, "");
    let plain = f.purge_json(&[]);
    assert_selected(&plain, &dir);
    assert!(payload_present(&dir));
    let dry = f.purge_json(&["--allow-unharvested", "--dry-run"]);
    assert_selected(&dry, &dir);
    assert_eq!(dry["executed"], false);
    assert!(payload_present(&dir));
}

/// The register in the operator's words: the base, the resolved path and
/// the size are printed, and an unreachable base says it was not scanned.
#[test]
fn adr191_text_register_names_base_path_and_size() {
    let f = fx();
    let absent = f.root.join("vol-c/builds");
    f.declare(&[(&f.base1, ""), (&absent, "")]);
    let dir = control(&f, &f.base1, "");
    let out = f.purge(&[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("external derived roots"), "{stdout}");
    assert!(stdout.contains(&dir.display().to_string()), "{stdout}");
    assert!(
        stdout.contains(&format!("base {}", f.base1.display())),
        "{stdout}"
    );
    assert!(stdout.contains("unreachable"), "{stdout}");
    assert!(stdout.contains(&absent.display().to_string()), "{stdout}");
    assert!(stdout.contains("not scanned"), "{stdout}");
}

// ---------------------------------------------------------------------------
// Link refusals (D2)
// ---------------------------------------------------------------------------

/// 1 — no base declared: a link that leaves the worktree is refused as
/// today and its target survives an executing run.
#[test]
fn adr191_01_without_declaration_an_escaping_link_is_unavailable() {
    let f = fx();
    f.molecule(M, "completed");
    let wt = f.worktree(M);
    let elsewhere = build_dir(&f.base1.join(M));
    symlink(&elsewhere, wt.join("target")).unwrap();
    let out = f.purge(&["--allow-unharvested"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(
        stdout.contains("resolves outside the candidate"),
        "{stdout}"
    );
    assert!(payload_present(&elsewhere));
}

/// 2 and 22 — a link outside every declared base is refused with the
/// resolution named; a link into an undeclared second base is the same.
#[test]
fn adr191_02_22_link_outside_every_declared_base_is_refused() {
    let f = fx();
    f.declare(&[(&f.base1, "-wt")]);
    let ctl = control(&f, &f.base1, "-wt");
    f.molecule(M, "completed");
    let wt = f.worktree(M);
    let undeclared = build_dir(&f.base2.join(format!("{M}-wt")));
    symlink(&undeclared, wt.join("target")).unwrap();

    let json = f.purge_json(&["--allow-unharvested"]);
    let why = withheld_reasons(&json, &undeclared);
    assert!(why.contains("outside every declared base"), "{why}");
    assert!(why.contains(&undeclared.display().to_string()), "{why}");
    assert!(payload_present(&undeclared));
    assert_reclaimed(&ctl, &json);
}

/// 3 — a link to the base itself is refused; nothing under that base is
/// touched.
#[test]
fn adr191_03_link_to_the_base_itself_is_refused() {
    let f = fx();
    f.declare(&[(&f.base1, ""), (&f.base2, "")]);
    let ctl = control(&f, &f.base2, "");
    f.molecule(M, "completed");
    let wt = f.worktree(M);
    build_dir(&f.base1);
    symlink(&f.base1, wt.join("target")).unwrap();

    let json = f.purge_json(&["--allow-unharvested"]);
    let why = withheld_reasons(&json, &f.base1);
    assert!(why.contains("the declared base itself"), "{why}");
    assert!(payload_present(&f.base1));
    assert_reclaimed(&ctl, &json);
}

/// 4 and 24 — a link to another molecule's directory is refused, even when
/// that molecule is Completed in this store; its directory is untouched.
#[test]
fn adr191_04_24_link_to_another_molecules_directory_is_refused() {
    for suffix in ["", "-wt"] {
        let f = fx();
        f.declare(&[(&f.base1, suffix)]);
        let ctl = control(&f, &f.base1, suffix);
        f.molecule(M, "completed");
        f.molecule(OTHER, "completed");
        let wt = f.worktree(M);
        let theirs = build_dir(&f.base1.join(format!("{OTHER}{suffix}")));
        symlink(&theirs, wt.join("target")).unwrap();

        let json = f.purge_json(&["--allow-unharvested"]);
        let why = withheld_reasons(&json, &theirs);
        assert!(
            why.contains("not this molecule's directory"),
            "suffix {suffix:?}: {why}"
        );
        // It is not an orphan either: a live link reaches it.
        assert!(
            why.contains(&wt.join("target").display().to_string()),
            "{why}"
        );
        assert!(payload_present(&theirs), "suffix {suffix:?}");
        assert_reclaimed(&ctl, &json);
    }
}

/// 23 — suffix `-wt` declared, link to `<base1>/<m>`: refused, untouched.
#[test]
fn adr191_23_link_without_the_declared_suffix_is_refused() {
    let f = fx();
    f.declare(&[(&f.base1, "-wt")]);
    let ctl = control(&f, &f.base1, "-wt");
    f.molecule(M, "completed");
    let wt = f.worktree(M);
    let bare = build_dir(&f.base1.join(M));
    symlink(&bare, wt.join("target")).unwrap();

    let json = f.purge_json(&["--allow-unharvested"]);
    let why = withheld_reasons(&json, &bare);
    assert!(why.contains("not this molecule's directory"), "{why}");
    assert!(payload_present(&bare));
    assert_reclaimed(&ctl, &json);
}

/// A link deeper than one level below a base is refused: not the layout
/// any entry names.
#[test]
fn adr191_link_below_a_base_child_is_refused() {
    let f = fx();
    f.declare(&[(&f.base1, "")]);
    let ctl = control(&f, &f.base1, "");
    f.molecule(M, "completed");
    let wt = f.worktree(M);
    let deep = build_dir(&f.base1.join(M).join("sub"));
    symlink(&deep, wt.join("target")).unwrap();

    let json = f.purge_json(&["--allow-unharvested"]);
    let why = withheld_reasons(&json, &deep);
    assert!(why.contains("not the declared layout"), "{why}");
    assert!(payload_present(&deep));
    assert_reclaimed(&ctl, &json);
}

/// 5 — two candidates whose links resolve to the same directory: neither
/// is selected and the directory is untouched.
#[test]
fn adr191_05_two_links_to_one_directory_select_neither() {
    let f = fx();
    f.declare(&[(&f.base1, "")]);
    let ctl = control(&f, &f.base1, "");
    f.molecule(M, "completed");
    let w1 = f.worktree(M);
    let w2 = f.root.join("elsewhere").join(M);
    fs::create_dir_all(w2.parent().unwrap()).unwrap();
    f.worktree_at(M, &w2);
    let shared = build_dir(&f.base1.join(M));
    symlink(&shared, w1.join("target")).unwrap();
    symlink(&shared, w2.join("target")).unwrap();

    let json = f.purge_json(&["--allow-unharvested"]);
    let why = withheld_reasons(&json, &shared);
    assert!(why.contains("same directory"), "{why}");
    assert!(payload_present(&shared));
    assert_reclaimed(&ctl, &json);
}

/// 25 — the same directory reached through two declared bases: `<base2>/<m>-wt`
/// is a link to `<base1>/<m>-wt`. Both candidates' links canonicalize to
/// one directory: neither selects it, and the orphan scan withholds the
/// link child of `<base2>`.
#[test]
fn adr191_25_one_directory_through_two_bases_selects_neither() {
    let f = fx();
    f.declare(&[(&f.base1, "-wt"), (&f.base2, "-wt")]);
    let ctl = control(&f, &f.base1, "-wt");
    f.molecule(M, "completed");
    let w1 = f.worktree(M);
    let w2 = f.root.join("elsewhere").join(M);
    fs::create_dir_all(w2.parent().unwrap()).unwrap();
    f.worktree_at(M, &w2);
    let real = build_dir(&f.base1.join(format!("{M}-wt")));
    let alias = f.base2.join(format!("{M}-wt"));
    symlink(&real, &alias).unwrap();
    symlink(&real, w1.join("target")).unwrap();
    symlink(&alias, w2.join("target")).unwrap();

    let json = f.purge_json(&["--allow-unharvested"]);
    let why = withheld_reasons(&json, &real);
    assert!(why.contains("same directory"), "{why}");
    let alias_why = withheld_reasons(&json, &alias);
    assert!(alias_why.contains("symbolic link"), "{alias_why}");
    assert!(payload_present(&real));
    assert!(fs::symlink_metadata(&alias)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_reclaimed(&ctl, &json);
}

// ---------------------------------------------------------------------------
// The gate (D4) and the lock (D5)
// ---------------------------------------------------------------------------

/// 7 — every non-terminal status, and an unreadable record, keep the
/// external payload.
#[test]
fn adr191_07_nonterminal_or_unknown_status_keeps_the_external_payload() {
    let f = fx();
    f.declare(&[(&f.base1, "")]);
    let ctl = control(&f, &f.base1, "");
    let statuses = ["running", "pending", "queued", "frozen", "starved"];
    let mut dirs = Vec::new();
    for (n, status) in statuses.iter().enumerate() {
        let id = format!("task-20261118-b00{n}");
        f.molecule(&id, status);
        let wt = f.worktree(&id);
        let dir = build_dir(&f.base1.join(&id));
        symlink(&dir, wt.join("target")).unwrap();
        dirs.push((status.to_string(), dir));
    }
    let unknown = "task-20261118-b0ff";
    f.unreadable_molecule(unknown);
    let wt = f.worktree(unknown);
    let dir = build_dir(&f.base1.join(unknown));
    symlink(&dir, wt.join("target")).unwrap();
    dirs.push(("unknown".to_owned(), dir));

    let json = f.purge_json(&["--allow-unharvested"]);
    for (status, dir) in &dirs {
        let why = withheld_reasons(&json, dir);
        assert!(why.contains("molecule"), "{status}: {why}");
        assert!(payload_present(dir), "{status}");
    }
    assert_reclaimed(&ctl, &json);
}

/// 8 — no record in this store: the external payload is untouched (an
/// external base may be shared with another fleet).
#[test]
fn adr191_08_absent_record_keeps_the_external_payload() {
    let f = fx();
    f.declare(&[(&f.base1, "")]);
    let ctl = control(&f, &f.base1, "");
    let wt = f.worktree(M);
    let dir = build_dir(&f.base1.join(M));
    symlink(&dir, wt.join("target")).unwrap();

    let json = f.purge_json(&["--allow-unharvested"]);
    let why = withheld_reasons(&json, &dir);
    assert!(why.contains("no record"), "{why}");
    assert!(payload_present(&dir));
    assert_reclaimed(&ctl, &json);
}

/// 9 — a second process holds the debug lock, or only the release lock:
/// `Held`, nothing removed.
#[test]
fn adr191_09_held_debug_or_release_lock_removes_nothing() {
    for profile in ["debug", "release"] {
        let f = fx();
        f.declare(&[(&f.base1, "")]);
        let ctl = control(&f, &f.base1, "");
        f.molecule(M, "completed");
        let wt = f.worktree(M);
        let dir = build_dir(&f.base1.join(M));
        fs::create_dir_all(dir.join("release")).unwrap();
        fs::write(dir.join("release/.cargo-lock"), "").unwrap();
        fs::write(dir.join("release/big.o"), "release bytes").unwrap();
        symlink(&dir, wt.join("target")).unwrap();
        let mut holder = hold_lock(&f, &dir.join(profile).join(".cargo-lock"), profile);

        let json = f.purge_json(&["--allow-unharvested"]);
        holder.kill().ok();
        holder.wait().ok();
        let why = withheld_reasons(&json, &dir);
        assert!(why.contains("holds the build lock"), "{profile}: {why}");
        assert!(payload_present(&dir), "{profile}");
        assert!(dir.join("release/big.o").is_file(), "{profile}");
        assert_reclaimed(&ctl, &json);
    }
}

/// 9, positive twin — with nobody holding them, every profile lock present
/// (debug, release, `<triple>/debug`) is acquired and preserved as an anchor.
#[test]
fn adr191_09_every_profile_lock_is_preserved_as_an_anchor() {
    let f = fx();
    f.declare(&[(&f.base1, "")]);
    f.molecule(M, "completed");
    let wt = f.worktree(M);
    let dir = build_dir(&f.base1.join(M));
    for p in ["release", "aarch64-apple-ios-sim/debug"] {
        fs::create_dir_all(dir.join(p)).unwrap();
        fs::write(dir.join(p).join(".cargo-lock"), "").unwrap();
        fs::write(dir.join(p).join("out.o"), "bytes").unwrap();
    }
    symlink(&dir, wt.join("target")).unwrap();

    let json = f.purge_json(&["--allow-unharvested"]);
    assert_reclaimed(&dir, &json);
    for p in ["release", "aarch64-apple-ios-sim/debug"] {
        assert!(dir.join(p).join(".cargo-lock").is_file(), "{p} anchor kept");
        assert!(!dir.join(p).join("out.o").exists(), "{p} payload gone");
    }
}

/// 10 — the anchor is missing: `ProbeFailed`, nothing removed.
#[test]
fn adr191_10_missing_anchor_is_probe_failed() {
    let f = fx();
    f.declare(&[(&f.base1, "")]);
    let ctl = control(&f, &f.base1, "");
    f.molecule(M, "completed");
    let wt = f.worktree(M);
    let dir = build_dir(&f.base1.join(M));
    fs::remove_file(dir.join("debug/.cargo-lock")).unwrap();
    symlink(&dir, wt.join("target")).unwrap();

    let json = f.purge_json(&["--allow-unharvested"]);
    let why = withheld_reasons(&json, &dir);
    assert!(why.contains("could not be probed"), "{why}");
    assert!(payload_present(&dir));
    assert!(
        !dir.join("debug/.cargo-lock").exists(),
        "the anchor is never created"
    );
    assert_reclaimed(&ctl, &json);
}

/// 11 — a `du` loop whose argv names the directory is not evidence of use:
/// with the flock acquired the payload goes; with the flock held by a
/// second process nothing goes, `du` or not.
///
/// No red run of the first half is possible against the code before this
/// change in a way that isolates argv — nothing in cosmon ever read argv;
/// the test pins that it never starts to.
#[test]
fn adr191_11_a_du_walker_does_not_block_and_the_flock_decides() {
    for held in [false, true] {
        let f = fx();
        f.declare(&[(&f.base1, "")]);
        f.molecule(M, "completed");
        let wt = f.worktree(M);
        let dir = build_dir(&f.base1.join(M));
        symlink(&dir, wt.join("target")).unwrap();
        let mut du = Command::new("sh")
            .arg("-c")
            .arg("while :; do du -sk \"$1\" >/dev/null 2>&1; sleep 0.05; done")
            .arg("sh")
            .arg(&dir)
            .spawn()
            .expect("sh");
        let mut holder = held.then(|| hold_lock(&f, &dir.join("debug/.cargo-lock"), "du"));

        let json = f.purge_json(&["--allow-unharvested"]);
        du.kill().ok();
        du.wait().ok();
        if let Some(h) = holder.as_mut() {
            h.kill().ok();
            h.wait().ok();
        }
        if held {
            assert!(payload_present(&dir), "held: {json:#}");
        } else {
            assert_reclaimed(&dir, &json);
        }
    }
}

// ---------------------------------------------------------------------------
// Orphans (D6) and the observed layout (D7)
// ---------------------------------------------------------------------------

/// 13 and 26 — an orphan `<base2>/<m>-wt` with `m` Completed and no live
/// link: payload removed, anchor and parent kept, base and siblings
/// untouched, report names `<base2>`.
#[test]
fn adr191_13_26_orphan_of_a_completed_molecule_is_reclaimed() {
    let f = fx();
    f.declare(&[(&f.base1, "-wt"), (&f.base2, "-wt")]);
    f.molecule(M, "completed");
    let dir = build_dir(&f.base2.join(format!("{M}-wt")));
    fs::write(f.base2.join("README"), "a sibling file").unwrap();
    let sibling = build_dir(&f.base2.join("scratch"));

    let json = f.purge_json(&["--allow-unharvested"]);
    let sel = item(&json, "selected", &dir).unwrap_or_else(|| panic!("{json:#}"));
    assert_eq!(sel["kind"], "orphan");
    assert_eq!(sel["base"], f.base2.display().to_string());
    assert_reclaimed(&dir, &json);
    assert!(f.base2.join("README").is_file());
    assert!(payload_present(&sibling));
    let why = withheld_reasons(&json, &sibling);
    assert!(why.contains("not a molecule directory"), "{why}");
}

/// 26 — each missing condition keeps the orphan and names `<base2>`:
/// Collapsed is enough, Running is not, no record is not, a held lock is
/// not; `<m>` without the suffix and `notanid-wt` are withheld.
#[test]
fn adr191_26_orphan_withheld_unless_every_condition_holds() {
    let f = fx();
    f.declare(&[(&f.base2, "-wt")]);
    let ok = "task-20261118-d001";
    let running = "task-20261118-d002";
    let absent = "task-20261118-d003";
    let held = "task-20261118-d004";
    f.molecule(ok, "collapsed");
    f.molecule(running, "running");
    f.molecule(held, "completed");
    f.molecule(M, "completed");
    let d_ok = build_dir(&f.base2.join(format!("{ok}-wt")));
    let d_running = build_dir(&f.base2.join(format!("{running}-wt")));
    let d_absent = build_dir(&f.base2.join(format!("{absent}-wt")));
    let d_held = build_dir(&f.base2.join(format!("{held}-wt")));
    let d_nosuffix = build_dir(&f.base2.join(M));
    let d_notid = build_dir(&f.base2.join("notanid-wt"));
    let mut holder = hold_lock(&f, &d_held.join("debug/.cargo-lock"), "orphan");

    let json = f.purge_json(&["--allow-unharvested"]);
    holder.kill().ok();
    holder.wait().ok();
    assert_reclaimed(&d_ok, &json);
    for (dir, needle) in [
        (&d_running, "running"),
        (&d_absent, "no record"),
        (&d_held, "holds the build lock"),
        (&d_nosuffix, "not a molecule directory"),
        (&d_notid, "not a molecule directory"),
    ] {
        let entry = item(&json, "withheld", dir).unwrap_or_else(|| panic!("{json:#}"));
        assert_eq!(entry["base"], f.base2.display().to_string(), "{entry:#}");
        let why = withheld_reasons(&json, dir);
        assert!(why.contains(needle), "{}: {why}", dir.display());
        assert!(payload_present(dir), "{}", dir.display());
    }
}

/// 14 — orphan names that are not molecule ids, or name a molecule absent
/// from this store, are reported withheld and untouched.
#[test]
fn adr191_14_orphan_without_a_molecule_of_this_store_is_withheld() {
    let f = fx();
    f.declare(&[(&f.base1, "")]);
    let ctl = build_dir(&f.base1.join(M));
    f.molecule(M, "completed");
    let not_id = build_dir(&f.base1.join("shared-cache"));
    let stranger = build_dir(&f.base1.join(OTHER));
    fs::write(f.base1.join("loose-file"), "x").unwrap();

    let json = f.purge_json(&["--allow-unharvested"]);
    assert!(withheld_reasons(&json, &not_id).contains("not a molecule directory"));
    assert!(withheld_reasons(&json, &stranger).contains("no record"));
    assert!(withheld_reasons(&json, &f.base1.join("loose-file")).contains("not a directory"));
    assert!(payload_present(&not_id));
    assert!(payload_present(&stranger));
    assert_reclaimed(&ctl, &json);
}

/// 12 — a real `w/target` and a stale `<base>/<m>`: the in-worktree rule
/// reclaims `w/target`, and `<base>/<m>` is reached only as an orphan.
#[test]
fn adr191_12_real_directory_and_stale_external_are_reclaimed_by_their_own_rules() {
    let f = fx();
    f.declare(&[(&f.base1, "")]);
    f.molecule(M, "completed");
    let wt = f.worktree(M);
    build_dir(&wt.join("target"));
    let stale = build_dir(&f.base1.join(M));

    let json = f.purge_json(&["--allow-unharvested"]);
    assert!(
        !payload_present(&wt.join("target")),
        "in-worktree rule: {json:#}"
    );
    let sel = item(&json, "selected", &stale).unwrap_or_else(|| panic!("{json:#}"));
    assert_eq!(sel["kind"], "orphan");
    assert_reclaimed(&stale, &json);
}

/// 15 — a link inside the payload that points outside it is unlinked; its
/// target is untouched.
#[test]
fn adr191_15_a_link_inside_the_payload_is_unlinked_not_followed() {
    let f = fx();
    f.declare(&[(&f.base1, "")]);
    let dir = control(&f, &f.base1, "");
    let precious = f.root.join("precious");
    fs::create_dir_all(&precious).unwrap();
    fs::write(precious.join("keep.txt"), "keep").unwrap();
    symlink(&precious, dir.join("debug/escape")).unwrap();

    let json = f.purge_json(&["--allow-unharvested"]);
    assert_reclaimed(&dir, &json);
    assert!(
        fs::symlink_metadata(dir.join("debug/escape")).is_err(),
        "link unlinked"
    );
    assert!(precious.join("keep.txt").is_file(), "its target untouched");
}

/// D7 — a dangling link is reported with a reason, not skipped silently.
#[test]
fn adr191_dangling_link_is_reported() {
    let f = fx();
    f.declare(&[(&f.base1, "")]);
    f.molecule(M, "completed");
    let wt = f.worktree(M);
    let gone = f.base1.join(M);
    symlink(&gone, wt.join("target")).unwrap();
    let json = f.purge_json(&["--allow-unharvested"]);
    let why = withheld_reasons(&json, &wt.join("target"));
    assert!(why.contains("dangling"), "{why}");
}

// ---------------------------------------------------------------------------
// Completeness (D2) and unreachable bases (D1, D6)
// ---------------------------------------------------------------------------

/// 6 — the enumeration is incomplete (`.worktrees/` unreadable): no
/// external root and no orphan is selected; an in-worktree root of a
/// registered worktree elsewhere is reclaimed as before.
#[test]
fn adr191_06_incomplete_enumeration_selects_no_external_root_or_orphan() {
    let f = fx();
    f.declare(&[(&f.base1, "")]);
    f.molecule(M, "completed");
    f.molecule(OTHER, "completed");
    let outside = f.root.join("elsewhere");
    fs::create_dir_all(&outside).unwrap();
    let w_link = outside.join(M);
    f.worktree_at(M, &w_link);
    let linked = build_dir(&f.base1.join(M));
    symlink(&linked, w_link.join("target")).unwrap();
    let w_real = outside.join(OTHER);
    f.worktree_at(OTHER, &w_real);
    build_dir(&w_real.join("target"));
    let orphan_id = "task-20261118-e001";
    f.molecule(orphan_id, "completed");
    let orphan = build_dir(&f.base1.join(orphan_id));
    let wts = f.repo.join(".worktrees");
    fs::create_dir_all(&wts).unwrap();
    fs::set_permissions(&wts, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();

    let json = f.purge_json(&["--allow-unharvested"]);
    fs::set_permissions(&wts, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    assert!(
        !json["enumeration_errors"].as_array().unwrap().is_empty(),
        "{json:#}"
    );
    assert!(withheld_reasons(&json, &linked).contains("enumeration is incomplete"));
    assert!(withheld_reasons(&json, &orphan).contains("enumeration is incomplete"));
    assert!(payload_present(&linked));
    assert!(payload_present(&orphan));
    assert!(
        !payload_present(&w_real.join("target")),
        "in-worktree roots unchanged: {json:#}"
    );
}

/// 27 — `<base2>` declared but absent: the pass runs, `<base2>` is reported
/// unreachable, `<base1>` keeps its selection, and a link into `<base2>`
/// is reported dangling.
#[test]
fn adr191_27_unreachable_base_is_reported_and_skipped() {
    let f = fx();
    let unmounted = f.root.join("vol-unmounted/builds");
    f.declare(&[(&f.base1, "-wt"), (&unmounted, "-wt")]);
    let ctl = control(&f, &f.base1, "-wt");
    let orphan_id = "task-20261118-f001";
    f.molecule(orphan_id, "completed");
    let orphan = build_dir(&f.base1.join(format!("{orphan_id}-wt")));
    f.molecule(M, "completed");
    let wt = f.worktree(M);
    symlink(unmounted.join(format!("{M}-wt")), wt.join("target")).unwrap();

    let json = f.purge_json(&["--allow-unharvested"]);
    let bases = ext(&json)["bases"].as_array().unwrap();
    let b2 = bases
        .iter()
        .find(|b| b["base"] == unmounted.display().to_string())
        .unwrap_or_else(|| panic!("{json:#}"));
    assert_eq!(b2["state"], "unreachable", "{json:#}");
    let b1 = bases
        .iter()
        .find(|b| b["base"] == f.base1.display().to_string())
        .unwrap();
    assert_eq!(b1["state"], "scanned", "{json:#}");
    assert_reclaimed(&ctl, &json);
    assert_reclaimed(&orphan, &json);
    assert!(withheld_reasons(&json, &wt.join("target")).contains("dangling"));
}

// ---------------------------------------------------------------------------
// Configuration errors (D1)
// ---------------------------------------------------------------------------

fn assert_refused(f: &Fx, needle: &str, ctl: &Path) {
    let out = f.purge(&["--allow-unharvested"]);
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "pass should refuse ({needle}):\n{all}"
    );
    assert!(
        all.contains(needle),
        "condition {needle:?} not named:\n{all}"
    );
    assert!(
        payload_present(ctl),
        "a refused pass removes nothing ({needle})"
    );
}

/// 18 — an invalid base refuses the whole pass with the condition named.
#[test]
fn adr191_18_invalid_base_refuses_the_pass() {
    let f = fx();
    let ctl = control(&f, &f.base1, "");
    let wt = f.root.join("elsewhere").join(OTHER);
    fs::create_dir_all(wt.parent().unwrap()).unwrap();
    f.worktree_at(OTHER, &wt);
    let cases: Vec<(String, &str)> = vec![
        ("relative/builds".to_owned(), "absolute"),
        ("/".to_owned(), "is `/`"),
        (f.home.display().to_string(), "home directory"),
        (f.repo.display().to_string(), "repository root"),
        (
            f.root.display().to_string(),
            "ancestor of the repository root",
        ),
        (
            f.repo.join("sub").display().to_string(),
            "inside the repository",
        ),
        (wt.display().to_string(), "inside the worktree"),
    ];
    fs::create_dir_all(f.repo.join("sub")).unwrap();
    for (base, needle) in cases {
        f.declare_raw(&format!(
            "[worktree_reclaim]\nevict = [\"target\"]\n\n[[worktree_reclaim.external]]\n\
             root = \"target\"\nbase = {base:?}\n\n[[worktree_reclaim.external]]\n\
             root = \"target\"\nbase = {:?}\n",
            f.base1.display().to_string()
        ));
        assert_refused(&f, needle, &ctl);
    }
}

/// 28 — suffix, duplicate and nesting errors each refuse the pass.
#[test]
fn adr191_28_suffix_duplicate_and_nesting_errors_refuse_the_pass() {
    let f = fx();
    let ctl = control(&f, &f.base1, "");
    let long = "x".repeat(65);
    for (suffix, needle) in [
        ("a/b", "contains `/`"),
        ("..x", "contains `..`"),
        ("a\u{0}b", "NUL"),
        (long.as_str(), "longer than 64 bytes"),
    ] {
        f.declare(&[(&f.base1, ""), (&f.base2, suffix)]);
        assert_refused(&f, needle, &ctl);
    }
    // Same canonical base (one spelling through a link), same suffix.
    let alias = f.root.join("alias");
    symlink(&f.base1, &alias).unwrap();
    f.declare(&[(&f.base1, ""), (&alias, "")]);
    assert_refused(&f, "duplicate", &ctl);
    // A base strictly inside another.
    let inner = f.base1.join("inner");
    fs::create_dir_all(&inner).unwrap();
    f.declare(&[(&f.base1, ""), (&inner, "-wt")]);
    assert_refused(&f, "nested", &ctl);
    // A root not in `evict`.
    f.declare_raw(&format!(
        "[worktree_reclaim]\nevict = [\"target\"]\n\n[[worktree_reclaim.external]]\n\
         root = \"cache\"\nbase = {:?}\n",
        f.base1.display().to_string()
    ));
    assert_refused(&f, "not in [worktree_reclaim].evict", &ctl);
}

/// 28 — one base with suffixes `""` and `t`: `task-20261118-b7d7t` reads as
/// two molecules of this store. Withheld as ambiguous, as a link target
/// (D2) and as an orphan (D6), and untouched.
#[test]
fn adr191_28_ambiguous_child_is_withheld_as_link_and_as_orphan() {
    let long = "task-20261118-b7d7t";
    let short = "task-20261118-b7d7";
    // As a link target.
    let f = fx();
    f.declare(&[(&f.base1, ""), (&f.base1, "t")]);
    f.molecule(long, "completed");
    f.molecule(short, "completed");
    let wt = f.worktree(long);
    let dir = build_dir(&f.base1.join(long));
    symlink(&dir, wt.join("target")).unwrap();
    let json = f.purge_json(&["--allow-unharvested"]);
    assert!(
        withheld_reasons(&json, &dir).contains("ambiguous"),
        "{json:#}"
    );
    assert!(payload_present(&dir));
    // As an orphan.
    let g = fx();
    g.declare(&[(&g.base1, ""), (&g.base1, "t")]);
    g.molecule(long, "completed");
    g.molecule(short, "completed");
    let dir = build_dir(&g.base1.join(long));
    let json = g.purge_json(&["--allow-unharvested"]);
    assert!(
        withheld_reasons(&json, &dir).contains("ambiguous"),
        "{json:#}"
    );
    assert!(payload_present(&dir));
}
