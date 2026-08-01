// SPDX-License-Identifier: Apache-2.0

//! The port itself: discovery, incremental read, and the registry that makes a
//! new provider an adapter rather than a cockpit edit.
//!
//! [`SessionProbe`] has exactly two required methods — *enumerate the sessions
//! you can see* and *normalise one of your lines*. Incremental reading is
//! provided, because the cursor discipline (partial trailing line, truncation,
//! rotation, byte-safe slicing) is the part every adapter would otherwise get
//! wrong in its own way. That is the M1 entry criterion ADR-168 added.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::cursor::{read_lines_from, Continuity, Cursor, RawLine};
use crate::error::ProbeError;
use crate::event::SessionEvent;
use crate::repo::RepoIdentity;
use crate::selector::{NativeSessionId, ProviderName, SessionSelector};

/// A session a probe can see, keyed the way the protocol keys it.
///
/// Every field except the first two is context for a human or a filter.
/// `display_name` in particular is an alias and nothing more: it is never read
/// by a comparison, which is what makes an unnamed session work exactly like a
/// renamed one (mission falsifier 3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSessionRef {
    /// Which adapter owns this session.
    pub provider: ProviderName,
    /// The provider's own id for it — half of the canonical key.
    pub native_session_id: NativeSessionId,
    /// The resolved repository, when the session's `cwd` is inside one.
    pub repo_identity: Option<RepoIdentity>,
    /// The working directory as recorded *inside* the log.
    pub cwd: Option<PathBuf>,
    /// Absolute path of the provider log this session is read from.
    pub source_locator: PathBuf,
    /// A human-facing title, when the provider records one. Alias only.
    pub display_name: Option<String>,
    /// First timestamp seen in the log.
    pub started_at: Option<DateTime<Utc>>,
    /// Last time the log itself changed (its mtime) — not a heartbeat, and not
    /// evidence the session is alive.
    pub last_observed_at: Option<DateTime<Utc>>,
}

impl ProviderSessionRef {
    /// The canonical `<provider>:<native-session-id>` key.
    #[must_use]
    pub fn selector(&self) -> SessionSelector {
        SessionSelector::new(self.provider.clone(), self.native_session_id.clone())
    }
}

/// What to keep when enumerating sessions.
///
/// There is intentionally no `name_contains` and no `path_contains`. REPO-EXACT
/// is enforced by the absence of the predicate that would break it, not by a
/// comment asking callers not to use it.
#[derive(Clone, Debug, Default)]
pub struct DiscoveryFilter {
    /// Keep only sessions whose resolved repository equals this one, exactly.
    /// A session whose repo cannot be resolved is dropped — unknown is not a
    /// match.
    pub repo: Option<RepoIdentity>,
    /// Keep only sessions whose recorded `cwd` canonicalises to this path.
    pub cwd: Option<PathBuf>,
}

impl DiscoveryFilter {
    /// Everything the adapters can see.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// Restrict to one repository.
    #[must_use]
    pub fn in_repo(repo: RepoIdentity) -> Self {
        Self {
            repo: Some(repo),
            cwd: None,
        }
    }

    /// Whether `session` survives this filter.
    #[must_use]
    pub fn accepts(&self, session: &ProviderSessionRef) -> bool {
        if let Some(want) = &self.repo {
            match &session.repo_identity {
                Some(got) if got.is_same(want) => {}
                _ => return false,
            }
        }
        if let Some(want) = &self.cwd {
            let want = std::fs::canonicalize(want).unwrap_or_else(|_| want.clone());
            let got = session
                .cwd
                .as_ref()
                .map(|c| std::fs::canonicalize(c).unwrap_or_else(|_| c.clone()));
            if got.as_ref() != Some(&want) {
                return false;
            }
        }
        true
    }
}

/// The outcome of one incremental read.
#[derive(Clone, Debug)]
pub struct ProbeRead {
    /// Normalised events, in file order.
    pub events: Vec<SessionEvent>,
    /// Cursor to hand back on the next call.
    pub cursor: Cursor,
    /// Whether this read resumed, started fresh, or rewound — and why.
    pub continuity: Continuity,
    /// Bytes of a partial trailing line left unconsumed.
    pub pending_bytes: u64,
}

/// A provider adapter.
///
/// Implementing it is the *whole* cost of adding a provider. If a future
/// provider cannot be added without also touching `cs sessions`, mission
/// falsifier 10 has fired and this trait is what needs fixing.
pub trait SessionProbe {
    /// The provider this adapter speaks for.
    fn provider(&self) -> &ProviderName;

    /// Enumerate the sessions this adapter can see, keeping only those the
    /// filter accepts.
    ///
    /// Returns the **set**, never a "best" pick: collapsing two sessions in
    /// one working directory to the most recently modified one is the mission's
    /// own falsifier 3, and it is what `resolve_codex_session_by_cwd` does
    /// today (probe P6).
    ///
    /// # Errors
    ///
    /// [`ProbeError::Io`] when the provider's session tree exists but cannot be
    /// read. A tree that simply does not exist is not an error — a host without
    /// Codex installed has zero Codex sessions, not a fault.
    fn discover(&self, filter: &DiscoveryFilter) -> Result<Vec<ProviderSessionRef>, ProbeError>;

    /// Normalise one complete line of this provider's log.
    ///
    /// Must be total: every line maps to some event, with
    /// [`SessionEventKind::Unparseable`](crate::event::SessionEventKind::Unparseable)
    /// as the floor. A live log is allowed to contain anything.
    fn normalize(&self, line: &RawLine) -> SessionEvent;

    /// Read everything the session has gained since `cursor`.
    ///
    /// Provided, and adapters should not override it: this is where the
    /// partial-line, truncation, rotation and byte-slicing discipline lives.
    ///
    /// # Errors
    ///
    /// [`ProbeError::Io`] if the log cannot be opened or read.
    fn read(&self, session: &ProviderSessionRef, cursor: Cursor) -> Result<ProbeRead, ProbeError> {
        let batch = read_lines_from(&session.source_locator, cursor)?;
        let events = batch.lines.iter().map(|l| self.normalize(l)).collect();
        Ok(ProbeRead {
            events,
            cursor: batch.cursor,
            continuity: batch.continuity,
            pending_bytes: batch.pending_bytes,
        })
    }
}

/// The set of adapters a cockpit knows about.
///
/// The registry is the executable form of falsifier 10: `cs sessions` will talk
/// to this type, and a third provider is a `register` call plus a file.
#[derive(Default)]
pub struct ProbeRegistry {
    probes: Vec<Box<dyn SessionProbe>>,
}

impl ProbeRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self { probes: Vec::new() }
    }

    /// Add an adapter.
    #[must_use]
    pub fn with(mut self, probe: Box<dyn SessionProbe>) -> Self {
        self.probes.push(probe);
        self
    }

    /// The providers currently registered, in registration order.
    #[must_use]
    pub fn providers(&self) -> Vec<&ProviderName> {
        self.probes.iter().map(|p| p.provider()).collect()
    }

    /// The adapter for a provider, if registered.
    #[must_use]
    pub fn probe_for(&self, provider: &ProviderName) -> Option<&dyn SessionProbe> {
        self.probes
            .iter()
            .find(|p| p.provider() == provider)
            .map(|p| &**p)
    }

    /// Enumerate every session every adapter can see, filtered.
    ///
    /// # Errors
    ///
    /// The first adapter error, unchanged — a cockpit that cannot read one
    /// provider's tree must say so rather than show a shorter list.
    pub fn discover(
        &self,
        filter: &DiscoveryFilter,
    ) -> Result<Vec<ProviderSessionRef>, ProbeError> {
        let mut all = Vec::new();
        for probe in &self.probes {
            all.extend(probe.discover(filter)?);
        }
        Ok(all)
    }

    /// Every session matching a canonical selector.
    ///
    /// Returns a `Vec` rather than an `Option` on purpose: one selector should
    /// match exactly one session, and if it ever matches two the caller must
    /// see both instead of being handed an arbitrary winner.
    ///
    /// # Errors
    ///
    /// As [`Self::discover`].
    pub fn candidates(
        &self,
        selector: &SessionSelector,
    ) -> Result<Vec<ProviderSessionRef>, ProbeError> {
        let Some(probe) = self.probe_for(&selector.provider) else {
            return Ok(Vec::new());
        };
        Ok(probe
            .discover(&DiscoveryFilter::all())?
            .into_iter()
            .filter(|s| s.native_session_id == selector.native_session_id)
            .collect())
    }
}

/// The mtime of a path as a UTC timestamp, when the filesystem offers one.
pub(crate) fn mtime_of(path: &std::path::Path) -> Option<DateTime<Utc>> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(DateTime::<Utc>::from(modified))
}
