// SPDX-License-Identifier: AGPL-3.0-only

//! install.sh portability gate (smithy C1, known wart « BSD-sed »).
//!
//! The script is piped to whatever `/bin/sh` + `sed` the tenant's
//! machine carries — macOS ships BSD sed, Linux ships GNU sed. The
//! historical wart: the profile-name derivation used the GNU-only BRE
//! `\?`, which BSD sed treats literally, so macOS installs silently
//! kept the URL scheme inside the profile name. The pipeline now uses
//! `sed -E` (POSIX ERE), and this gate exercises **the script itself**
//! (its `--derive-profile-name` self-test hook) — never a copy of the
//! pipeline — under:
//!
//! - the system `sed` (BSD on macOS, GNU on Linux: the two CI/dev
//!   platforms together cover both implementations);
//! - GNU `gsed` additionally, when installed — reported as an explicit
//!   skip otherwise (an honest skip, never a fabricated pass);
//! - `shellcheck`, when installed — same skip discipline.
//!
//! The verdict of each leg is a re-computation (shannon G-discipline):
//! the script runs and its stdout is compared, no PASS is declared
//! from reading the source.

use std::path::PathBuf;
use std::process::Command;

fn install_sh() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/install.sh")
}

/// Run `sh install.sh --derive-profile-name <host>` with `dir`
/// prepended to PATH (lets a leg pin which `sed` the pipeline sees).
fn derive(host: &str, path_prefix: Option<&std::path::Path>) -> String {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg(install_sh()).arg("--derive-profile-name").arg(host);
    if let Some(prefix) = path_prefix {
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{path}", prefix.display()));
    }
    let out = cmd.output().expect("sh must be runnable");
    assert!(
        out.status.success(),
        "install.sh --derive-profile-name failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

const CASES: &[(&str, &str)] = &[
    // The Tenant-Demo Tailscale host — the live deployment shape.
    (
        "https://tenant-demo.tailnet0.ts.net",
        "tenant-demo-tailnet0-ts-net",
    ),
    // Loopback with port + trailing slash (AWS VM internal endpoint).
    ("http://127.0.0.1:8443/", "127-0-0-1-8443"),
    // Scheme-free input must pass through un-mangled.
    ("cosmon.example.org", "cosmon-example-org"),
];

/// System sed (BSD on macOS — the platform the wart bit). The expected
/// values are the ones the server-side templating and the operator's
/// profile files already rely on.
#[test]
fn profile_name_derivation_under_system_sed() {
    for (host, expected) in CASES {
        assert_eq!(&derive(host, None), expected, "host {host}");
    }
}

/// GNU sed leg: when `gsed` is installed (brew coreutils-style), pin
/// it as `sed` via a PATH shim and re-run the same cases — both
/// implementations must derive byte-identical names. Skipped (loudly,
/// via stderr) when gsed is absent; on Linux CI the system-sed test
/// above already runs GNU sed, so the pair of platforms covers both.
#[test]
fn profile_name_derivation_under_gnu_sed_when_available() {
    let Ok(gsed) = which_gsed() else {
        eprintln!(
            "SKIP: gsed not installed — GNU leg covered by Linux runs of the system-sed test"
        );
        return;
    };
    let shim_dir = tempfile::tempdir().unwrap();
    let shim = shim_dir.path().join("sed");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&gsed, &shim).unwrap();
    for (host, expected) in CASES {
        assert_eq!(
            &derive(host, Some(shim_dir.path())),
            expected,
            "host {host} under GNU sed"
        );
    }
}

fn which_gsed() -> Result<PathBuf, ()> {
    let out = Command::new("sh")
        .args(["-c", "command -v gsed"])
        .output()
        .map_err(|_| ())?;
    if !out.status.success() {
        return Err(());
    }
    let path = String::from_utf8(out.stdout).map_err(|_| ())?;
    let path = path.trim();
    if path.is_empty() {
        Err(())
    } else {
        Ok(PathBuf::from(path))
    }
}

/// Run `sh install.sh --pilot-pack` with `HOME` pinned to a scratch dir,
/// exercising the **shipped** drop function (never a copy), and return the
/// scratch `HOME` path so the caller can inspect the artifacts it produced.
/// This mirrors the `--derive-profile-name` self-test discipline: the
/// verdict is a re-computation from real on-disk effects, never a read of
/// the source.
fn pilot_pack_drop(home: &std::path::Path) {
    let out = Command::new("/bin/sh")
        .arg(install_sh())
        .arg("--pilot-pack")
        .env("HOME", home)
        .output()
        .expect("sh must be runnable");
    assert!(
        out.status.success(),
        "install.sh --pilot-pack failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A fresh box (no `~/AGENTS.md`) gets the canonical file, a managed block,
/// and the harness symlinks — and the block speaks the REMOTE surface.
#[test]
fn pilot_pack_fresh_drop_creates_all_artifacts() {
    let home = tempfile::tempdir().unwrap();
    pilot_pack_drop(home.path());

    let canon = home.path().join(".config/cosmon/pilot.AGENTS.md");
    assert!(canon.is_file(), "canonical pilot.AGENTS.md must exist");
    let canon_body = std::fs::read_to_string(&canon).unwrap();
    assert!(
        canon_body.contains("cosmon-remote do"),
        "pilot-pack must speak the remote surface (do)"
    );
    assert!(
        !canon_body.contains("cs done"),
        "remote surface has NO client done"
    );

    let agents = home.path().join("AGENTS.md");
    let agents_body = std::fs::read_to_string(&agents).unwrap();
    assert!(agents_body.contains("# >>> cosmon pilot-pack >>>"));
    assert!(agents_body.contains("# <<< cosmon pilot-pack <<<"));

    // One file, every harness: the harness names are symlinks to AGENTS.md.
    for name in ["CLAUDE.md", "GEMINI.md"] {
        let link = home.path().join(name);
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            std::path::PathBuf::from("AGENTS.md"),
            "{name} must symlink to AGENTS.md"
        );
    }
}

/// Running the drop twice produces a byte-identical `~/AGENTS.md` with
/// exactly one managed block — the install path is safe to re-run.
#[test]
fn pilot_pack_drop_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    let agents = home.path().join("AGENTS.md");

    pilot_pack_drop(home.path());
    let first = std::fs::read_to_string(&agents).unwrap();
    pilot_pack_drop(home.path());
    let second = std::fs::read_to_string(&agents).unwrap();

    assert_eq!(first, second, "second drop must be byte-identical");
    assert_eq!(
        second.matches("# >>> cosmon pilot-pack >>>").count(),
        1,
        "exactly one managed block after re-run"
    );
}

/// A user's own `~/AGENTS.md` content is never clobbered: text before AND
/// after the managed block survives byte-for-byte across a refresh, and a
/// stale block body is replaced in place.
#[test]
fn pilot_pack_preserves_user_content_around_block() {
    let home = tempfile::tempdir().unwrap();
    let agents = home.path().join("AGENTS.md");
    std::fs::write(
        &agents,
        "TOP user line\n\n\
         # >>> cosmon pilot-pack >>>\nOLD STALE CONTENT\n# <<< cosmon pilot-pack <<<\n\n\
         BOTTOM user line\n",
    )
    .unwrap();

    pilot_pack_drop(home.path());
    let body = std::fs::read_to_string(&agents).unwrap();

    assert!(body.contains("TOP user line"), "top content preserved");
    assert!(
        body.contains("BOTTOM user line"),
        "bottom content preserved"
    );
    assert!(!body.contains("OLD STALE CONTENT"), "stale block replaced");
    assert!(body.contains("cosmon-remote do"), "fresh block written");
    assert_eq!(
        body.matches("# >>> cosmon pilot-pack >>>").count(),
        1,
        "still exactly one block"
    );
    // Position preserved: TOP before the block, BOTTOM after it.
    let top = body.find("TOP user line").unwrap();
    let block = body.find("# >>> cosmon pilot-pack >>>").unwrap();
    let bottom = body.find("BOTTOM user line").unwrap();
    assert!(top < block && block < bottom, "order preserved");
}

/// A pre-existing **real** `~/CLAUDE.md` (not our symlink) must never be
/// clobbered — the drop warns and leaves it untouched.
#[test]
fn pilot_pack_never_clobbers_a_real_harness_file() {
    let home = tempfile::tempdir().unwrap();
    let claude = home.path().join("CLAUDE.md");
    std::fs::write(&claude, "MY PRECIOUS CLAUDE FILE\n").unwrap();

    pilot_pack_drop(home.path());

    assert!(
        !claude.is_symlink(),
        "real CLAUDE.md must stay a regular file"
    );
    assert_eq!(
        std::fs::read_to_string(&claude).unwrap(),
        "MY PRECIOUS CLAUDE FILE\n",
        "real CLAUDE.md content untouched"
    );
    // The other name (absent) is still linked.
    assert_eq!(
        std::fs::read_link(home.path().join("GEMINI.md")).unwrap(),
        std::path::PathBuf::from("AGENTS.md"),
    );
}

/// `sh -n` — the script must parse under a POSIX sh on every platform.
#[test]
fn install_sh_parses_under_posix_sh() {
    let out = Command::new("/bin/sh")
        .arg("-n")
        .arg(install_sh())
        .output()
        .expect("sh must be runnable");
    assert!(
        out.status.success(),
        "sh -n: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// shellcheck, when installed. Honest skip otherwise.
#[test]
fn install_sh_passes_shellcheck_when_available() {
    let probe = Command::new("sh")
        .args(["-c", "command -v shellcheck"])
        .output();
    let Ok(probe) = probe else {
        eprintln!("SKIP: cannot probe for shellcheck");
        return;
    };
    if !probe.status.success() {
        eprintln!("SKIP: shellcheck not installed");
        return;
    }
    let out = Command::new("shellcheck")
        .arg(install_sh())
        .output()
        .expect("shellcheck runnable");
    assert!(
        out.status.success(),
        "shellcheck findings:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}
