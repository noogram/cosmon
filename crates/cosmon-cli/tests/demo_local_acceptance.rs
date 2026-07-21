// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end acceptance gate for the **local-default** walking
//! skeleton.
//!
//! # What this test proves
//!
//! A bare `cs tackle <id>` — **no `--adapter` flag** — routes to the
//! built-in `local` Adapter, which drives cosmon's *own* harness spine
//! (`cosmon-agent-harness::run_loop`) through the existing
//! `OpenAIProvider` pointed at Ollama's OpenAI-compat `/v1` endpoint.
//! The molecule is executed end-to-end and a non-empty `synthesis.md`
//! lands in the molecule's state directory —
//!
//! `Ollama /v1 → OpenAIProvider → cosmon-agent-harness → cs tackle`
//!
//! — landing three canonical rows on `events.jsonl`:
//!
//! 1. `adapter_selected` with `adapter_name = "local"` and
//!    `selection_source = "default"` — the cat-test witness that the
//!    DEFAULT (no flag) is local, NOT claude.
//! 2. `worker_spawned` with `adapter_name = "local"` — the structural
//!    proof the validated name traversed the spawn seam in-process.
//! 3. `molecule_step_completed` (or `molecule_completed`) — proof the
//!    in-process agent loop returned Ok and the completion-emit ran.
//!
//! Plus a non-empty `synthesis.md` carrying the `# local synthesis`
//! header.
//!
//! # ZERO Claude Code in the default path
//!
//! This is the whole point of the walking skeleton: the default route
//! spawns NO `claude` subprocess. The loop lives inside the `cs tackle`
//! address space; only an HTTP request to `localhost:11434` leaves the
//! process. The architect's smoke recipe (`pgrep -f 'claude'; echo
//! "exit=$?"` ⇒ `exit=1`) is the shell-level twin of this gate.
//!
//! # Double gate (opt-in, no CI surprise)
//!
//! - `#[ignore]` — opt-in (`cargo test … -- --ignored`).
//! - `COSMON_LOCAL_DEMO=1` env var + a reachable `ollama serve` at
//!   `COSMON_LOCAL_BASE_URL` (default `http://localhost:11434`). Absent
//!   or unreachable → loud skip, never a hang.
//!
//! Run with:
//!
//! ```bash
//! ollama serve &
//! export COSMON_LOCAL_DEMO=1
//! # optional: export COSMON_LOCAL_MODEL=qwen3:8b
//! cargo test -p cosmon-cli --test demo_local_acceptance -- --ignored --nocapture
//! ```
//!
//! # Why a sibling of `demo_llama_acceptance.rs`, not an edit of it
//!
//! `demo_llama_acceptance.rs` is `#![cfg(feature = "llama")]` and
//! exercises the FFI GGUF path via `cs demo`. The local-default path
//! is feature-flag-free (it rides the always-compiled `OpenAIProvider`
//! HTTP envelope) and uses a bare `cs tackle`. godin §7 — each file
//! pins one contract; this file pins the local-first DEFAULT contract.

use std::fs;
use std::path::Path;
use std::process::Command;

/// Opt-in switch. Absent → the test skips loudly.
const DEMO_ENV: &str = "COSMON_LOCAL_DEMO";

/// Ollama OpenAI-compat host root. Matches `DEFAULT_LOCAL_BASE_URL` in
/// `cs tackle`'s `spawn_local_session`.
const DEFAULT_BASE_URL: &str = "http://localhost:11434";

fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_STATE_DIR")
        // Hermetic adapter resolution: this test pins that the *built-in*
        // floor is `local`, so the env-var tier ($COSMON_DEFAULT_ADAPTER)
        // and the global-config tier ($COSMON_CONFIG_HOME/cosmon/config.toml)
        // must not leak in from the operator's machine. The operator's real
        // `~/.config/cosmon/config.toml` carries a `default = "claude"` while
        // on critical tasks; without this isolation the bare-tackle default
        // resolves to claude and the local-first contract pin fails. The
        // redirect target is a stable nonexistent path under the system temp
        // dir — `read_to_string` fails → resolver falls through to the floor.
        .env_remove("COSMON_DEFAULT_ADAPTER")
        .env(
            "COSMON_CONFIG_HOME",
            std::env::temp_dir().join("cosmon-test-xdg-isolated-local-acceptance"),
        );
    cmd
}

/// Resolve the base URL the test should health-check, honouring the
/// same override chain as the adapter (`COSMON_LOCAL_BASE_URL` →
/// default).
fn base_url() -> String {
    std::env::var("COSMON_LOCAL_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned())
}

/// Best-effort liveness probe: is Ollama answering at `<base>/api/tags`?
/// Uses a short `curl` rather than a Rust HTTP client so the test stays
/// dependency-free. A non-success exit means "skip", never "fail".
fn ollama_reachable(base: &str) -> bool {
    Command::new("curl")
        .args([
            "-s",
            "-o",
            "/dev/null",
            "-m",
            "3",
            "-w",
            "%{http_code}",
            &format!("{base}/api/tags"),
        ])
        .output()
        .ok()
        .is_some_and(|o| {
            let code = String::from_utf8_lossy(&o.stdout);
            code.trim() == "200"
        })
}

/// Initialise a self-contained cosmon project rooted at `dir`.
///
/// Note: **no `[adapters.default]` row** — the whole point is that the
/// *built-in* fallback is `local`. The config carries only the project
/// id and the task-work formula.
fn setup_project(dir: &Path) {
    let cosmon_dir = dir.join(".cosmon");
    fs::create_dir_all(cosmon_dir.join("state")).unwrap();
    fs::create_dir_all(cosmon_dir.join("formulas")).unwrap();

    let cfg = "[project]\nproject_id = \"demo-local-acceptance-821f\"\n";
    fs::write(cosmon_dir.join("config.toml"), cfg).unwrap();

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let formula_src = manifest
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join(".cosmon/formulas/task-work.formula.toml"))
        .expect("walk-up to workspace root must succeed");
    let formula_body = fs::read_to_string(&formula_src)
        .unwrap_or_else(|e| panic!("read task-work formula at {}: {e}", formula_src.display()));
    fs::write(
        cosmon_dir.join("formulas").join("task-work.formula.toml"),
        formula_body,
    )
    .unwrap();

    let _ = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .output();
    fs::write(dir.join(".gitignore"), ".cosmon/state/\n").unwrap();
    fs::write(dir.join("README.md"), "# demo-local-acceptance\n").unwrap();
    let _ = Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=test@cosmon.test",
            "-c",
            "user.name=cosmon-test",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(dir)
        .output();
}

fn read_events_rows(state_dir: &Path) -> (String, Vec<serde_json::Value>) {
    let raw = fs::read_to_string(state_dir.join("events.jsonl")).unwrap_or_default();
    let rows: Vec<serde_json::Value> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    (raw, rows)
}

/// Capture the molecule id from `cs nucleate --json` stdout.
fn nucleate_molecule(dir: &Path) -> String {
    let out = cs()
        .current_dir(dir)
        .args([
            "nucleate",
            "task-work",
            "--json",
            "--var",
            // Must leave a real worktree deliverable: the Jesse #4 real-work
            // floor refuses to book `completed` on chatter alone, so the demo
            // task explicitly writes a file rather than merely replying.
            "topic=Create a file README.md containing a haiku.",
        ])
        .output()
        .expect("spawn cs nucleate");
    assert!(
        out.status.success(),
        "cs nucleate must succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // NDJSON: scan every line for an object carrying a molecule id.
    for line in stdout.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            for key in ["molecule_id", "id", "mol_id"] {
                if let Some(id) = v.get(key).and_then(serde_json::Value::as_str) {
                    if id.starts_with("task-") {
                        return id.to_owned();
                    }
                }
            }
        }
    }
    panic!("could not parse molecule id from cs nucleate output:\n{stdout}");
}

/// **The local-first walking-skeleton acceptance gate.**
///
/// A bare `cs tackle <id>` (no `--adapter`) must route to `local`,
/// drive the in-process loop against Ollama, complete the molecule,
/// and write a non-empty `synthesis.md` — with ZERO `claude` process.
#[test]
#[ignore = "requires COSMON_LOCAL_DEMO=1 and a reachable `ollama serve`"]
fn bare_tackle_routes_local_and_writes_synthesis() {
    if std::env::var(DEMO_ENV).ok().as_deref() != Some("1") {
        eprintln!("[skip] {DEMO_ENV} != 1 — set {DEMO_ENV}=1 and run `ollama serve` to opt in");
        return;
    }
    let base = base_url();
    if !ollama_reachable(&base) {
        eprintln!("[skip] Ollama not reachable at {base}/api/tags — start `ollama serve`");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    setup_project(tmp.path());
    let state_dir = tmp.path().join(".cosmon").join("state");

    let mol_id = nucleate_molecule(tmp.path());

    // The load-bearing invocation: NO `--adapter` flag.
    let out = cs()
        .current_dir(tmp.path())
        .args(["tackle", &mol_id])
        .output()
        .expect("spawn cs tackle");
    assert!(
        out.status.success(),
        "bare `cs tackle` must succeed against local Ollama.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let (events_raw, rows) = read_events_rows(&state_dir);

    let adapter_selected = rows.iter().find(|r| {
        r.get("type").and_then(serde_json::Value::as_str) == Some("adapter_selected")
            && r.get("adapter_name").and_then(serde_json::Value::as_str) == Some("local")
    });
    assert!(
        adapter_selected.is_some(),
        "events.jsonl must contain `adapter_selected` with adapter_name=\"local\" \
         — the bare-tackle default must be local, not claude.\n\nEvents:\n{events_raw}",
    );
    // And the source must be `default` (no flag, no config row).
    if let Some(sel) = adapter_selected {
        let source = sel
            .get("selection_source")
            .and_then(|s| s.get("source"))
            .and_then(serde_json::Value::as_str);
        assert_eq!(
            source,
            Some("default"),
            "the local adapter must be reached via the built-in DEFAULT, not a flag/config: {sel}",
        );
    }

    let worker_spawned = rows.iter().find(|r| {
        r.get("type").and_then(serde_json::Value::as_str) == Some("worker_spawned")
            && r.get("adapter_name").and_then(serde_json::Value::as_str) == Some("local")
    });
    assert!(
        worker_spawned.is_some(),
        "events.jsonl must contain `worker_spawned` with adapter_name=\"local\".\n\nEvents:\n{events_raw}",
    );

    let step_or_completed = rows.iter().find(|r| {
        let kind = r.get("type").and_then(serde_json::Value::as_str);
        kind == Some("molecule_step_completed") || kind == Some("molecule_completed")
    });
    assert!(
        step_or_completed.is_some(),
        "events.jsonl must contain `molecule_step_completed` or `molecule_completed` \
         — the in-process loop must drive the completion-emit.\n\nEvents:\n{events_raw}",
    );

    let mol_dir = locate_molecule_dir(&state_dir)
        .expect("must find the molecule's state directory under <state>/fleets/<fleet>/molecules/");
    let synthesis = fs::read_to_string(mol_dir.join("synthesis.md")).unwrap_or_else(|e| {
        panic!(
            "synthesis.md missing under {} ({e}). spawn_local_session must write it.",
            mol_dir.display(),
        )
    });
    assert!(
        !synthesis.trim().is_empty(),
        "synthesis.md must be non-empty (the local loop returned no content)",
    );
    assert!(
        synthesis.contains("local synthesis"),
        "synthesis.md must carry the `# local synthesis` header. Got:\n{synthesis}",
    );
}

/// Walk the on-disk layout and return the single molecule directory.
fn locate_molecule_dir(state_dir: &Path) -> Option<std::path::PathBuf> {
    let fleets_dir = state_dir.join("fleets");
    if let Ok(entries) = fs::read_dir(&fleets_dir) {
        for fleet in entries.flatten() {
            let molecules = fleet.path().join("molecules");
            if let Ok(mol_entries) = fs::read_dir(&molecules) {
                let dirs: Vec<_> = mol_entries
                    .flatten()
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.path())
                    .collect();
                if dirs.len() == 1 {
                    return Some(dirs.into_iter().next().unwrap());
                }
            }
        }
    }
    let flat = state_dir.join("molecules");
    if let Ok(entries) = fs::read_dir(&flat) {
        let dirs: Vec<_> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.path())
            .collect();
        if dirs.len() == 1 {
            return Some(dirs.into_iter().next().unwrap());
        }
    }
    None
}
