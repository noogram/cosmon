// SPDX-License-Identifier: Apache-2.0

//! `session-probe-core` — the provider-neutral port for discovering agent
//! sessions and reading them incrementally, without touching them.
//!
//! This is M1 of the *co-pilotage multi-provider* mission: the layer that lets
//! a co-pilot watch a primary pilot's session live, whoever wrote that
//! session's log format. It is a **library only** — no CLI verb, no flag, no
//! output byte. `cs sessions` is M5.
//!
//! # The one question this crate answers
//!
//! *How do you follow a file someone else is writing, without lying about what
//! you saw and without the writer noticing?*
//!
//! Three failure modes make that harder than it sounds, and all three were
//! measured on the existing code before this crate was written (ADR-168,
//! probes P4/P5/P7):
//!
//! | Failure | What it looks like | What this crate does |
//! |---|---|---|
//! | The line is half-written | the parser errors on the whole file | only complete lines are consumed; the partial tail waits |
//! | The file was rotated or truncated | the reader sits past the end and reports success forever | the read rewinds and says [`Continuity::Restarted`] with a cause |
//! | The cursor lands inside a codepoint | the reading process panics | every slice is a byte slice, every line is decoded lossily |
//!
//! # Shape
//!
//! ```text
//!   ProbeRegistry ──▶ SessionProbe (per provider)
//!                        │  discover(filter) → Vec<ProviderSessionRef>
//!                        │  read(session, cursor) → ProbeRead { events, cursor, continuity }
//!                        └─ normalize(line) → SessionEvent      ← the only per-provider code
//! ```
//!
//! Adding a provider is implementing [`SessionProbe`] and registering it. If
//! that ever stops being true — if a new provider needs a cockpit change — the
//! mission's falsifier 10 has fired and this port is what is wrong.
//!
//! # What is deliberately absent
//!
//! - **No writes.** Nothing here opens a file for writing, renames one, sends a
//!   keystroke or spawns a process. OBSERVATION-NEUTRE is not a promise the
//!   caller makes; it is the absence of an API that could break it.
//! - **No conversation content.** Events carry roles, models, sizes and
//!   counters. The transcript stays with the provider (ADR-168 D3.3).
//! - **No substring matching on paths.** Repository identity is a resolved,
//!   canonicalised root ([`RepoIdentity`]), and a worktree is never its
//!   canonical checkout.
//! - **No "best match" resolution.** Discovery returns the *set*; two sessions
//!   in one working directory stay two sessions.
//!
//! # Example — follow a session across two polls
//!
//! ```no_run
//! use cosmon_session_probe::{
//!     ClaudeProbe, Cursor, DiscoveryFilter, ProbeRegistry, RepoIdentity, SessionProbe,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let registry = ProbeRegistry::new().with(Box::new(ClaudeProbe::from_home()?));
//!
//! let repo = RepoIdentity::resolve(".").ok_or("not in a repository")?;
//! let sessions = registry.discover(&DiscoveryFilter::in_repo(repo))?;
//!
//! for session in &sessions {
//!     println!("{} → {}", session.selector(), session.source_locator.display());
//! }
//!
//! if let Some(session) = sessions.first() {
//!     let probe = registry
//!         .probe_for(&session.provider)
//!         .ok_or("no adapter for that provider")?;
//!
//!     let first = probe.read(session, Cursor::start())?;
//!     println!("{} events", first.events.len());
//!
//!     // Later, from the same cursor: only what the session has since written.
//!     let next = probe.read(session, first.cursor)?;
//!     println!("{} new events ({:?})", next.events.len(), next.continuity);
//! }
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod claude;
pub mod codex;
pub mod cursor;
pub mod error;
pub mod event;
pub mod port;
pub mod repo;
pub mod selector;

pub use claude::ClaudeProbe;
pub use codex::CodexProbe;
pub use cursor::{read_lines_from, Continuity, Cursor, LineBatch, RawLine, RestartCause};
pub use error::ProbeError;
pub use event::{QuotaReading, SessionEvent, SessionEventKind, TurnUsage};
pub use port::{DiscoveryFilter, ProbeRead, ProbeRegistry, ProviderSessionRef, SessionProbe};
pub use repo::{RepoIdentity, RepoKind};
pub use selector::{NativeSessionId, ProviderName, SessionSelector};
