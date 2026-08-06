// SPDX-License-Identifier: AGPL-3.0-only

//! Cross-check the external cockpit contract against the live clap tree.

use std::collections::BTreeSet;
use std::error::Error;
use std::path::PathBuf;
use std::process::Command;

use cosmon_surface_canon::parse_cockpit_canon;

#[test]
fn every_cockpit_view_names_a_live_cs_command() -> Result<(), Box<dyn Error>> {
    let canon_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../cosmon-cockpit/data/cockpit_views.txt");
    let raw = std::fs::read_to_string(&canon_path)?;
    let events = parse_cockpit_canon(&raw, &canon_path.display().to_string())?;

    let output = Command::new(env!("CARGO_BIN_EXE_cs"))
        .args(["__help-tree", "--all"])
        .output()?;
    assert!(
        output.status.success(),
        "`cs __help-tree --all` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let command_paths: BTreeSet<&str> = std::str::from_utf8(&output.stdout)?.lines().collect();

    for event in &events {
        let path = source_command_path(&event.source_cs, &command_paths);
        assert!(
            path.is_some(),
            "cockpit view {:?} names no live command path in {:?}",
            event.view.as_str(),
            event.source_cs
        );
    }
    Ok(())
}

fn source_command_path(source: &str, command_paths: &BTreeSet<&str>) -> Option<String> {
    let words: Vec<&str> = source.split_ascii_whitespace().skip(1).collect();
    (1..=words.len()).rev().find_map(|len| {
        let candidate = words[..len].join(" ");
        command_paths
            .contains(candidate.as_str())
            .then_some(candidate)
    })
}
