// SPDX-License-Identifier: AGPL-3.0-only

//! One caller-identity resolution contract for provider hooks and shell gestures.
//!
//! A provider callback carries native identity; tool environments do not
//! establish that binding uniformly. The launcher gives the provider a fresh
//! `COSMON_SESSION_ID`; its hooks and its tool subprocesses then resolve the
//! same caller.  The legacy Claude variable remains a compatibility fallback,
//! never a substitute for a launch-scoped identity.

use crate::id::SessionId;

/// Resolve one caller identity without reading ambient process state.
///
/// `explicit` is an operator-supplied override, `native` is admitted only from
/// a provider callback, and `env_lookup` is injected so every caller can test
/// the same precedence.  A native callback identity wins over inherited shell
/// state unless the launcher marked it: an unmarked id may belong to another pilot.
///
/// # Errors
/// Returns an identifier error when the selected nonempty identity is invalid.
pub fn resolve<F>(
    explicit: Option<&str>,
    native: Option<&str>,
    env_lookup: F,
) -> Result<Option<SessionId>, crate::id::IdError>
where
    F: Fn(&str) -> Option<String>,
{
    let launch = marked_launch_id(&env_lookup);
    if let Some(value) = explicit
        .into_iter()
        .chain(launch.as_deref())
        .chain(native)
        .map(str::trim)
        .find(|value| !value.is_empty())
    {
        return SessionId::new(value.to_owned()).map(Some);
    }
    for name in ["COSMON_SESSION_ID", "CLAUDE_SESSION_ID"] {
        if let Some(value) = env_lookup(name) {
            let value = value.trim();
            if !value.is_empty() {
                return SessionId::new(value.to_owned()).map(Some);
            }
        }
    }
    Ok(None)
}

/// The launch marker duplicates the fresh identity so stale or partial exports
/// cannot accidentally claim the supported launch contract. It is provenance,
/// not an authority token; takeover still requires an operator signature.
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
    fn provider_callback_refuses_an_inherited_pilot_identity() {
        let sid = resolve(None, Some("native-b"), |name| {
            (name == "COSMON_SESSION_ID").then(|| "native-a".to_owned())
        })
        .unwrap()
        .unwrap();
        assert_eq!(sid.as_str(), "native-b");
    }

    #[test]
    fn shell_uses_the_launch_identity() {
        let sid = resolve(None, None, |name| {
            (name == "COSMON_SESSION_ID").then(|| "launch-a".to_owned())
        })
        .unwrap()
        .unwrap();
        assert_eq!(sid.as_str(), "launch-a");
    }
}
