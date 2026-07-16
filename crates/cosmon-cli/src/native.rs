// SPDX-License-Identifier: AGPL-3.0-only

//! Native step executor — registry of in-process Rust functions callable as
//! formula steps.
//!
//! A native step declares `native = "<key>"` in its formula TOML. When
//! `cs tackle` encounters it, instead of spawning a shell (`command = `) or
//! launching a Claude worker, it looks the key up in a compile-time registry
//! of `fn(&NativeCtx) -> Result<(), NativeError>` and calls it directly.
//!
//! Native steps are **constrained leaves**:
//!
//! - They run in-process (sub-millisecond dispatch).
//! - They cannot spawn sub-molecules or call `cs evolve` / `cs complete`;
//!   success is signaled by `Ok(())`, failure by `Err`.
//! - Output is written to `MOLECULE_DIR/gate-output.log` (same contract as
//!   shell gates) so operators see a unified trail.
//!
//! The registry is built once per process via `built_in_registry()`. Adding a
//! native is a matter of writing a function and appending it to that map —
//! no trait, no plugin loader, no `unsafe`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Context passed to a native step.
#[derive(Debug, Clone)]
pub struct NativeCtx {
    /// The molecule's `.cosmon/state/molecules/<id>/` directory.
    ///
    /// Native functions may read or write artifacts here (e.g. evidence,
    /// analysis outputs). Currently unused by the built-in `smoke::*`
    /// functions but kept in the API for future natives.
    #[allow(dead_code)]
    pub mol_dir: PathBuf,
    /// The formula step id being executed.
    pub step_id: String,
    /// Working directory (repo root) for any subprocess helpers.
    pub work_dir: PathBuf,
}

/// Error returned by a native step.
#[derive(Debug, Clone)]
pub enum NativeError {
    /// The native function returned a failure.
    Failed(String),
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for NativeError {}

/// Signature of a native step function.
pub type NativeFn = fn(&NativeCtx) -> Result<NativeOutput, NativeError>;

/// Output of a native step — captured stdout/stderr-like buffers written to
/// `gate-output.log` for parity with shell gates.
#[derive(Debug, Default, Clone)]
pub struct NativeOutput {
    /// Human-readable log captured during execution.
    pub log: String,
}

/// Resolve `key` to a registered native function.
#[must_use]
pub fn lookup(key: &str) -> Option<NativeFn> {
    built_in_registry().get(key).copied()
}

/// Build the compile-time registry of native functions.
fn built_in_registry() -> HashMap<&'static str, NativeFn> {
    let mut m: HashMap<&'static str, NativeFn> = HashMap::new();
    m.insert("cosmon::smoke::cargo_check", smoke::cargo_check);
    m.insert("cosmon::smoke::cargo_test", smoke::cargo_test);
    m.insert("cosmon::smoke::cargo_clippy", smoke::cargo_clippy);
    m.insert("cosmon::smoke::cargo_fmt_check", smoke::cargo_fmt_check);
    m.insert("cosmon::noop", noop);
    m
}

/// Trivial always-success native; used in tests and as a reference example.
#[allow(clippy::unnecessary_wraps)]
fn noop(ctx: &NativeCtx) -> Result<NativeOutput, NativeError> {
    Ok(NativeOutput {
        log: format!("noop step {} ok", ctx.step_id),
    })
}

/// Write the captured log to `MOLECULE_DIR/gate-output.log`.
pub fn write_log(mol_dir: &Path, step_id: &str, native_fn: &str, out: &NativeOutput, dur_ms: u64) {
    let content = format!(
        "# Native step (step: {step_id})\n\
         # Function: {native_fn}\n\
         # Duration: {dur_ms}ms\n\n\
         {}\n",
        out.log,
    );
    let _ = std::fs::write(mol_dir.join("gate-output.log"), content);
}

/// Built-in smoke-test natives — thin wrappers around `cargo` subcommands.
///
/// These replicate the behaviour of the existing shell gate steps but avoid
/// the `sh -c` round-trip. They still spawn a subprocess (there is no stable
/// in-process Cargo API), so the latency gain is modest; the real value is
/// eliminating shell parsing, variable expansion, and PATH surprises.
pub mod smoke {
    use super::{NativeCtx, NativeError, NativeOutput};
    use std::process::Command;

    fn run_cargo(ctx: &NativeCtx, args: &[&str]) -> Result<NativeOutput, NativeError> {
        let output = Command::new("cargo")
            .args(args)
            .current_dir(&ctx.work_dir)
            .output()
            .map_err(|e| NativeError::Failed(format!("failed to spawn cargo: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let log = format!(
            "$ cargo {}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
            args.join(" "),
        );

        if output.status.success() {
            Ok(NativeOutput { log })
        } else {
            let code = output.status.code().unwrap_or(-1);
            Err(NativeError::Failed(format!(
                "cargo {} failed (exit {code})\n{log}",
                args.join(" "),
            )))
        }
    }

    /// `cargo check --workspace` as a native step.
    pub fn cargo_check(ctx: &NativeCtx) -> Result<NativeOutput, NativeError> {
        run_cargo(ctx, &["check", "--workspace"])
    }

    /// `cargo test --workspace` as a native step.
    pub fn cargo_test(ctx: &NativeCtx) -> Result<NativeOutput, NativeError> {
        run_cargo(ctx, &["test", "--workspace"])
    }

    /// `cargo clippy --workspace -- -D warnings` as a native step.
    pub fn cargo_clippy(ctx: &NativeCtx) -> Result<NativeOutput, NativeError> {
        run_cargo(ctx, &["clippy", "--workspace", "--", "-D", "warnings"])
    }

    /// `cargo fmt --all -- --check` as a native step.
    pub fn cargo_fmt_check(ctx: &NativeCtx) -> Result<NativeOutput, NativeError> {
        run_cargo(ctx, &["fmt", "--all", "--", "--check"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> NativeCtx {
        NativeCtx {
            mol_dir: std::env::temp_dir(),
            step_id: "t".to_owned(),
            work_dir: std::env::temp_dir(),
        }
    }

    #[test]
    fn noop_succeeds() {
        let f = lookup("cosmon::noop").expect("noop registered");
        let out = f(&ctx()).expect("noop ok");
        assert!(out.log.contains("noop"));
    }

    #[test]
    fn unknown_key_returns_none() {
        assert!(lookup("cosmon::nonexistent").is_none());
    }

    #[test]
    fn smoke_natives_registered() {
        assert!(lookup("cosmon::smoke::cargo_check").is_some());
        assert!(lookup("cosmon::smoke::cargo_test").is_some());
        assert!(lookup("cosmon::smoke::cargo_clippy").is_some());
        assert!(lookup("cosmon::smoke::cargo_fmt_check").is_some());
    }
}
