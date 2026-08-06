// SPDX-License-Identifier: AGPL-3.0-only

//! Validates the cockpit surface event log and its committed JSON projection.

use std::env;
use std::fs;
use std::path::PathBuf;

use cosmon_surface_canon::{parse_cockpit_canon, render_cockpit_json};

const CANON_FILE: &str = "data/cockpit_views.txt";
const JSON_FILE: &str = "data/cockpit_views.v1.json";

fn main() {
    println!("cargo:rerun-if-changed={CANON_FILE}");
    println!("cargo:rerun-if-changed={JSON_FILE}");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = match env::var("CARGO_MANIFEST_DIR") {
        Ok(value) => PathBuf::from(value),
        Err(err) => panic!("CARGO_MANIFEST_DIR is unavailable: {err}"),
    };
    let raw = read(&manifest_dir, CANON_FILE);
    let events = match parse_cockpit_canon(&raw, CANON_FILE) {
        Ok(value) => value,
        Err(err) => panic!("{err}"),
    };
    assert!(
        !events.is_empty(),
        "{CANON_FILE} contains no views; the cockpit boundary cannot be empty"
    );

    let rendered = match render_cockpit_json(&events) {
        Ok(value) => value,
        Err(err) => panic!("{err}"),
    };
    let committed = read(&manifest_dir, JSON_FILE);
    assert_eq!(
        committed, rendered,
        "{JSON_FILE} drifted from {CANON_FILE}; regenerate the JSON projection"
    );
}

fn read(manifest_dir: &std::path::Path, relative: &str) -> String {
    let path = manifest_dir.join(relative);
    match fs::read_to_string(&path) {
        Ok(value) => value,
        Err(err) => panic!("failed to read {}: {err}", path.display()),
    }
}
