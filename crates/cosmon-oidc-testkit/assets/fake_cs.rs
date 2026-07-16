// SPDX-License-Identifier: AGPL-3.0-only

//! `fake-cs` — minimal stand-in for `cs --json observe <id>` and
//! `cs --json nucleate <formula> ...`.
//!
//! Used by `cosmon-rpp-adapter` integration tests so the subprocess
//! envelope (ADR-080 §3.5 clause (e)) can be exercised without
//! rebuilding the full cosmon-cli or shelling out to a system binary.
//!
//! Behaviour:
//!
//! - `--json observe <id>`: looks for
//!   `<cwd>/.cosmon/state/molecules/<id>/state.json`. If found, prints
//!   it on stdout and exits 0. If not, exits 4 (the real `cs`'s
//!   not-found code).
//! - `--json nucleate <formula> [--kind <k>] [--var k=v] [--tag t]...`:
//!   synthesises a deterministic id `task-fakecs-<n>` (n = file count
//!   under `<cwd>/.cosmon/state/molecules/`), writes a minimal
//!   `state.json` to disk so a follow-up `observe` succeeds, and
//!   prints the cosmon-style nucleate JSON on stdout. Honours an empty
//!   formula → exit 2 to exercise the 409 mapping.
//! - `--json __dump_env`: prints every `COSMON_*` env var the child
//!   inherited as a JSON object on stdout (`{ "COSMON_FOO": "bar", … }`)
//!   and exits 0. Used by the subprocess env-hygiene test
//!   (idea-20260514-5c2e child A) to assert the §3.5 strip half — the
//!   asymmetry that an adapter `COSMON_STATE_DIR=/wrong` does NOT
//!   reach the child while the three envelope vars
//!   (`COSMON_API_REQUEST`, `COSMON_API_REQUEST_ID`,
//!   `COSMON_API_NUCLEON`) DO.
//! - Any other invocation exits 2.
//!
//! The cwd lookup is intentional and load-bearing: it is precisely
//! what proves tenant isolation.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    if argv.first() != Some(&"--json") {
        eprintln!("fake-cs: unknown invocation {args:?}");
        return ExitCode::from(2);
    }
    match argv.get(1).copied() {
        Some("observe") => match argv.get(2) {
            Some(id) => observe(id),
            None => ExitCode::from(2),
        },
        Some("nucleate") => nucleate(&argv[2..]),
        Some("run") => match argv.get(2) {
            Some(root) => run(root),
            None => ExitCode::from(2),
        },
        Some("__dump_env") => dump_env(),
        _ => {
            eprintln!("fake-cs: unknown invocation {args:?}");
            ExitCode::from(2)
        }
    }
}

/// `--json __dump_env` — emit every inherited `COSMON_*` env var as a
/// JSON object on stdout. Used by the subprocess env-hygiene test to
/// verify the §3.5 strip half: the asymmetry that adapter-side
/// `COSMON_STATE_DIR` etc. do NOT reach the child while the envelope
/// vars DO. The argv-form `__dump_env` keeps the entry point inside
/// the existing `--json <subcommand>` dispatch shape so the real `cs`
/// surface is unchanged.
fn dump_env() -> ExitCode {
    let mut keys: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("COSMON_"))
        .collect();
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    let pairs: Vec<(&str, serde_json_lite::Value)> = keys
        .iter()
        .map(|(k, v)| (k.as_str(), serde_json_lite::Value::String(v.clone())))
        .collect();
    let stdout = serde_json_lite::object(&pairs);
    print!("{stdout}");
    ExitCode::SUCCESS
}

fn observe(molecule_id: &str) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fake-cs: cannot read cwd: {e}");
            return ExitCode::from(1);
        }
    };
    let path = cwd
        .join(".cosmon")
        .join("state")
        .join("molecules")
        .join(molecule_id)
        .join("state.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!(
                "fake-cs: molecule {molecule_id} not found at {}: {e}",
                path.display()
            );
            ExitCode::from(4)
        }
    }
}

/// `--json run <root> [--max-actions N --max-depth N --max-molecules N
/// --timeout T]` — stand-in for the resident drain loop. Behaviour:
///
/// - root molecule missing (checked under the canonical
///   `<cwd>/.cosmon/state/fleets/default/molecules/` and the legacy
///   `<cwd>/.cosmon/state/molecules/`) → exit 4 (not found);
/// - a `drain-hold` file inside the root molecule dir → poll until it
///   disappears (max ~10 s) — lets tests hold the drain slot open to
///   exercise the 409 `drain_already_active` path deterministically;
/// - a `drain-exit` file inside the root molecule dir → exit with the
///   code it contains (lets tests exercise the named bound exits
///   90/91/92/124 without a real drain);
/// - otherwise prints `{"root": ..., "exit": "drained"}` and exits 0.
///
/// The bound flags are accepted and ignored — the route-level test
/// asserts them through the argument-construction unit tests of
/// `run_molecule_args`, not here.
fn run(root: &str) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fake-cs: cannot read cwd: {e}");
            return ExitCode::from(1);
        }
    };
    let base = cwd.join(".cosmon").join("state");
    let candidates = [
        base.join("fleets").join("default").join("molecules").join(root),
        base.join("molecules").join(root),
    ];
    let Some(mol_dir) = candidates.iter().find(|p| p.exists()) else {
        eprintln!("fake-cs: molecule {root} not found");
        return ExitCode::from(4);
    };
    let hold = mol_dir.join("drain-hold");
    let mut waited = 0u32;
    while hold.exists() && waited < 200 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        waited += 1;
    }
    if let Ok(text) = std::fs::read_to_string(mol_dir.join("drain-exit")) {
        if let Ok(code) = text.trim().parse::<u8>() {
            eprintln!("fake-cs: drain exiting with pinned code {code}");
            return ExitCode::from(code);
        }
    }
    let stdout = serde_json_lite::object(&[
        ("root", serde_json_lite::Value::String(root.to_owned())),
        (
            "exit",
            serde_json_lite::Value::String("drained".to_owned()),
        ),
    ]);
    print!("{stdout}");
    ExitCode::SUCCESS
}

fn nucleate(args: &[&str]) -> ExitCode {
    // First positional after `nucleate` is the formula name. We
    // recognise an empty argument list as "missing formula" → exit 2
    // to exercise the 409 wire-mapping.
    let Some(formula) = args.first() else {
        eprintln!("fake-cs: nucleate requires a formula");
        return ExitCode::from(2);
    };
    if formula.is_empty() {
        eprintln!("fake-cs: empty formula");
        return ExitCode::from(2);
    }
    let mut kind: String = "task".to_owned();
    let mut tags: Vec<String> = Vec::new();
    let mut variables: serde_json_lite::Map = serde_json_lite::Map::new();
    let mut i = 1;
    while i < args.len() {
        match args[i] {
            "--kind" => {
                if let Some(v) = args.get(i + 1) {
                    kind = (*v).to_owned();
                    i += 2;
                } else {
                    return ExitCode::from(2);
                }
            }
            "--var" => {
                if let Some(kv) = args.get(i + 1) {
                    if let Some((k, v)) = kv.split_once('=') {
                        variables.insert(k.to_owned(), v.to_owned());
                    } else {
                        eprintln!("fake-cs: invalid --var (expected key=value): {kv}");
                        return ExitCode::from(2);
                    }
                    i += 2;
                } else {
                    return ExitCode::from(2);
                }
            }
            "--tag" => {
                if let Some(v) = args.get(i + 1) {
                    tags.push((*v).to_owned());
                    i += 2;
                } else {
                    return ExitCode::from(2);
                }
            }
            other => {
                eprintln!("fake-cs: unknown flag {other}");
                return ExitCode::from(2);
            }
        }
    }

    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fake-cs: cannot read cwd: {e}");
            return ExitCode::from(1);
        }
    };
    let molecules_dir = cwd.join(".cosmon").join("state").join("molecules");
    let n = count_existing_molecules(&molecules_dir);
    let id = format!("task-fakecs-{n:04x}");
    let mol_dir = molecules_dir.join(&id);
    if let Err(e) = std::fs::create_dir_all(&mol_dir) {
        eprintln!("fake-cs: cannot create {}: {e}", mol_dir.display());
        return ExitCode::from(1);
    }
    let state_json = serde_json_lite::object(&[
        ("id", serde_json_lite::Value::String(id.clone())),
        ("kind", serde_json_lite::Value::String(kind.clone())),
        ("status", serde_json_lite::Value::String("pending".to_owned())),
        (
            "formula",
            serde_json_lite::Value::String((*formula).to_owned()),
        ),
        ("tags", serde_json_lite::Value::Array(
            tags.iter().map(|t| serde_json_lite::Value::String(t.clone())).collect(),
        )),
        ("variables", serde_json_lite::Value::Object(variables.clone())),
    ]);
    if let Err(e) = std::fs::write(mol_dir.join("state.json"), &state_json) {
        eprintln!("fake-cs: cannot write state.json: {e}");
        return ExitCode::from(1);
    }
    // The real `cs --json nucleate` emits a single JSON object on
    // stdout. Match the shape the route handler expects.
    let stdout = serde_json_lite::object(&[
        ("id", serde_json_lite::Value::String(id)),
        (
            "formula",
            serde_json_lite::Value::String((*formula).to_owned()),
        ),
        ("status", serde_json_lite::Value::String("active".to_owned())),
    ]);
    print!("{stdout}");
    ExitCode::SUCCESS
}

fn count_existing_molecules(dir: &PathBuf) -> usize {
    std::fs::read_dir(dir).map(|it| it.count()).unwrap_or(0)
}

/// Tiny inline JSON encoder. Avoids pulling serde_json into a binary
/// that already depends on it transitively (the integration build
/// already has serde_json), but keeping the surface here zero-dep
/// keeps the binary tiny — the subprocess invocation is hot-path.
mod serde_json_lite {
    use std::fmt;

    pub type Map = std::collections::BTreeMap<String, String>;

    pub enum Value {
        String(String),
        Array(Vec<Value>),
        Object(Map),
    }

    pub fn object(pairs: &[(&str, Value)]) -> String {
        let mut s = String::from("{");
        for (i, (k, v)) in pairs.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push('"');
            push_escaped(&mut s, k);
            s.push_str("\":");
            push_value(&mut s, v);
        }
        s.push('}');
        s
    }

    fn push_value(s: &mut String, v: &Value) {
        match v {
            Value::String(t) => {
                s.push('"');
                push_escaped(s, t);
                s.push('"');
            }
            Value::Array(items) => {
                s.push('[');
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    push_value(s, it);
                }
                s.push(']');
            }
            Value::Object(m) => {
                s.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    s.push('"');
                    push_escaped(s, k);
                    s.push_str("\":\"");
                    push_escaped(s, v);
                    s.push('"');
                }
                s.push('}');
            }
        }
    }

    fn push_escaped(s: &mut String, t: &str) {
        for c in t.chars() {
            match c {
                '"' => s.push_str("\\\""),
                '\\' => s.push_str("\\\\"),
                '\n' => s.push_str("\\n"),
                '\r' => s.push_str("\\r"),
                '\t' => s.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    let _ = std::fmt::Write::write_fmt(s, format_args!("\\u{:04x}", c as u32));
                }
                _ => s.push(c),
            }
        }
    }

    impl fmt::Display for Value {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let mut s = String::new();
            push_value(&mut s, self);
            f.write_str(&s)
        }
    }
}
