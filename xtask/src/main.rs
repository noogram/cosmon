// SPDX-License-Identifier: AGPL-3.0-only

//! `cargo xtask` — repo tooling entry point. See `xtask/src/lib.rs`
//! for the gen-api-ref mechanics.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn workspace_root() -> PathBuf {
    // This crate lives at `<workspace>/xtask`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent dir")
        .to_path_buf()
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: cargo xtask gen-api-ref [--check] <target.md>\n\n\
         Re-renders the generated blocks (routes-v1, bijection-8p) of the\n\
         smithy API reference from {} .\n\
         --check: exit 1 if a re-render would change the file (gate mode).",
        xtask::CANON_RELATIVE,
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    if it.next().map(String::as_str) != Some("gen-api-ref") {
        return usage();
    }
    let mut check = false;
    let mut target: Option<&str> = None;
    for arg in it {
        match arg.as_str() {
            "--check" => check = true,
            other if target.is_none() && !other.starts_with('-') => target = Some(other),
            _ => return usage(),
        }
    }
    let Some(target) = target else {
        return usage();
    };

    let canon_path = workspace_root().join(xtask::CANON_RELATIVE);
    let canon_text = match std::fs::read_to_string(&canon_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("gen-api-ref: read {}: {e}", canon_path.display());
            return ExitCode::FAILURE;
        }
    };
    let document = match std::fs::read_to_string(target) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("gen-api-ref: read {target}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let regenerated = match xtask::regenerate(&canon_text, &document) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("gen-api-ref: {e}");
            return ExitCode::FAILURE;
        }
    };

    if regenerated == document {
        eprintln!("gen-api-ref: {target} is up to date with the canon");
        return ExitCode::SUCCESS;
    }
    if check {
        eprintln!(
            "gen-api-ref: {target} is STALE — re-run `cargo xtask gen-api-ref {target}` \
             and commit the result"
        );
        return ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(target, regenerated) {
        eprintln!("gen-api-ref: write {target}: {e}");
        return ExitCode::FAILURE;
    }
    eprintln!("gen-api-ref: {target} regenerated from the canon");
    ExitCode::SUCCESS
}
