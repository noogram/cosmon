// SPDX-License-Identifier: AGPL-3.0-only

//! Launch-time tmux witnesses and guarded, read-only attachment.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cosmon_core::terminal::{TerminalBackend, TerminalIdentity, TerminalUnavailable};
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

fn identity_guard(identity: &TerminalIdentity) -> String {
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
        "#{==:#{pane_dead},0}".into(),
        "#{==:#{session_windows},1}".into(),
        "#{==:#{window_panes},1}".into(),
    ];
    conditions
        .into_iter()
        .reduce(|a, b| format!("#{{&&:{a},{b}}}"))
        .unwrap_or_default()
}

// This is a capability floor, not a liveness or provider-version heuristic.
// tmux 3.2's manual specifies -E, read-only and ignore-size client semantics.
fn supports_client_flags(version: &str) -> bool {
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
        .stderr(Stdio::null())
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
