// SPDX-License-Identifier: AGPL-3.0-only

//! Static guard: no `cs` command writes into a worker's pane anonymously
//! (COSMON #26 residual).
//!
//! The provenance event is only as good as its coverage. One call site that
//! reaches for the unattributed [`send_input`] port method puts text in a
//! composer that the ledger cannot explain — and that is invisible until
//! someone is staring at an unexplained composer, which is precisely the
//! situation issue #26 describes.
//!
//! So the coverage is a compile-adjacent fact rather than a review habit: this
//! test reddens the moment a production source in `cosmon-cli` calls
//! `send_input` instead of `send_input_observed`.
//!
//! It is a source scan, in the idiom of
//! `cosmon-rpp-adapter/tests/no_state_read_test.rs`. That is a real limitation
//! — it reads text, not semantics — but the drift it catches is textual: a new
//! nudge path copied from an old one. Nothing here claims to prove the absence
//! of injection; it proves the absence of the *known* anonymous spelling.
//!
//! [`send_input`]: cosmon_core::transport::TransportBackend::send_input

use std::fs;
use std::path::{Path, PathBuf};

/// The anonymous spelling. `send_input_observed` contains this substring, so
/// the scan below must exclude it explicitly rather than by luck.
const ANONYMOUS: &str = ".send_input(";

#[test]
fn no_cli_source_injects_anonymously() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();

    for file in rust_sources(&src) {
        let body = fs::read_to_string(&file).unwrap();
        for (n, line) in body.lines().enumerate() {
            if line.contains(ANONYMOUS) {
                offenders.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "every `cs` injection into a worker pane must declare its origin — \
         use `send_input_observed` with a stamp from \
         `cosmon_cli::injection_provenance` instead of the anonymous \
         `send_input`:\n  {}",
        offenders.join("\n  "),
    );
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    out
}
