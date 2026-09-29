// SPDX-License-Identifier: AGPL-3.0-only

//! Disk-backed deny-list — clauses (a)+(c), kill-switch (ADR-080 §7).
//!
//! The deny-list lives at `<state_dir>/security/oidc-policy.toml` and
//! the global blast-door at `<state_dir>/security/oidc-kill.toml`.
//! Both are re-read with a 30 s TTL — operator intent → adapter
//! effect ≤ 30 s without a redeploy.
//!
//! Operator commands writing these files (`cs security oidc revoke`,
//! `cs security oidc kill`) are themselves operator-only verbs (§5);
//! the RPP only **reads** the files.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{de::DeserializeOwned, Deserialize};

use crate::nucleon_map::Noyau;

/// Default TTL for the cached deny-list snapshot.
pub const DEFAULT_TTL: Duration = Duration::from_secs(30);

/// Disk-backed kill-switch + revocation list with a TTL'd cache.
#[derive(Debug)]
pub struct DenyList {
    state_dir: PathBuf,
    ttl: Duration,
    cache: Mutex<CacheSlot>,
}

#[derive(Debug, Default)]
struct CacheSlot {
    /// Cached snapshot (defaults to "nothing denied" before the first read).
    snapshot: Snapshot,
    /// Last refresh attempt, including a failed one that closed admission.
    refreshed_at: Option<Instant>,
}

/// Read-only view of the deny-list at a point in time.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// Global blast-door (every request rejected).
    pub global_kill: bool,
    /// Set of revoked `sub` BLAKE3 hex hashes.
    pub denied_sub_hashes: Vec<String>,
    /// Set of revoked `jti` strings.
    pub denied_jtis: Vec<String>,
    /// Set of revoked tenants (`noyau` values).
    pub denied_noyaus: Vec<String>,
}

impl DenyList {
    /// Construct a deny-list rooted at `<state_dir>/security/`.
    #[must_use]
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            state_dir,
            ttl: DEFAULT_TTL,
            cache: Mutex::new(CacheSlot::default()),
        }
    }

    /// Override the cache TTL (tests rely on `Duration::ZERO` to
    /// force every read).
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Refresh the cache if the TTL elapsed. A read or parse error closes
    /// admission until a later successful refresh.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let now = Instant::now();
        let mut slot = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let refresh_due = match slot.refreshed_at {
            Some(t) => now.duration_since(t) >= self.ttl,
            None => true,
        };
        if refresh_due {
            slot.snapshot = match read_from_disk(&self.state_dir) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    tracing::error!(%error, "OIDC deny-list unavailable; refusing all admission until the next successful refresh");
                    Snapshot {
                        global_kill: true,
                        ..Snapshot::default()
                    }
                }
            };
            slot.refreshed_at = Some(now);
        }
        slot.snapshot.clone()
    }

    /// Force a re-read on the next [`Self::snapshot`] call. Used by
    /// integration tests that mutate the deny-list mid-flight.
    pub fn invalidate(&self) {
        let mut slot = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.refreshed_at = None;
    }

    /// Convenience — true iff the global blast-door is closed.
    #[must_use]
    pub fn is_globally_killed(&self) -> bool {
        self.snapshot().global_kill
    }

    /// True when `sub_hash` is denied or the global door is closed.
    #[must_use]
    pub fn is_sub_revoked(&self, sub_hash: &str) -> bool {
        let snapshot = self.snapshot();
        snapshot.global_kill || snapshot.denied_sub_hashes.iter().any(|s| s == sub_hash)
    }

    /// True when `jti` is denied or the global door is closed.
    #[must_use]
    pub fn is_jti_revoked(&self, jti: &str) -> bool {
        let snapshot = self.snapshot();
        snapshot.global_kill || snapshot.denied_jtis.iter().any(|j| j == jti)
    }

    /// True when `noyau` is paused or the global door is closed.
    #[must_use]
    pub fn is_noyau_revoked(&self, noyau: &Noyau) -> bool {
        let snapshot = self.snapshot();
        snapshot.global_kill || snapshot.denied_noyaus.iter().any(|n| n == noyau.as_str())
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct KillFile {
    #[serde(default)]
    global: GlobalKill,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobalKill {
    #[serde(default)]
    enabled: bool,
    #[serde(rename = "reason")]
    _reason: Option<String>,
    #[serde(rename = "since")]
    _since: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    #[serde(default)]
    deny: DenyEntries,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DenyEntries {
    #[serde(default, rename = "sub")]
    subs: Vec<DeniedSub>,
    #[serde(default, rename = "jti")]
    jtis: Vec<DeniedJti>,
    #[serde(default, rename = "noyau")]
    noyaus: Vec<DeniedNoyau>,
}

#[derive(Debug, Deserialize)]
struct DeniedSub {
    sub_hash: String,
}

#[derive(Debug, Deserialize)]
struct DeniedJti {
    jti: String,
}

#[derive(Debug, Deserialize)]
struct DeniedNoyau {
    noyau: String,
}

fn read_from_disk(state_dir: &std::path::Path) -> Result<Snapshot, String> {
    let mut snap = Snapshot::default();
    let kill_path = state_dir.join("security/oidc-kill.toml");
    if let Some(file) = read_optional_toml::<KillFile>(&kill_path)? {
        snap.global_kill = file.global.enabled;
    }
    let policy_path = state_dir.join("security/oidc-policy.toml");
    if let Some(file) = read_optional_toml::<PolicyFile>(&policy_path)? {
        snap.denied_sub_hashes = file.deny.subs.into_iter().map(|d| d.sub_hash).collect();
        snap.denied_jtis = file.deny.jtis.into_iter().map(|d| d.jti).collect();
        snap.denied_noyaus = file.deny.noyaus.into_iter().map(|d| d.noyau).collect();
    }
    Ok(snap)
}

fn read_optional_toml<T: DeserializeOwned>(path: &std::path::Path) -> Result<Option<T>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A dangling symlink has a directory entry, so it is an unreadable
            // operator file rather than an intentionally absent optional one.
            match std::fs::symlink_metadata(path) {
                Err(metadata_error) if metadata_error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(None);
                }
                _ => return Err(format!("cannot read {}: {error}", path.display())),
            }
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    toml::from_str(&text)
        .map(Some)
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_kill(td: &TempDir, enabled: bool) {
        let dir = td.path().join("security");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("oidc-kill.toml"),
            format!("[global]\nenabled = {enabled}\n"),
        )
        .unwrap();
    }

    fn write_policy(td: &TempDir, body: &str) {
        let dir = td.path().join("security");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("oidc-policy.toml"), body).unwrap();
    }

    #[test]
    fn empty_dir_means_nothing_denied() {
        let td = TempDir::new().unwrap();
        let dl = DenyList::new(td.path().to_path_buf()).with_ttl(Duration::ZERO);
        assert!(!dl.is_globally_killed());
        assert!(!dl.is_sub_revoked("anything"));
    }

    #[test]
    fn reads_global_kill() {
        let td = TempDir::new().unwrap();
        write_kill(&td, true);
        let dl = DenyList::new(td.path().to_path_buf()).with_ttl(Duration::ZERO);
        assert!(dl.is_globally_killed());
    }

    #[test]
    fn reads_revoked_sub_and_jti() {
        let td = TempDir::new().unwrap();
        write_policy(
            &td,
            r#"
[[deny.sub]]
sub_hash = "blake3:abcd"
reason = "compromised"
since = "2026-04-27T15:00:00Z"

[[deny.jti]]
jti = "tok-x"
reason = "leak"
since = "2026-04-27T16:00:00Z"

[[deny.noyau]]
noyau = "tenant-demo"
reason = "tenant pause"
since = "2026-04-27T17:00:00Z"
"#,
        );
        let dl = DenyList::new(td.path().to_path_buf()).with_ttl(Duration::ZERO);
        assert!(dl.is_sub_revoked("blake3:abcd"));
        assert!(dl.is_jti_revoked("tok-x"));
        assert!(dl.is_noyau_revoked(&Noyau::new("tenant-demo")));
        assert!(!dl.is_jti_revoked("tok-y"));
    }

    #[test]
    fn invalidate_forces_reread() {
        let td = TempDir::new().unwrap();
        let dl = DenyList::new(td.path().to_path_buf()).with_ttl(Duration::from_secs(3600));
        // First snapshot — empty.
        assert!(!dl.is_globally_killed());
        write_kill(&td, true);
        // Without invalidation, the cache still says "no".
        assert!(!dl.is_globally_killed());
        dl.invalidate();
        assert!(dl.is_globally_killed());
    }
}
