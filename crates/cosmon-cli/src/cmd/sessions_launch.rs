// SPDX-License-Identifier: AGPL-3.0-only

//! The supported interactive provider launch boundary. Identity is minted here,
//! before either a callback or a tool subprocess can inherit the environment.

use cosmon_core::id::SessionId;
use std::ffi::OsString;

/// Providers whose callback identity adapters are supported.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Provider {
    /// Claude Code.
    Claude,
    /// Codex CLI.
    Codex,
}

impl Provider {
    fn executable(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// Start an interactive pilot with a new anonymous Cosmon identity.
#[derive(clap::Args)]
pub struct Args {
    /// Provider executable to resolve on PATH.
    pub provider: Provider,
    /// Provider arguments, passed verbatim after `--`.
    #[arg(last = true, value_name = "ARGS")]
    pub args: Vec<OsString>,
}

/// Injectable process boundary; tests can inspect the identity and argv without
/// launching an agent. The real adapter replaces the process on Unix so signals,
/// terminal handling and exit status remain the provider's own.
trait Launcher {
    fn launch(&self, provider: Provider, args: &[OsString], sid: &SessionId) -> anyhow::Result<()>;
}

struct ProcessLauncher;

impl Launcher for ProcessLauncher {
    fn launch(&self, provider: Provider, args: &[OsString], sid: &SessionId) -> anyhow::Result<()> {
        let mut command = std::process::Command::new(provider.executable());
        command
            .args(args)
            .env("COSMON_SESSION_ID", sid.as_str())
            .env(cosmon_core::caller_identity::LAUNCH_MARKER, sid.as_str())
            // A new pilot is not the worker or leased pilot that launched it.
            .env_remove("COSMON_PARENT_MOL_ID")
            .env_remove("COSMON_WORKER_ID")
            .env_remove("CLAUDE_SESSION_ID");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            Err(command.exec().into())
        }
        #[cfg(not(unix))]
        {
            let status = command.status()?;
            std::process::exit(status.code().unwrap_or(1));
        }
    }
}

fn launch_with(args: &Args, launcher: &impl Launcher) -> anyhow::Result<()> {
    let sid = SessionId::new(format!("pilot-{:032x}", rand::random::<u128>()))?;
    launcher.launch(args.provider, &args.args, &sid)
}

/// Launch the selected provider with inherited stdio and a fresh identity.
///
/// # Errors
/// Returns provider execution errors without starting a shell.
pub fn run(args: &Args) -> anyhow::Result<()> {
    launch_with(args, &ProcessLauncher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Capture(RefCell<Vec<(String, Vec<OsString>, SessionId)>>);

    impl Launcher for Capture {
        fn launch(
            &self,
            provider: Provider,
            args: &[OsString],
            sid: &SessionId,
        ) -> anyhow::Result<()> {
            self.0.borrow_mut().push((
                provider.executable().to_owned(),
                args.to_vec(),
                sid.clone(),
            ));
            Ok(())
        }
    }

    #[test]
    fn launch_boundary_mints_identity_and_passes_opaque_arguments() -> anyhow::Result<()> {
        let capture = Capture::default();
        let args = Args {
            provider: Provider::Codex,
            args: vec![OsString::from("a 'quoted' argument"), OsString::new()],
        };
        launch_with(&args, &capture)?;
        launch_with(&args, &capture)?;
        let calls = capture.0.borrow();
        assert_eq!(calls[0].0, "codex");
        assert_eq!(calls[0].1, args.args);
        assert_ne!(calls[0].2, calls[1].2);
        Ok(())
    }
}
