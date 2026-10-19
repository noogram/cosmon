// SPDX-License-Identifier: AGPL-3.0-only

//! Launch-time tmux witnesses and guarded, read-only attachment.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cosmon_core::terminal::{
    TerminalBackend, TerminalIdentity, TerminalInteractionBackend, TerminalUnavailable,
};
use cosmon_process_witness::process_start_time;

use crate::TmuxBackend;

/// Format used in `new-session -P`: captures the newly created target itself.
pub(crate) const LAUNCH_FORMAT: &str = "#{socket_path}\t#{pid}\t#{session_id}\t#{session_created}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{session_windows}\t#{window_panes}\t#{version}";

/// Parse a fresh tmux observation, refusing missing kernel launch witnesses.
pub(crate) fn parse_identity(raw: &str) -> Result<TerminalIdentity, TerminalUnavailable> {
    let fields: Vec<_> = raw.trim_end_matches('\n').split('\t').collect();
    if fields.len() != 10 || fields[0].is_empty() {
        return Err(TerminalUnavailable::IdentityUnknown);
    }
    if !supports_client_flags(fields[9]) {
        return Err(TerminalUnavailable::BackendUnavailable);
    }
    if fields[6..9] != ["0", "1", "1"] {
        return Err(TerminalUnavailable::AmbiguousTerminal);
    }
    let number = |s: &str| {
        s.parse::<u32>()
            .map_err(|_| TerminalUnavailable::IdentityUnknown)
    };
    let server_pid = number(fields[1])?;
    let session_id = number(
        fields[2]
            .strip_prefix('$')
            .ok_or(TerminalUnavailable::IdentityUnknown)?,
    )?;
    let session_created = fields[3]
        .parse()
        .map_err(|_| TerminalUnavailable::IdentityUnknown)?;
    let pane_id = number(
        fields[4]
            .strip_prefix('%')
            .ok_or(TerminalUnavailable::IdentityUnknown)?,
    )?;
    let root_pid = number(fields[5])?;
    Ok(TerminalIdentity::Tmux {
        socket_path: fields[0].to_owned(),
        server_pid,
        server_start: process_start_time(server_pid).ok_or(TerminalUnavailable::IdentityUnknown)?,
        session_id,
        session_created,
        pane_id,
        pane_pid: root_pid,
        pane_start: process_start_time(root_pid).ok_or(TerminalUnavailable::IdentityUnknown)?,
    })
}

impl TerminalBackend for TmuxBackend {
    fn validate_terminal(&self, identity: &TerminalIdentity) -> Result<(), TerminalUnavailable> {
        if !probe(
            Command::new("python3").args(["-c", "import os, select, subprocess, termios, tty"]),
        )?
        .status
        .success()
        {
            return Err(TerminalUnavailable::BackendUnavailable);
        }
        let TerminalIdentity::Tmux {
            socket_path,
            session_id,
            ..
        } = identity;
        let output = probe(Command::new("tmux").args([
            "-S",
            socket_path,
            "display-message",
            "-p",
            "-t",
            &format!("${session_id}"),
            LAUNCH_FORMAT,
        ]))?;
        if !output.status.success() {
            return Err(TerminalUnavailable::BackendUnavailable);
        }
        let text = std::str::from_utf8(&output.stdout)
            .map_err(|_| TerminalUnavailable::IdentityUnknown)?;
        if &parse_identity(text)? != identity {
            return Err(TerminalUnavailable::IdentityChanged);
        }
        Ok(())
    }

    fn attach_read_only(&self, identity: &TerminalIdentity) -> Result<(), TerminalUnavailable> {
        self.validate_terminal(identity)?;
        let TerminalIdentity::Tmux {
            socket_path,
            session_id,
            ..
        } = identity;
        // All values interpolated into tmux's command language are typed numbers.
        // The format and attach execute on the SAME server queue. A reused name,
        // changed active pane, respawn or added window cannot redirect this client.
        let guard = identity_guard(identity);
        let TerminalIdentity::Tmux { pane_id, .. } = identity;
        let read = format!("if-shell -F -t ${session_id} '{guard}' 'capture-pane -p -e -t %{pane_id}' 'terminal-incarnation-refused'");
        let attach = format!("attach-session -E -r -f ignore-size -t ${session_id}");
        let status = Command::new("python3")
            .args(["-c", include_str!("terminal_view.py"), &read, "tmux", "-C"])
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .args([
                "-S",
                socket_path,
                "if-shell",
                "-F",
                "-t",
                &format!("${session_id}"),
                &guard,
                &attach,
                "terminal-incarnation-refused",
            ])
            .status()
            .map_err(|_| TerminalUnavailable::BackendUnavailable)?;
        if !status.success() {
            return Err(TerminalUnavailable::BackendUnavailable);
        }
        Ok(())
    }
}

impl TerminalInteractionBackend for TmuxBackend {
    fn attach_interactive(&self, identity: &TerminalIdentity) -> Result<(), TerminalUnavailable> {
        self.validate_terminal(identity)?;
        let TerminalIdentity::Tmux {
            socket_path,
            session_id,
            pane_id,
            ..
        } = identity;
        let guard = identity_guard(identity);
        // Numeric targets and a fixed predicate only; keyboard bytes travel as hex.
        let attach = format!("attach-session -E -t ${session_id}");
        let status = Command::new("python3")
            .args([
                "-c",
                include_str!("terminal_interactive.py"),
                &session_id.to_string(),
                &pane_id.to_string(),
                &guard,
                "tmux",
                "-C",
                "-S",
                socket_path,
                "if-shell",
                "-F",
                "-t",
                &format!("${session_id}"),
                &guard,
                &attach,
                "terminal-incarnation-refused",
            ])
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .status()
            .map_err(|_| TerminalUnavailable::BackendUnavailable)?;
        if !status.success() {
            return Err(TerminalUnavailable::BackendUnavailable);
        }
        Ok(())
    }
}

fn identity_guard(identity: &TerminalIdentity) -> String {
    identity_guard_for(identity, true)
}

fn identity_guard_for(identity: &TerminalIdentity, live: bool) -> String {
    identity_guard_for_scope(identity, live, true)
}

fn identity_guard_for_scope(identity: &TerminalIdentity, live: bool, layout: bool) -> String {
    let TerminalIdentity::Tmux {
        server_pid,
        session_id,
        session_created,
        pane_id,
        pane_pid,
        ..
    } = identity;
    let conditions = [
        format!("#{{==:#{{pid}},{server_pid}}}"),
        format!("#{{==:#{{session_id}},${session_id}}}"),
        format!("#{{==:#{{session_created}},{session_created}}}"),
        format!("#{{==:#{{pane_id}},%{pane_id}}}"),
        format!("#{{==:#{{pane_pid}},{pane_pid}}}"),
        if live {
            "#{==:#{pane_dead},0}".into()
        } else {
            "1".into()
        },
        if layout {
            "#{==:#{session_windows},1}".into()
        } else {
            "1".into()
        },
        if layout {
            "#{==:#{window_panes},1}".into()
        } else {
            "1".into()
        },
    ];
    conditions
        .into_iter()
        .reduce(|a, b| format!("#{{&&:{a},{b}}}"))
        .unwrap_or_default()
}

/// Outcome of reclaiming a pinned terminal, so purge can report whether it killed anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinnedPurgeOutcome {
    /// The guarded kill addressed the recorded incarnation only.
    Terminated,
    /// The recorded server process no longer exists with its recorded start time.
    ServerGone,
    /// The exact terminal could not be addressed; no replacement was killed.
    IncarnationUnavailable,
}

/// Reclaim an exact pinned terminal without falling back to a session name.
///
/// A disappeared server and a refused incarnation both permit forgetting the
/// stale fleet record. Neither grants authority over a replacement terminal.
///
/// # Errors
/// Refuses an unobservable server whose disappearance cannot be established.
pub fn purge_pinned_terminal(
    identity: &TerminalIdentity,
) -> Result<PinnedPurgeOutcome, cosmon_core::transport::TransportError> {
    let TerminalIdentity::Tmux {
        server_pid,
        server_start,
        ..
    } = identity;
    let observed = process_start_time(*server_pid);
    let absent = observed.is_some_and(|start| start != *server_start)
        || (observed.is_none() && server_process_absent(*server_pid));
    if absent {
        return Ok(PinnedPurgeOutcome::ServerGone);
    }
    if observed != Some(*server_start) {
        return Err(cosmon_core::transport::TransportError::Io(
            "pinned tmux server identity is unavailable; disappearance is unproven".into(),
        ));
    }
    match checked_bound_command(identity, &["kill-session"]) {
        Ok(_) => Ok(PinnedPurgeOutcome::Terminated),
        Err(BoundCommandFailure::IncarnationChanged) => {
            Ok(PinnedPurgeOutcome::IncarnationUnavailable)
        }
        Err(BoundCommandFailure::Unavailable(error)) => Err(error),
    }
}

fn server_process_absent(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // Signal zero observes existence only. EPERM/unknown is not absence.
        matches!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH)
        )
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Execute only observation or exact teardown on a durable Exec terminal.
/// Server fingerprints are checked before using numeric targets; the incarnation
/// format guard and the operation run together on tmux's command queue.
pub(crate) fn bound_command(
    identity: &TerminalIdentity,
    args: &[&str],
) -> Result<String, cosmon_core::transport::TransportError> {
    checked_bound_command(identity, args).map_err(|error| match error {
        BoundCommandFailure::IncarnationChanged => cosmon_core::transport::TransportError::Io(
            "pinned terminal incarnation changed or unavailable".into(),
        ),
        BoundCommandFailure::Unavailable(error) => error,
    })
}

enum BoundCommandFailure {
    IncarnationChanged,
    Unavailable(cosmon_core::transport::TransportError),
}

fn checked_bound_command(
    identity: &TerminalIdentity,
    args: &[&str],
) -> Result<String, BoundCommandFailure> {
    use cosmon_core::transport::TransportError;
    let error = || {
        BoundCommandFailure::Unavailable(TransportError::Io(
            "pinned terminal incarnation changed or unavailable".into(),
        ))
    };
    let TerminalIdentity::Tmux {
        socket_path,
        server_pid,
        server_start,
        pane_id,
        session_id,
        pane_pid,
        pane_start,
        ..
    } = identity;
    if process_start_time(*server_pid) != Some(*server_start) {
        return Err(error());
    }
    let operation = args.first().copied().ok_or_else(error)?;
    if !matches!(
        operation,
        "capture-pane" | "list-panes" | "display-message" | "has-session" | "kill-session"
    ) {
        return Err(BoundCommandFailure::Unavailable(TransportError::Io(
            "pinned Codex Exec transport does not support this operation".into(),
        )));
    }
    let target = if operation == "capture-pane" {
        format!("%{pane_id}")
    } else {
        format!("${session_id}")
    };
    let output = pinned_probe(
        socket_path,
        *server_pid,
        &[
            "display-message",
            "-p",
            "-t",
            &format!("%{pane_id}"),
            "#{pane_dead}",
        ],
    )?;
    if !output.status.success() {
        return Err(classify_tmux_failure(&output));
    }
    let dead = String::from_utf8_lossy(&output.stdout).trim() == "1";
    if !dead {
        match process_start_time(*pane_pid) {
            Some(start) if start == *pane_start => {}
            Some(_) => return Err(BoundCommandFailure::IncarnationChanged),
            None if server_process_absent(*pane_pid) => {
                return Err(BoundCommandFailure::IncarnationChanged)
            }
            None => return Err(error()),
        }
    }
    // Never pass a caller's name-addressed target through to the effect.
    let mut command = vec![operation.to_owned()];
    let mut index = 1;
    while index < args.len() {
        if args[index] == "-t" {
            index += 2;
        } else if args[index] == "-a" {
            index += 1;
        } else {
            command.push(args[index].to_owned());
            index += 1;
        }
    }
    command.extend(["-t".into(), target]);
    let command = command
        .iter()
        .map(|s| crate::TmuxBackend::shell_quote(s))
        .collect::<Vec<_>>()
        .join(" ");
    let guard = identity_guard_for(identity, false);
    // A layout-only refusal does not establish that the original worker is
    // gone. Distinguish it from a changed binding on the same server queue.
    let refusal = format!(
        "if-shell -F -t %{pane_id} '{}' 'display-message -p cosmon-terminal-layout-refused' 'display-message -p cosmon-incarnation-refused'",
        identity_guard_for_scope(identity, false, false),
    );
    let output = pinned_probe(
        socket_path,
        *server_pid,
        &[
            "if-shell",
            "-F",
            "-t",
            &format!("%{pane_id}"),
            &guard,
            &command,
            &refusal,
        ],
    )?;
    bound_output(output)
}

fn bound_output(output: std::process::Output) -> Result<String, BoundCommandFailure> {
    use cosmon_core::transport::TransportError;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.trim() == "cosmon-terminal-layout-refused" {
        return Err(BoundCommandFailure::Unavailable(TransportError::Io(
            "pinned terminal layout changed while its worker incarnation is still present".into(),
        )));
    }
    if text.trim() == "cosmon-incarnation-refused" {
        return Err(BoundCommandFailure::IncarnationChanged);
    }
    if !output.status.success() {
        return Err(classify_tmux_failure(&output));
    }
    Ok(text)
}

// The witness and operation share one connection/command queue. A live original
// server may lose its socket while another server binds that same path. Such a
// replacement's missing numeric target cannot prove the original target absent.
fn pinned_probe(
    socket_path: &str,
    server_pid: u32,
    args: &[&str],
) -> Result<std::process::Output, BoundCommandFailure> {
    let unavailable = || {
        BoundCommandFailure::Unavailable(cosmon_core::transport::TransportError::Io(
            "pinned terminal server endpoint unavailable or changed".into(),
        ))
    };
    let mut output = probe(
        Command::new("tmux")
            .args(["-S", socket_path, "display-message", "-p", "#{pid}", ";"])
            .args(args),
    )
    .map_err(|_| unavailable())?;
    let newline = output
        .stdout
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(unavailable)?;
    let observed = std::str::from_utf8(&output.stdout[..newline])
        .ok()
        .and_then(|pid| pid.trim().parse::<u32>().ok());
    if observed != Some(server_pid) {
        return Err(unavailable());
    }
    output.stdout.drain(..=newline);
    Ok(output)
}

fn classify_tmux_failure(output: &std::process::Output) -> BoundCommandFailure {
    let stderr = String::from_utf8_lossy(&output.stderr);
    // These diagnostics establish that the exact numeric target is absent.
    // Socket failures, process launch errors and timeouts are not absence.
    if stderr.starts_with("can't find pane:") || stderr.starts_with("can't find session:") {
        BoundCommandFailure::IncarnationChanged
    } else {
        BoundCommandFailure::Unavailable(cosmon_core::transport::TransportError::Io(
            "pinned terminal transport unavailable".into(),
        ))
    }
}

// This is a capability floor, not a liveness or provider-version heuristic.
// tmux 3.2's manual specifies -E, read-only and ignore-size client semantics.
pub(crate) fn supports_client_flags(version: &str) -> bool {
    let Some((major, minor)) = version.split_once('.') else {
        return false;
    };
    let Ok(major) = major.parse::<u32>() else {
        return false;
    };
    let minor = minor.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let Ok(minor) = minor.parse::<u32>() else {
        return false;
    };
    major > 3 || (major == 3 && minor >= 2)
}

fn probe(command: &mut Command) -> Result<std::process::Output, TerminalUnavailable> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| TerminalUnavailable::BackendUnavailable)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|_| TerminalUnavailable::BackendUnavailable)
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TerminalUnavailable::BackendUnavailable);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_or_old_tmux_capability_is_refused() {
        assert!(supports_client_flags("3.2"));
        assert!(supports_client_flags("3.5a"));
        assert!(!supports_client_flags("3.1c"));
        assert!(!supports_client_flags("unknown"));
        assert!(!supports_client_flags("next-3.5"));
    }
}
