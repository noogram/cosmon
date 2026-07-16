// SPDX-License-Identifier: AGPL-3.0-only

//! Build the `fake-cs` test stand-in once per `OUT_DIR`.
//!
//! See `Cargo.toml` for the rationale (Cargo's `CARGO_BIN_EXE_<name>`
//! is set only for the defining crate's integration tests, which
//! makes `[[bin]]` unusable as a fixture for downstream consumers).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let src = PathBuf::from(&manifest_dir).join("assets/fake_cs.rs");
    let target_name = if cfg!(windows) {
        "fake-cs.exe"
    } else {
        "fake-cs"
    };
    let target = PathBuf::from(&out_dir).join(target_name);

    println!("cargo:rerun-if-changed=assets/fake_cs.rs");
    println!("cargo:rerun-if-changed=build.rs");

    // Compile with `rustc` directly. `fake-cs` is std-only so no
    // resolver / dependency machinery is needed; this is faster than
    // re-entering Cargo and avoids cargo lock contention in tests.
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let status = Command::new(&rustc)
        .arg("--edition=2021")
        .arg("-O")
        .arg("--crate-type=bin")
        .arg(&src)
        .arg("-o")
        .arg(&target)
        .status()
        .expect("invoke rustc to build fake-cs");
    assert!(status.success(), "rustc failed to compile fake-cs");

    println!(
        "cargo:rustc-env=COSMON_OIDC_TESTKIT_FAKE_CS={}",
        target.display()
    );
}
