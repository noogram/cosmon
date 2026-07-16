// SPDX-License-Identifier: AGPL-3.0-only

//! Wheat-paste rule tests for [`render_canonical`].
//!
//! The rule:
//!
//! > `cs peek --snapshot` on the same fleet state, captured from
//! > iPhone-Blink, iPad-Blink, MacBook-Ghostty, and AWS-SSH-tmux, must
//! > diff **byte-for-byte to zero**.
//!
//! We cannot spin up four real devices in CI, so this suite freezes the
//! equivalent invariant: the rendering function is pure. Same input +
//! same config = same bytes, regardless of environment, locale, or
//! terminal dimensions. An `insta` snapshot also locks the exact byte
//! sequence so a reviewer sees any drift as a diff — `cargo insta
//! review` is the place to sign off on an intentional change.

use cosmon_observability::fixture::canonical_snapshot;
use cosmon_observability::render::{render_canonical, SnapshotConfig, CANONICAL_WIDTH};

/// The exact bytes produced by [`render_canonical`] for the canonical
/// fixture. Intentionally locked via `insta` — any change forces a
/// conscious `cargo insta review` acknowledgement.
#[test]
fn canonical_snapshot_matches_insta_lock() {
    let out = render_canonical(&canonical_snapshot(), &SnapshotConfig::default());
    insta::assert_snapshot!(out);
}

/// No environment variable can perturb the canonical output. This is
/// the wheat-paste rule projected into CI: no `$COLUMNS`, `$ROWS`,
/// `$TERM`, `$LC_*`, or `$LANG` changes the bytes.
#[test]
fn canonical_is_invariant_under_tty_envvars() {
    let baseline = render_canonical(&canonical_snapshot(), &SnapshotConfig::default());

    let pairs = [
        ("COLUMNS", "40"),
        ("COLUMNS", "200"),
        ("ROWS", "10"),
        ("ROWS", "80"),
        ("TERM", "dumb"),
        ("TERM", "xterm-256color"),
        ("LANG", "C"),
        ("LANG", "fr_FR.UTF-8"),
        ("LC_ALL", "C"),
    ];
    for (k, v) in pairs {
        std::env::set_var(k, v);
        let out = render_canonical(&canonical_snapshot(), &SnapshotConfig::default());
        assert_eq!(
            baseline, out,
            "{k}={v} perturbed canonical output — the rendering path read an env var",
        );
        std::env::remove_var(k);
    }
}

/// Every non-empty line fits the canonical wall exactly. The operator
/// may see the canvas letterboxed or horizontally panned, but never
/// partially truncated by the renderer itself.
#[test]
fn canonical_every_line_is_canonical_width_wide() {
    let out = render_canonical(&canonical_snapshot(), &SnapshotConfig::default());
    for (i, line) in out.lines().enumerate() {
        assert_eq!(
            line.chars().count(),
            CANONICAL_WIDTH,
            "line {i} has wrong width ({}): {line:?}",
            line.chars().count(),
        );
    }
}

/// Byte identity across repeated calls — the hardest test the rule
/// projects into CI. If this ever fails, the renderer acquired a
/// hidden dependency (clock, randomness, global state).
#[test]
fn canonical_is_byte_identical_across_repeated_calls() {
    let a = render_canonical(&canonical_snapshot(), &SnapshotConfig::default()).into_bytes();
    let b = render_canonical(&canonical_snapshot(), &SnapshotConfig::default()).into_bytes();
    let c = render_canonical(&canonical_snapshot(), &SnapshotConfig::default()).into_bytes();
    assert_eq!(a, b);
    assert_eq!(b, c);
}

/// Output is pure ASCII — `bytes == chars == columns`. Any non-ASCII
/// byte would make byte-count diverge from visual column count, which
/// defeats the byte-for-byte diff promise.
#[test]
fn canonical_is_ascii_only() {
    let out = render_canonical(&canonical_snapshot(), &SnapshotConfig::default());
    for (i, b) in out.as_bytes().iter().enumerate() {
        assert!(
            *b == b'\n' || (*b >= 0x20 && *b <= 0x7E),
            "byte {b:#04x} at offset {i} is not printable ASCII — canonical stream must stay ASCII-only",
        );
    }
}
