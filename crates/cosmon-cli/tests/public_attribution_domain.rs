// SPDX-License-Identifier: AGPL-3.0-only

//! Regression for the public Noogram attribution domain.
//!
//! Historical and defensive-DNS documentation may legitimately mention
//! `noogram.dev`; these shipped maker/byline slots may not. Keeping the list
//! explicit makes the public perimeter reviewable instead of pretending every
//! occurrence of the defensive domain has the same semantics.

use std::path::Path;

#[test]
fn shipped_attribution_slots_use_noogram_org() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let public_slots = [
        "AGENTS.md",
        "LICENSE",
        "NOTICE",
        "THESIS.md",
        "docs/book/src/explanation/cosmon-and-noogram.md",
        "docs/style/naming.md",
        "docs/vision/north-star.md",
        "evidence/calibration-corpus/schema.json",
    ];

    for relative in public_slots {
        let path = root.join(relative);
        let body = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!("read public attribution slot {}: {error}", path.display())
        });
        assert!(
            !body.contains("noogram.dev"),
            "public attribution slot {} still carries noogram.dev",
            path.display()
        );
        assert!(
            body.contains("noogram.org"),
            "public attribution slot {} must carry noogram.org",
            path.display()
        );
    }
}
