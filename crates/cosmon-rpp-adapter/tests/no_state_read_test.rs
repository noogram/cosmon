// SPDX-License-Identifier: AGPL-3.0-only

//! R1 instrumentation — `cosmon-rpp-adapter` MUST NOT write under
//! `<state_dir>/state/`. The adapter reads sealed JWKS / nucleon map
//! / deny-list / rate-limiter under `<state_dir>/security/...` and
//! `<state_dir>/security/oidc-rate-limit/`, but never under
//! `state.json` or `events.jsonl`. Source-level static check —
//! `strace`-grade enforcement is a V1 follow-up (ADR-080 §12 R1).

use std::fs;
use std::path::Path;

#[test]
fn no_module_writes_to_state_json_or_events_jsonl() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = crate_root.join("src");
    let mut offenders = vec![];
    for entry in walkdir(&src) {
        if entry
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "rs")
        {
            let body = fs::read_to_string(&entry).unwrap();
            // Heuristic: forbid any direct `state.json` / `events.jsonl`
            // string mentions in production sources. Tests that *read*
            // such files for verification land under `tests/` and are
            // not scanned here.
            if body.contains("state.json") || body.contains("events.jsonl") {
                offenders.push(entry.display().to_string());
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "RPP adapter source must never reference state.json / events.jsonl directly: {offenders:?}",
    );
}

fn walkdir(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    if !root.exists() {
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path);
                }
            }
        }
    }
    out
}
