// SPDX-License-Identifier: AGPL-3.0-only

//! Cosmon-side golden for the generated API-reference blocks — the
//! cross-repo tripwire (avatar-surface A3).
//!
//! The smithy doc lives in another repository, so its « git diff
//! vide après re-gen » gate cannot run in cosmon CI. This golden pins
//! the rendered blocks HERE: appending a route to the canon fails this
//! test until the golden is re-blessed — and re-blessing it is the
//! reminder that `docs/specs/cosmon-rpp-api-reference.md` (smithy)
//! must be regenerated in the same gesture.
//!
//! Bless: `UPDATE_GOLDENS=1 cargo test -p xtask --test golden`.

use std::path::{Path, PathBuf};

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens/api-ref-blocks.md")
}

#[test]
fn rendered_blocks_match_golden() {
    let canon_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(xtask::CANON_RELATIVE);
    let canon_text = std::fs::read_to_string(&canon_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", canon_path.display()));
    let events = cosmon_surface_canon::parse_canon(&canon_text, xtask::CANON_RELATIVE)
        .expect("canon parses");

    let mut rendered = String::new();
    for (name, content) in xtask::render_blocks(&events).expect("blocks render") {
        rendered.push_str(&format!("===== {name} =====\n{content}\n"));
    }

    let path = golden_path();
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().expect("goldens dir")).expect("mkdir");
        std::fs::write(&path, &rendered).expect("write golden");
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e} — bless with UPDATE_GOLDENS=1", path.display()));
    assert_eq!(
        rendered, committed,
        "generated API-reference blocks drifted from the golden — the canon changed; \
         re-bless (UPDATE_GOLDENS=1) AND regenerate the smithy doc \
         (`cargo xtask gen-api-ref <smithy>/docs/specs/cosmon-rpp-api-reference.md`)"
    );
}
