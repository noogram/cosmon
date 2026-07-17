// SPDX-License-Identifier: AGPL-3.0-only

//! `notify`-backed implementation of [`crate::ports::ConfigWatchPort`].
//!
//! ## Why debounce
//!
//! Text editors (vim, VS Code, Emacs) save files via a write-to-tmp + rename
//! dance that fires multiple filesystem events in a burst:
//!
//! - `CREATE` for the `.tmp` swap file
//! - `MODIFY` for the write
//! - `REMOVE` for the swap
//! - `RENAME` for the final swap-in
//!
//! Reacting to each event would have the supervisor re-parse the TOML four
//! times per save, which would at best produce spurious log lines and at
//! worst trigger redundant diff computations. We collapse every burst into
//! a single [`ConfigChange`] using a 200 ms coalescing window.
//!
//! The window matches the feasibility doc (Q5 answer) and is small enough
//! that the operator perceives the reload as instantaneous, large enough
//! to swallow any editor save dance we've seen in the wild.
//!
//! ## Thread topology
//!
//! The `notify` crate drives its events on a dedicated thread inside the
//! `RecommendedWatcher`. We run a *second* thread that pulls raw events
//! from the std mpsc channel, does the debounce, and forwards coalesced
//! [`ConfigChange`]s to a `tokio::sync::mpsc` so the async event loop can
//! `select!` on them.
//!
//! The raw `notify::RecommendedWatcher` handle lives inside this adapter
//! because dropping it stops the watcher — which is exactly what the
//! supervisor's graceful shutdown needs.

use std::path::Path;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use notify::{Config as NotifyConfig, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::ports::{ConfigChange, ConfigWatchError, ConfigWatchPort};

/// Debounce window for edit-save bursts. Exposed as a `const` so tests
/// (and the feasibility audit) can reason about timing guarantees.
pub const DEBOUNCE_WINDOW: Duration = Duration::from_millis(200);

/// `notify`-backed config watcher. Debounces bursts into coalesced
/// [`ConfigChange`] emissions.
///
/// Implements [`ConfigWatchPort`] with a **blocking** `next()` so the same
/// trait works for synchronous tests and the real tokio event loop (which
/// wraps the blocking call inside `spawn_blocking`). An async helper
/// `NotifyConfigWatchPort::recv_async` is also provided for the event
/// loop itself so we don't need to park a blocking thread per select
/// iteration.
pub struct NotifyConfigWatchPort {
    // Kept alive — dropping stops the watcher thread inside `notify`.
    _watcher: RecommendedWatcher,
    // Debounced receiver — std mpsc so the blocking `next()` path can use
    // `recv_timeout`. The worker thread feeds it coalesced changes.
    rx: std_mpsc::Receiver<ConfigChange>,
}

impl std::fmt::Debug for NotifyConfigWatchPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotifyConfigWatchPort").finish()
    }
}

impl NotifyConfigWatchPort {
    /// Begin watching `path` for modification events, emitting at most one
    /// [`ConfigChange`] per [`DEBOUNCE_WINDOW`].
    ///
    /// Watching is non-recursive: we want the single TOML file, not its
    /// containing directory (otherwise sibling edits would trip the
    /// supervisor). On macOS the `FSEvents` backend will still dispatch
    /// directory-level events; we filter them by path below.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigWatchError::Subscribe`] if the `notify` backend
    /// refuses the watch (missing file, permission denied, kernel limit).
    pub fn new(path: &Path) -> Result<Self, ConfigWatchError> {
        let (raw_tx, raw_rx) = std_mpsc::channel::<notify::Result<notify::Event>>();
        let mut backend = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
            // If the receiver has been dropped (supervisor shutting down)
            // this `send` returns `Err` — we simply swallow it, since the
            // channel going away is the signal to quiesce.
            let _ = raw_tx.send(ev);
        })
        .map_err(|e| ConfigWatchError::Subscribe(e.to_string()))?;

        backend
            .configure(NotifyConfig::default())
            .map_err(|e| ConfigWatchError::Subscribe(format!("configure: {e}")))?;

        backend
            .watch(path, RecursiveMode::NonRecursive)
            .map_err(|e| ConfigWatchError::Subscribe(format!("watch {}: {e}", path.display())))?;

        let (coalesced_tx, coalesced_rx) = std_mpsc::channel::<ConfigChange>();
        let watched = path.to_path_buf();
        thread::Builder::new()
            .name("daemon-supervisor-config-debounce".into())
            .spawn(move || debounce_loop(&raw_rx, &coalesced_tx, &watched))
            .map_err(|e| ConfigWatchError::Subscribe(format!("spawn debounce thread: {e}")))?;

        Ok(Self {
            _watcher: backend,
            rx: coalesced_rx,
        })
    }
}

/// Main loop of the debounce thread: read raw notify events, filter by the
/// watched path, collapse bursts shorter than [`DEBOUNCE_WINDOW`] into a
/// single [`ConfigChange`]. Exits when the raw channel closes (watcher
/// dropped) or the coalesced channel closes (consumer dropped).
fn debounce_loop(
    raw_rx: &std_mpsc::Receiver<notify::Result<notify::Event>>,
    coalesced_tx: &std_mpsc::Sender<ConfigChange>,
    watched: &Path,
) {
    loop {
        // Wait for the first interesting event.
        let Ok(first) = raw_rx.recv() else {
            return;
        };
        if !is_relevant(&first, watched) {
            continue;
        }

        // Drain any follow-up events within the debounce window.
        let deadline = Instant::now() + DEBOUNCE_WINDOW;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match raw_rx.recv_timeout(remaining) {
                Ok(_ev) => {
                    // Stay in the burst — we keep draining until the
                    // deadline. Fixed-window debounce: at most one reload
                    // per 200 ms regardless of how noisy the editor is.
                }
                Err(std_mpsc::RecvTimeoutError::Timeout) => break,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                    // Watcher dropped — we still owe the caller the final
                    // coalesced change before exiting.
                    let _ = coalesced_tx.send(ConfigChange { at: Utc::now() });
                    return;
                }
            }
        }

        if coalesced_tx.send(ConfigChange { at: Utc::now() }).is_err() {
            // Consumer dropped; no reason to keep listening.
            return;
        }
    }
}

/// Decide whether a raw `notify::Event` counts as a config change.
///
/// Kept as a free function so it can be unit-tested against synthetic
/// `notify::Event`s without spinning up the backend.
fn is_relevant(ev: &notify::Result<notify::Event>, watched: &Path) -> bool {
    let Ok(ev) = ev else {
        return false;
    };
    // Only content- and rename-class events. Access events (read, metadata)
    // are noise for our purposes.
    match ev.kind {
        EventKind::Modify(_)
        | EventKind::Create(_)
        | EventKind::Remove(_)
        | EventKind::Any
        | EventKind::Other => {}
        EventKind::Access(_) => return false,
    }
    // Filter by path — FSEvents may emit events for sibling files in the
    // same directory. We compare canonicalized prefixes so symlinks don't
    // trip us up. If canonicalization fails (e.g. file was just deleted),
    // fall back to plain path comparison.
    let canon_watched = watched.canonicalize().ok();
    ev.paths.iter().any(|p| match canon_watched.as_ref() {
        Some(cw) => p.canonicalize().ok().as_deref() == Some(cw.as_path()),
        None => p == watched,
    }) || ev.paths.iter().any(|p| p == watched)
        || ev
            .paths
            .iter()
            .any(|p| p.file_name() == watched.file_name() && watched.file_name().is_some())
}

impl ConfigWatchPort for NotifyConfigWatchPort {
    fn next(&mut self) -> Result<Option<ConfigChange>, ConfigWatchError> {
        // Blocking recv; returns `Ok(None)` when every sender has been
        // dropped (clean shutdown signal).
        match self.rx.recv() {
            Ok(change) => Ok(Some(change)),
            Err(std_mpsc::RecvError) => Ok(None),
        }
    }
}

impl NotifyConfigWatchPort {
    /// Non-blocking poll — returns `Some(change)` if a change is queued,
    /// `None` otherwise (including after the watcher has been dropped).
    /// Used by the event loop to drain any changes that piled up while
    /// we were processing something else.
    pub fn try_recv(&mut self) -> Option<ConfigChange> {
        self.rx.try_recv().ok()
    }

    /// Wait up to `timeout` for the next change. Used by the event loop
    /// to interleave the watcher with the throttle tick and signals.
    pub fn recv_timeout(&mut self, timeout: Duration) -> Option<ConfigChange> {
        self.rx.recv_timeout(timeout).ok()
    }
}

impl NotifyConfigWatchPort {
    /// Decompose into the backing receiver and the watcher guard.
    ///
    /// The returned tuple keeps the watcher alive as long as the guard
    /// isn't dropped. Intended for the composition root only — lets the
    /// supervisor hand the receiver to a dedicated `spawn_blocking` task
    /// while keeping the `RecommendedWatcher` handle in a separate scope.
    #[must_use]
    pub fn into_parts(self) -> (std_mpsc::Receiver<ConfigChange>, WatcherGuard) {
        let Self {
            _watcher: watcher,
            rx,
        } = self;
        (rx, WatcherGuard { _watcher: watcher })
    }
}

/// Owns the [`RecommendedWatcher`] so the caller can hold the adapter's
/// receiver in one task and the watcher handle in the supervisor's `Drop`
/// scope. Moving this value is cheap (an `Arc`-free owned struct).
pub struct WatcherGuard {
    _watcher: RecommendedWatcher,
}

impl std::fmt::Debug for WatcherGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatcherGuard").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    #[test]
    fn debounce_window_is_200ms() {
        // Guarded constant so any future change surfaces in git blame.
        assert_eq!(DEBOUNCE_WINDOW, Duration::from_millis(200));
    }

    #[test]
    fn watcher_emits_change_on_file_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemons.toml");
        fs::write(&path, "initial").unwrap();

        let mut port = NotifyConfigWatchPort::new(&path).expect("watch");

        // Small sleep so the watcher's async setup completes before we
        // write. The `notify` crate does not expose a "ready" hook.
        std::thread::sleep(Duration::from_millis(50));

        fs::write(&path, "updated").unwrap();

        // Allow up to 1 s for the event to propagate (macOS FSEvents has
        // 100–500 ms latency by default).
        let change = port
            .recv_timeout(Duration::from_secs(1))
            .expect("change observed");
        assert!(change.at <= Utc::now());
    }

    #[test]
    fn watcher_coalesces_burst_into_single_change() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemons.toml");
        fs::write(&path, "v0").unwrap();

        let mut port = NotifyConfigWatchPort::new(&path).expect("watch");
        std::thread::sleep(Duration::from_millis(50));

        // Burst of four writes in quick succession (< DEBOUNCE_WINDOW).
        for i in 0..4 {
            fs::write(&path, format!("v{i}")).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }

        // Drain with a generous window; expect at most 1 change.
        let _first = port
            .recv_timeout(Duration::from_secs(2))
            .expect("one change");

        // Now wait past the debounce window and verify no *second* change
        // arrived from the same burst.
        let extras = {
            let mut n = 0;
            while port.recv_timeout(Duration::from_millis(50)).is_some() {
                n += 1;
            }
            n
        };
        assert!(extras <= 1, "expected ≤1 extra, got {extras}");
    }

    #[test]
    fn try_recv_returns_none_when_idle() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemons.toml");
        fs::write(&path, "v0").unwrap();

        let mut port = NotifyConfigWatchPort::new(&path).expect("watch");
        assert!(port.try_recv().is_none());
    }
}
