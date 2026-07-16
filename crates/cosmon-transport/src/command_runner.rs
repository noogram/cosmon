// SPDX-License-Identifier: AGPL-3.0-only

//! Production [`CommandRunner`] adapter backed by [`std::process::Command`].
//!
//! The [`CommandRunner`] *port* is defined in [`cosmon_core::harness`] (the
//! zero-I/O domain crate). This is its production *adapter*: the
//! process-execution seam, moved out of `cosmon-core` per
//! INV-DOMAIN-PURE-NO-IO (ADR-082). Tests of higher layers inject
//! `cosmon_core::harness::MockCommandRunner` instead.

use std::path::Path;

use cosmon_core::harness::{CommandOutput, CommandRunner, CommandRunnerError};

/// Real [`CommandRunner`] backed by [`std::process::Command`].
///
/// This is the production implementation. It captures stdout/stderr with
/// [`std::process::Command::output`], which waits for the child to exit.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    #[allow(clippy::similar_names)]
    fn exec(
        &self,
        cmd: &str,
        args: &[&str],
        cwd: &Path,
    ) -> Result<CommandOutput, CommandRunnerError> {
        let out = std::process::Command::new(cmd)
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(|e| CommandRunnerError::Spawn {
                cmd: cmd.to_owned(),
                reason: e.to_string(),
            })?;
        Ok(CommandOutput {
            status: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_runner_executes_true_and_echo() {
        let runner = RealCommandRunner;
        let cwd = std::env::temp_dir();

        // `true` exits 0 with no output.
        let out = runner.exec("true", &[], &cwd).expect("spawn true");
        assert!(out.success(), "true should exit 0");

        // `echo hello` writes to stdout.
        let out = runner.exec("echo", &["hello"], &cwd).expect("spawn echo");
        assert!(out.success());
        assert_eq!(out.stdout.trim(), "hello");
    }

    #[test]
    fn real_runner_reports_spawn_failure_for_missing_binary() {
        let runner = RealCommandRunner;
        let cwd = std::env::temp_dir();
        let err = runner
            .exec("definitely-not-a-real-binary-xyzzy", &[], &cwd)
            .expect_err("missing binary should fail to spawn");
        assert!(matches!(err, CommandRunnerError::Spawn { .. }));
    }
}
