// SPDX-License-Identifier: AGPL-3.0-only

//! One caller-identity resolution contract for provider hooks and shell gestures.
//!
//! A provider callback may carry its native identity.  Ordinary tool
//! subprocesses do not, so a launcher must give the provider process a fresh
//! `COSMON_SESSION_ID`; its hooks and its tool subprocesses then resolve the
//! same caller.  The legacy Claude variable remains a compatibility fallback,
//! never a substitute for a launch-scoped identity.

use crate::id::SessionId;

/// Resolve one caller identity without reading ambient process state.
///
/// `explicit` is an operator-supplied override, `native` is admitted only from
/// a provider callback, and `env_lookup` is injected so every caller can test
/// the same precedence.  A native callback identity wins over inherited shell
/// state: an inherited id may belong to another concurrent pilot.
pub fn resolve<F>(
    explicit: Option<&str>,
    native: Option<&str>,
    env_lookup: F,
) -> Result<Option<SessionId>, crate::id::IdError>
where
    F: Fn(&str) -> Option<String>,
{
    for value in explicit
        .into_iter()
        .chain(native)
        .map(str::trim)
        .filter(|value| !value.is_empty())
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
