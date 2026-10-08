// SPDX-License-Identifier: AGPL-3.0-only

//! Shared caller identity for callbacks, interactive commands and lifecycle guards.
//! Codex native subprocess identity takes precedence over a marked launch. Claude
//! and Codex without native exports use the supported launch fallback. Workers
//! retain their dispatch identity independently of provider metadata.

use crate::id::{IdError, SessionId};

/// The source determines which provider metadata may participate in resolution.
#[derive(Clone, Copy, Debug)]
pub enum Caller<'a> {
    /// A tool subprocess or interactive command, with environment identity.
    Shell,
    /// A Claude callback carrying its optional `session_id` field.
    ClaudeCallback(Option<&'a str>),
    /// A Codex callback carrying its optional `thread-id` field.
    CodexCallback(Option<&'a str>),
}

/// Identity disagreement refuses resolution without exposing private values.
#[derive(Debug, thiserror::Error)]
pub enum ResolutionError {
    /// The selected identity does not satisfy the `SessionId` contract.
    #[error(transparent)]
    InvalidId(#[from] IdError),
    /// Both Codex subprocess aliases exist but do not identify the same caller.
    #[error("CODEX_THREAD_ID and CODEX_SESSION_ID disagree; refusing caller identity")]
    ConflictingCodexEnvironment,
    /// The callback and subprocess environment identify different callers.
    #[error("Codex callback thread-id disagrees with CODEX_THREAD_ID/CODEX_SESSION_ID; refusing caller identity")]
    ConflictingCodexCallback,
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn identity(value: Option<&str>) -> Result<Option<SessionId>, ResolutionError> {
    nonempty(value)
        .map(|value| SessionId::new(value.to_owned()).map_err(Into::into))
        .transpose()
}

/// Resolve a caller without reading ambient state.
///
/// Workers (nonempty `COSMON_PARENT_MOL_ID`) use dispatch `COSMON_SESSION_ID`,
/// falling back to an explicit worker id or the worker callback payload when
/// dispatch has not exported an id. Inherited native variables are never worker
/// identity. Other explicit overrides remain
/// available after native consistency checks. Codex exports must agree with each other and,
/// for a Codex callback, its payload. Native exports precede a matching launch
/// marker. Without native exports, marked launch identity precedes callback
/// identity. Unmarked callbacks use their native payload; unmarked shells keep
/// the legacy `COSMON_SESSION_ID` / `CLAUDE_SESSION_ID` compatibility fallback.
/// Claude callbacks never read Codex metadata.
///
/// # Errors
/// Refuses contradictory Codex metadata or an invalid selected identifier.
pub fn resolve<F>(
    explicit: Option<&str>,
    caller: Caller<'_>,
    env_lookup: F,
) -> Result<Option<SessionId>, ResolutionError>
where
    F: Fn(&str) -> Option<String>,
{
    if nonempty(env_lookup("COSMON_PARENT_MOL_ID").as_deref()).is_some() {
        let dispatch = env_lookup("COSMON_SESSION_ID");
        let callback = match caller {
            Caller::Shell => None,
            Caller::ClaudeCallback(native) | Caller::CodexCallback(native) => nonempty(native),
        };
        return identity(
            nonempty(dispatch.as_deref())
                .or(nonempty(explicit))
                .or(callback),
        );
    }
    if !matches!(caller, Caller::ClaudeCallback(_)) {
        let thread = env_lookup("CODEX_THREAD_ID");
        let session = env_lookup("CODEX_SESSION_ID");
        let thread = nonempty(thread.as_deref());
        let session = nonempty(session.as_deref());
        if matches!((thread, session), (Some(a), Some(b)) if a != b) {
            return Err(ResolutionError::ConflictingCodexEnvironment);
        }
        if let Some(native) = thread.or(session) {
            if let Caller::CodexCallback(payload) = caller {
                if nonempty(payload).is_some_and(|payload| payload != native) {
                    return Err(ResolutionError::ConflictingCodexCallback);
                }
            }
            return identity(nonempty(explicit).or(Some(native)));
        }
    }
    if nonempty(explicit).is_some() {
        return identity(explicit);
    }
    let launch = marked_launch_id(&env_lookup);
    if launch.is_some() {
        return identity(launch.as_deref());
    }
    match caller {
        Caller::ClaudeCallback(native) | Caller::CodexCallback(native) => identity(native),
        Caller::Shell => {
            let cosmon = env_lookup("COSMON_SESSION_ID");
            let claude = env_lookup("CLAUDE_SESSION_ID");
            identity(nonempty(cosmon.as_deref()).or(nonempty(claude.as_deref())))
        }
    }
}

/// A matching fresh identity marks the supported launch fallback. This is
/// provenance, not an authority token; takeover requires an operator signature.
pub const LAUNCH_MARKER: &str = "COSMON_SESSION_LAUNCH_ID";

/// Return the launch identity only when both exports agree and are nonempty.
pub fn marked_launch_id<F>(env_lookup: &F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    let id = env_lookup("COSMON_SESSION_ID")?;
    let marker = env_lookup(LAUNCH_MARKER)?;
    (!id.trim().is_empty() && id == marker).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_are_independently_usable_but_conflicts_are_redacted() -> Result<(), ResolutionError>
    {
        for key in ["CODEX_THREAD_ID", "CODEX_SESSION_ID"] {
            let sid = resolve(None, Caller::Shell, |name| {
                (name == key).then(|| "native-fixture".to_owned())
            })?;
            assert_eq!(sid.as_ref().map(SessionId::as_str), Some("native-fixture"));
        }
        let result = resolve(None, Caller::Shell, |name| match name {
            "CODEX_THREAD_ID" => Some("private-fixture-a".to_owned()),
            "CODEX_SESSION_ID" => Some("private-fixture-b".to_owned()),
            _ => None,
        });
        assert!(matches!(
            result,
            Err(ResolutionError::ConflictingCodexEnvironment)
        ));
        assert!(!result
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default()
            .contains("private-fixture"));
        Ok(())
    }
}
