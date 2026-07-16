// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for `cs inspect` and `cs artifacts audit` (ADR-057).
//!
//! Each test spins up a temporary git repository containing a
//! representative `.cosmon/artifact-map.toml` and a handful of fixture
//! paths. Tests drive the compiled `cs` binary as a subprocess so the
//! full CLI wiring is exercised (argument parsing, file discovery,
//! JSON emission, exit codes).

use std::fs;
use std::path::Path;
use std::process::Command;

const MAP_TOML: &str = r#"
[chronicle]
location = ["docs/lore/**/*.md"]
audience = "author+agent"

[adr]
location = ["docs/adr/**/*.md"]
audience = "public"

[addl]
location = ["docs/addl/<name>/**/*"]
audience = "partner:<name>"

[github-surface]
location = ["docs/surfaces/**/*.md", "STATUS.md", "ISSUES.md"]
audience = "solo"

[deliberation]
location = [".cosmon/state/fleets/*/molecules/*/synthesis.md"]
audience = "author+agent"

[code]
location = ["**/*"]
audience = "public"
"#;

fn cs() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cs"))
}

fn seed_galaxy(root: &Path) {
    fs::create_dir_all(root.join(".cosmon")).unwrap();
    fs::write(root.join(".cosmon/artifact-map.toml"), MAP_TOML).unwrap();
    // Also write a minimal config so cosmon walk-up discovery is happy.
    fs::write(
        root.join(".cosmon/config.toml"),
        r#"[project]
project_id = "test"
"#,
    )
    .unwrap();
}

fn inspect(root: &Path, path: &str, extra: &[&str]) -> std::process::Output {
    let mut cmd = cs();
    cmd.current_dir(root);
    cmd.arg("inspect").arg(path);
    for a in extra {
        cmd.arg(a);
    }
    cmd.output().expect("cs inspect must run")
}

#[test]
fn inspect_chronicle_classifies_as_author_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    seed_galaxy(root);

    let out = inspect(root, "docs/lore/2026-04-20-le-triangle.md", &["--json"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = stdout
        .lines()
        .next()
        .and_then(|l| serde_json::from_str(l).ok())
        .unwrap_or_else(|| panic!("not JSON: {stdout}"));
    assert_eq!(v["genre"], "chronicle");
    assert_eq!(v["audience"], "author+agent");
    assert_eq!(v["residence"], "team");
}

#[test]
fn inspect_adr_classifies_as_public() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    seed_galaxy(root);

    let out = inspect(root, "docs/adr/052-one-ledger.md", &["--json"]);
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).lines().next().unwrap()).unwrap();
    assert_eq!(v["genre"], "adr");
    assert_eq!(v["audience"], "public");
    assert_eq!(v["residence"], "team");
}

#[test]
fn inspect_github_surface_defaults_to_solo() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    seed_galaxy(root);

    let out = inspect(root, "docs/surfaces/issues.md", &["--json"]);
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).lines().next().unwrap()).unwrap();
    assert_eq!(v["genre"], "github-surface");
    assert_eq!(v["audience"], "solo");
    assert_eq!(v["residence"], "solo");
}

#[test]
fn inspect_partner_capture_resolves_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    seed_galaxy(root);

    let out = inspect(root, "docs/addl/operator-b/videos/demo.mp4", &["--json"]);
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).lines().next().unwrap()).unwrap();
    assert_eq!(v["genre"], "addl");
    assert_eq!(v["audience"], "partner:operator-b");
    assert_eq!(v["residence"], "team");
}

#[test]
fn inspect_unclassified_falls_back_to_code() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    seed_galaxy(root);

    let out = inspect(root, "random/weird/path.xyz", &["--json"]);
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).lines().next().unwrap()).unwrap();
    assert_eq!(v["genre"], "code");
    assert_eq!(v["audience"], "public");
    assert_eq!(v["residence"], "team");
}

#[test]
fn audit_totality_holds_for_mixed_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    seed_galaxy(root);

    // Seed a tiny git repo with one file of each genre.
    fs::create_dir_all(root.join("docs/lore")).unwrap();
    fs::create_dir_all(root.join("docs/adr")).unwrap();
    fs::create_dir_all(root.join("docs/addl/bob")).unwrap();
    fs::create_dir_all(root.join("docs/surfaces")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("docs/lore/a.md"), "c").unwrap();
    fs::write(root.join("docs/adr/001-x.md"), "c").unwrap();
    fs::write(root.join("docs/addl/bob/deck.pdf"), "c").unwrap();
    fs::write(root.join("docs/surfaces/issues.md"), "c").unwrap();
    fs::write(root.join("STATUS.md"), "c").unwrap();
    fs::write(root.join("src/main.rs"), "c").unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("git must run")
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "t@t.t"]);
    git(&["config", "user.name", "t"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "seed"]);

    let out = cs()
        .current_dir(root)
        .args(["artifacts", "audit", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    // The last line is the summary envelope.
    let summary_line = stdout.lines().last().expect("at least one line");
    let summary: serde_json::Value = serde_json::from_str(summary_line).unwrap();
    assert_eq!(summary["kind"], "summary");
    assert_eq!(summary["invariants_hold"], true);
    assert_eq!(summary["unclassified_count"], 0);
}

#[test]
fn audit_flags_violations_when_map_is_empty() {
    // An empty map (no `code` catch-all) would fail I1 totality. Our
    // default_code_catchall kicks in only when the TOML is *absent*; a
    // TOML that declares no genres parses to zero entries, so
    // classification returns None for every path.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join(".cosmon")).unwrap();
    fs::write(
        root.join(".cosmon/artifact-map.toml"),
        "# deliberately empty\n",
    )
    .unwrap();
    fs::write(root.join("hello.txt"), "hi").unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("git must run")
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "t@t.t"]);
    git(&["config", "user.name", "t"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "seed"]);

    let out = cs()
        .current_dir(root)
        .args(["artifacts", "audit", "--json"])
        .output()
        .unwrap();
    // Non-zero exit: violations exist.
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    let summary: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(summary["invariants_hold"], false);
    assert!(summary["unclassified_count"].as_u64().unwrap() > 0);
}
