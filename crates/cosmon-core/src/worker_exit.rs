// SPDX-License-Identifier: AGPL-3.0-only

//! Launch-bound OS observations. These never imply molecule completion or kill authority.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::id::MoleculeId;

/// An opaque launch or incarnation token, safe to use as one path component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LaunchToken(String);

impl LaunchToken {
    /// Construct a token from entropy supplied by the impure caller.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        let mut token = String::with_capacity(32);
        for byte in bytes {
            token.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
            token.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
        }
        Self(token)
    }

    /// Borrow the validated path component.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for LaunchToken {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(Self(value))
        } else {
            Err("invalid worker launch token".to_owned())
        }
    }
}

impl From<LaunchToken> for String {
    fn from(value: LaunchToken) -> Self {
        value.0
    }
}

/// Expected identity minted before spawning; terminal labels and PIDs cannot replace it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchIdentity {
    /// Receipt identity schema; incompatible versions remain unknown to older readers.
    pub schema: u32,
    /// Molecule whose canonical directory owns the evidence.
    pub molecule_id: MoleculeId,
    /// Distinguishes dispatches of the same molecule.
    pub launch_id: LaunchToken,
    /// Prevents a retry from reusing an incarnation.
    pub incarnation: LaunchToken,
    /// Binds argv, cwd and the declared environment changes without persisting their values.
    pub request_hash: String,
}

/// Private request metadata; contains neither argv nor environment values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRequest {
    /// Versioned identity against which every receipt is checked.
    pub identity: LaunchIdentity,
    /// Hashes of declared assignments; `None` requires absence in the child environment.
    pub environment: BTreeMap<String, Option<String>>,
}

impl LaunchRequest {
    /// Bind a launch to the command and environment policy the adapter resolved.
    /// Only environment changes are bound; ambient inherited values are never exported.
    #[allow(clippy::too_many_arguments)]
    pub fn bind(
        molecule_id: MoleculeId,
        launch_id: LaunchToken,
        incarnation: LaunchToken,
        program: &str,
        args: &[String],
        cwd: &Path,
        environment: &[(String, Option<String>)],
    ) -> Result<Self, String> {
        let environment = environment
            .iter()
            .map(|(name, value)| (name.clone(), value.as_deref().map(value_hash)))
            .collect();
        let request_hash = command_hash(program, args, cwd, &environment)?;
        Ok(Self {
            identity: LaunchIdentity {
                schema: 1,
                molecule_id,
                launch_id,
                incarnation,
                request_hash,
            },
            environment,
        })
    }

    /// Check actual launch inputs; a changed or missing assignment refuses spawn.
    pub fn matches(
        &self,
        program: &str,
        args: &[String],
        cwd: &Path,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> bool {
        self.environment
            .iter()
            .all(|(name, expected)| lookup(name).as_deref().map(value_hash) == *expected)
            && command_hash(program, args, cwd, &self.environment)
                .is_ok_and(|hash| hash == self.identity.request_hash)
    }
}

fn value_hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn command_hash(
    program: &str,
    args: &[String],
    cwd: &Path,
    environment: &BTreeMap<String, Option<String>>,
) -> Result<String, String> {
    let bytes = serde_json::to_vec(&(1, program, args, cwd, environment))
        .map_err(|_| "worker request cannot be encoded".to_owned())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// An OS observation, deliberately separate from provider classification and completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerExitObservation {
    /// The parent waited for the final executable's normal termination.
    Exited {
        /// POSIX exit code, never a synthesized `128 + signal` value.
        code: u8,
    },
    /// The parent waited for the final executable's signal termination.
    Signaled {
        /// OS signal number, kept distinct from an exit code.
        signal: i32,
        /// Whether the OS wait status carried its core-dump bit.
        core_dumped: bool,
    },
    /// Spawn's exec handshake failed before a program birth could be recorded.
    ExecFailed {
        /// OS errno from the exec handshake.
        errno: i32,
    },
    /// No validated terminal receipt exists; absence never means success.
    Unknown,
}

impl WorkerExitObservation {
    /// Whether the observation is a well-formed terminal OS result.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        match self {
            Self::Exited { .. } => true,
            Self::Signaled { signal, .. } => (1..=127).contains(&signal),
            Self::ExecFailed { errno } => errno > 0,
            Self::Unknown => false,
        }
    }
}

/// Durable result produced solely by the exclusive owner of a launch claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerExitReceipt {
    /// Exact identity from the ex ante request.
    pub identity: LaunchIdentity,
    /// Diagnostic parent PID; conveys no signal or adoption capability.
    pub supervisor_pid: u32,
    /// Child witnessed by this parent; absent only on exec failure.
    pub child_pid: Option<u32>,
    /// Observation produced by owned-child wait or exec handshake.
    pub observation: WorkerExitObservation,
}

/// Injectable durable receipt effects; a claim is never released or reacquired.
pub trait WorkerExitStore {
    /// Reserve the launch exclusively and durably before any child exists.
    fn claim(&self, supervisor_pid: u32) -> Result<(), String>;
    /// Publish the child birth after a successful exec handshake.
    fn record_birth(&self, supervisor_pid: u32, child_pid: u32) -> Result<(), String>;
    /// Publish a single terminal observation atomically and durably.
    fn publish(&self, receipt: &WorkerExitReceipt) -> Result<(), String>;
    /// Publish with injectable crash checkpoints; ordinary callers use a no-op observer.
    fn publish_observed(
        &self,
        receipt: &WorkerExitReceipt,
        checkpoint: &dyn Fn(SupervisionPoint),
    ) -> Result<(), String> {
        self.publish(receipt)?;
        checkpoint(SupervisionPoint::AfterDurableResult);
        Ok(())
    }
    /// Read only validated final evidence, never a temporary or a PID inference.
    fn read(&self) -> WorkerExitObservation;
}

/// Effect boundaries at which a crash may destroy an observation, never justify replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisionPoint {
    /// The exclusive durable claim exists; no child has been spawned.
    BeforeSpawn,
    /// Exec succeeded but child birth has not been persisted.
    AfterSpawn,
    /// Child birth is durable; the parent has not waited yet.
    BeforeWait,
    /// Wait returned; its observation has not been persisted.
    AfterWait,
    /// Temporary result contents are synced but not published.
    AfterTemporarySync,
    /// Final rename and directory sync completed.
    AfterDurableResult,
}

/// An owned child handle; only its spawning parent may obtain its wait result.
pub trait OwnedWorker {
    /// Diagnostic child PID, never an authority handle.
    fn pid(&self) -> u32;
    /// Wait for this child, preserving signal versus exit-code semantics.
    fn wait(&mut self) -> Result<WorkerExitObservation, String>;
}

/// Injectable direct-executable launch; implementations inherit TTY and signal dispositions.
pub trait WorkerLaunchPort {
    /// Spawn the final executable without an intermediate shell.
    /// Returns the OS errno on an exec-handshake failure. Absence means the
    /// launcher could not supply OS provenance and must remain unknown.
    fn spawn(&self, program: &str, args: &[String]) -> Result<Box<dyn OwnedWorker>, Option<i32>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_binds_arguments_environment_and_incarnation_without_exporting_secrets(
    ) -> Result<(), String> {
        let molecule = MoleculeId::new("task-20261019-aaaa").map_err(|e| e.to_string())?;
        let request = LaunchRequest::bind(
            molecule,
            LaunchToken::from_bytes([1; 16]),
            LaunchToken::from_bytes([2; 16]),
            "worker",
            &["a b".to_owned()],
            Path::new("/work"),
            &[("CREDENTIAL".to_owned(), Some("secret-canary".to_owned()))],
        )?;
        assert!(
            request.matches("worker", &["a b".to_owned()], Path::new("/work"), |_| Some(
                "secret-canary".to_owned()
            ))
        );
        assert!(!request.matches(
            "worker",
            &["a".to_owned(), "b".to_owned()],
            Path::new("/work"),
            |_| Some("secret-canary".to_owned())
        ));
        let json = serde_json::to_string(&request).map_err(|e| e.to_string())?;
        assert!(!json.contains("secret-canary"));
        assert!(!json.contains("a b"));
        Ok(())
    }

    #[test]
    fn unknown_and_signal_are_never_normal_success() {
        assert!(!WorkerExitObservation::Unknown.is_terminal());
        assert_ne!(
            WorkerExitObservation::Exited { code: 143 },
            WorkerExitObservation::Signaled {
                signal: 15,
                core_dumped: false
            }
        );
    }
}
