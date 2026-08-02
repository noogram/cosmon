// SPDX-License-Identifier: AGPL-3.0-only

//! The event-driven briefing receipt: a signature by Claude Code that the
//! briefing cosmon pasted entered its `UserPromptSubmit` lifecycle.
//!
//! # Why this exists
//!
//! [`TmuxBackend::send_input_observed`](crate::tmux::TmuxBackend) learns that a
//! briefing was submitted by pausing, pressing the carriage return, then
//! reading the composer until the pasted text is no longer there. Both of those
//! are *inferences from pixels*. Claude Code fires a `UserPromptSubmit` hook and
//! `claude --settings <file>` installs one per session, so the application can
//! instead sign a receipt naming the dispatch.
//!
//! # What the measurement said, and what this module is therefore allowed to do
//!
//! `experiments/briefing-receipt-hook/RESULTS.md` measured 171 dispatches of
//! this mechanism. Everything below is shaped by eight findings from it, and
//! each is restated at the code it constrains:
//!
//! 1. **The composer poll stays.** The composer read observed a submit in 15/15
//!    production-shape trials; the receipt failed to arrive in 6 % of dispatches
//!    where the hook was installed and working. The weaker signal is the more
//!    available one. This module *adds* an early exit; it removes nothing.
//! 2. **A receipt-driven loop must consult the composer before every re-press.**
//!    On a pane that is already mid-response Claude Code *queues* the paste: the
//!    composer empties in ~0.9 s while `UserPromptSubmit` does not fire for 5–6 s.
//!    A loop that presses until the receipt arrives sent **23 carriage returns**
//!    per dispatch instead of 1. See [`await_submit_evidence`].
//! 3. **The outcome is typed, never a bool.** [`SubmitEvidence`] keeps
//!    "the application said so" and "we read pixels" as distinct variants, and
//!    every demotion carries a [`FallbackReason`]. 8/8 broken-hook trials demoted
//!    correctly; a `bool submitted` would have thrown that away.
//! 4. **`EventAck` never means "the worker is working".** Measured, 3/3: a
//!    blocking hook *rejected* the prompt and the receipt was written anyway. See
//!    [`SubmitEvidence::EventAck`].
//! 5. **The hook is a subcommand of the already-compiled `cs`, by absolute
//!    path** — not an interpreter, and above all not a version-manager shim. See
//!    [`hook_command`].
//! 6. **The hook mutes stdout structurally, on its first statement.** A probe
//!    showed the model obeying an instruction that existed only in a hook's
//!    stdout, 3/3: a stray line becomes unattributed instructions in every
//!    dispatched briefing. See [`record_hook_ack`] and its CLI caller.
//! 7. **The deadline is generous.** A busy pane's receipt arrives 5–6 s after the
//!    paste and one trial in five drained at 8.1 s. See [`ACK_DEADLINE_MS`].
//! 8. **One file per dispatch, swept.** The prototype grew the receipt directory
//!    without bound. See [`ReceiptStation::consume`] and
//!    [`ReceiptStation::prune`].

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cosmon_core::id::WorkerId;

/// How long [`await_submit_evidence`] will keep waiting for a receipt once the
/// composer has stopped being a reason to press again.
///
/// Generous on purpose (RESULTS §Recommendation 8). A busy pane's receipt
/// arrives 5–6 s after the paste because Claude Code queues the message, and one
/// busy trial in five drained at 8.1 s — a shorter deadline manufactures
/// fallbacks on exactly the workers that are busiest. Nothing waits on this:
/// [`await_submit_evidence`] returns the moment either signal lands, so the
/// deadline only bounds the pathological case where *neither* does.
pub const ACK_DEADLINE_MS: u64 = 12_000;

/// Interval between receipt polls.
///
/// Two orders of magnitude cheaper than the composer poll it runs alongside: a
/// `stat` on a local file rather than a `capture-pane` subprocess. This is what
/// buys the 3–4× latency win when the receipt arrives before the composer
/// clears (253 ms median against 890 ms, run D).
pub const ACK_POLL_INTERVAL_MS: u64 = 50;

/// Age past which [`ReceiptStation::prune`] deletes a receipt nobody claimed.
///
/// Comfortably beyond [`ACK_DEADLINE_MS`]: a receipt older than this can no
/// longer be the answer to any dispatch still in flight.
pub const RECEIPT_MAX_AGE_SECS: u64 = 300;

/// Environment variable naming the directory the hook writes receipts into.
pub const ENV_RECEIPT_DIR: &str = "COSMON_RECEIPT_DIR";

/// Environment variable naming the file the hook reads the current nonce from.
pub const ENV_RECEIPT_NONCE_FILE: &str = "COSMON_RECEIPT_NONCE_FILE";

/// Environment variable overriding the root under which per-worker receipt
/// directories are created. Absent, [`receipt_root`] falls back to the
/// system temp directory.
pub const ENV_RECEIPT_ROOT: &str = "COSMON_RECEIPT_ROOT";

/// The `cs` subcommand that plays the hook. Spelled once, here, because the
/// spawn side writes it into a settings file and the CLI side must answer to
/// exactly that string.
pub const HOOK_SUBCOMMAND: &str = "briefing-receipt-hook";

/// The nonce keying one dispatch to its receipt.
///
/// A newtype rather than a `String` because this value is used as a **filename**
/// on a path the hook composes, and the hook reads it from a file cosmon wrote.
/// Constructing one is the only place the filename-safety rule lives, so a
/// hostile or corrupted nonce cannot become `../../../etc/passwd` further down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptNonce(String);

impl ReceiptNonce {
    /// Longest nonce accepted. A nonce is 16 hex characters in practice; the
    /// cap exists for the hostile-input path, where the bytes come off disk.
    const MAX_LEN: usize = 64;

    /// Mint a fresh nonce for one dispatch.
    #[must_use]
    pub fn mint() -> Self {
        use rand::Rng as _;
        let bytes: [u8; 8] = rand::thread_rng().gen();
        let mut s = String::with_capacity(16);
        for b in bytes {
            let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
        }
        Self(s)
    }

    /// Accept a nonce read from disk, keeping only what is safe as a filename.
    ///
    /// Filters to `[A-Za-z0-9_-]` and truncates. Returns `None` when nothing
    /// survives, so the caller must decide what an unkeyed submission means
    /// rather than silently writing `ack-.json`.
    #[must_use]
    pub fn sanitize(raw: &str) -> Option<Self> {
        let filtered: String = raw
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .take(Self::MAX_LEN)
            .collect();
        if filtered.is_empty() {
            None
        } else {
            Some(Self(filtered))
        }
    }

    /// The nonce as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The receipt filename this nonce is answered by.
    #[must_use]
    pub fn ack_file_name(&self) -> String {
        format!("ack-{}.json", self.0)
    }
}

/// What a dispatch learned about its own submission.
///
/// # Why three variants and not a bool
///
/// A composer reading and an event acknowledgement answer the same question
/// with very different confidence, and every earlier version of this code path
/// had only the weaker one — so there was no type to be honest with. Here there
/// is: a caller needing the strong claim matches [`EventAck`](Self::EventAck), a
/// caller needing "probably submitted" accepts either, and neither can confuse
/// them by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitEvidence {
    /// Claude Code signed a receipt naming this dispatch's nonce.
    ///
    /// # What this does *not* prove
    ///
    /// That the model began processing the briefing. Measured, 3/3: with a
    /// second `UserPromptSubmit` hook exiting 2, Claude Code **rejected** the
    /// prompt — surfacing the blocking hook's reason in the pane — and the
    /// receipt was written anyway. A receipt proves the prompt entered the
    /// `UserPromptSubmit` lifecycle and nothing beyond it.
    ///
    /// "Is the worker working?" is still answered where it always was: by the
    /// readiness sensor's `Working` / `⏺` observation. Nothing in this module
    /// may be wired into that question.
    EventAck,
    /// The pane was read and the briefing is no longer in the composer. The
    /// inference from pixels — weaker than a receipt, and more available.
    ComposerCleared,
    /// Neither signal landed. The submit is *not known* to have happened.
    Unobserved,
}

impl SubmitEvidence {
    /// Whether this reading is positive evidence that the briefing was
    /// submitted.
    ///
    /// Deliberately the only predicate on this type. There is no
    /// `is_being_worked_on`, because no variant here can answer that — see
    /// [`EventAck`](Self::EventAck).
    #[must_use]
    pub fn submitted(self) -> bool {
        matches!(self, Self::EventAck | Self::ComposerCleared)
    }
}

/// Why a dispatch fell back from a receipt to a weaker signal.
///
/// Recorded on every demotion. The typed reason is what let 8/8 broken-hook
/// trials be told apart from a healthy dispatch that simply lost the race.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    /// No receipt inside the deadline; the composer was read and had cleared.
    AckAbsentComposerCleared,
    /// No receipt inside the deadline; the briefing is still in the composer.
    AckAbsentComposerPending,
    /// No receipt inside the deadline and the pane could not be read at all.
    AckAbsentComposerUnobservable,
    /// No receipt was ever possible: the station could not be prepared, so the
    /// hook had nowhere to write. Distinct from the three above because it is
    /// cosmon's own fault rather than a statement about the worker.
    ReceiptStationUnavailable,
}

impl FallbackReason {
    /// The stable string form, for logs and events.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AckAbsentComposerCleared => "ack_absent_composer_cleared",
            Self::AckAbsentComposerPending => "ack_absent_composer_pending",
            Self::AckAbsentComposerUnobservable => "ack_absent_composer_unobservable",
            Self::ReceiptStationUnavailable => "receipt_station_unavailable",
        }
    }
}

/// The full outcome of one submit attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitOutcome {
    /// What was established, and how.
    pub evidence: SubmitEvidence,
    /// Milliseconds from the first submit press to the evidence.
    pub latency_ms: u64,
    /// Carriage returns this loop sent, including the caller's first press.
    pub submits_sent: u32,
    /// Carriage returns sent *after* the evidence landed. Must be 0: the
    /// duplicate presses into an already-submitted composer are the failure this
    /// mechanism exists to avoid, so a version that reproduced them would be no
    /// better than the loop it augments.
    pub submits_after_evidence: u32,
    /// Why the outcome is not [`SubmitEvidence::EventAck`], when it is not.
    pub fallback_reason: Option<FallbackReason>,
}

impl SubmitOutcome {
    /// The stable evidence string, for logs and events.
    #[must_use]
    pub fn evidence_str(&self) -> &'static str {
        match self.evidence {
            SubmitEvidence::EventAck => "event_ack",
            SubmitEvidence::ComposerCleared => "composer_cleared",
            SubmitEvidence::Unobserved => "unobserved",
        }
    }
}

/// Root under which per-worker receipt directories live.
///
/// `$COSMON_RECEIPT_ROOT` when set, else `<temp>/cosmon-briefing-receipts`. Both
/// ends of the mechanism — the spawn that writes the settings overlay and the
/// send path that waits on it — resolve the directory from the worker id alone,
/// so no state has to travel between the two processes.
#[must_use]
pub fn receipt_root() -> PathBuf {
    std::env::var_os(ENV_RECEIPT_ROOT).map_or_else(
        || std::env::temp_dir().join("cosmon-briefing-receipts"),
        PathBuf::from,
    )
}

/// One worker's receipt directory, plus the nonce file that keys it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptStation {
    dir: PathBuf,
}

impl ReceiptStation {
    /// The station for `worker`, under `root`.
    ///
    /// The worker name is sanitized with the same filename rule as the nonce:
    /// a [`WorkerId`] is derived from operator-supplied text and this value
    /// becomes a path component.
    #[must_use]
    pub fn for_worker(root: &Path, worker: &WorkerId) -> Self {
        let name = ReceiptNonce::sanitize(worker.name())
            .map_or_else(|| "worker".to_owned(), |n| n.as_str().to_owned());
        Self {
            dir: root.join(name),
        }
    }

    /// The station rooted at an explicit directory (tests, and the hook side,
    /// which is handed the path rather than deriving it).
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory receipts land in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file the hook reads the current nonce from.
    #[must_use]
    pub fn nonce_file(&self) -> PathBuf {
        self.dir.join("nonce")
    }

    /// Create the directory, 0700.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] when the directory cannot be
    /// created. Callers treat that as [`FallbackReason::ReceiptStationUnavailable`]
    /// rather than as a dispatch failure — a missing receipt must never stop a
    /// briefing being sent.
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }

    /// Publish the nonce for the dispatch about to happen, atomically.
    ///
    /// Written-then-renamed for the same reason the receipt is: the hook may
    /// read this file at any instant, including the one we are rewriting it in,
    /// and a half-written nonce keys the receipt to nothing.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] if the nonce cannot be written
    /// or renamed into place.
    pub fn stamp(&self, nonce: &ReceiptNonce) -> std::io::Result<()> {
        let tmp = self.dir.join(".nonce-tmp");
        {
            let mut fh = std::fs::File::create(&tmp)?;
            fh.write_all(nonce.as_str().as_bytes())?;
            fh.write_all(b"\n")?;
            fh.sync_all()?;
        }
        std::fs::rename(&tmp, self.nonce_file())
    }

    /// The receipt for `nonce`, if the hook has written one.
    ///
    /// Never raises on a hostile or missing directory: a receipt that cannot be
    /// read is simply absent, which the caller already has a fallback for.
    #[must_use]
    pub fn read_ack(&self, nonce: &ReceiptNonce) -> Option<AckRecord> {
        let raw = std::fs::read_to_string(self.dir.join(nonce.ack_file_name())).ok()?;
        let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
        Some(AckRecord {
            nonce: nonce.clone(),
            session_id: value
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned),
        })
    }

    /// Delete this dispatch's receipt.
    ///
    /// One file per dispatch, and a worker takes hundreds. The prototype left
    /// every one of them on disk; this is the half that makes the directory
    /// bounded in the normal case, with [`prune`](Self::prune) covering the
    /// dispatches that never came back for theirs.
    pub fn consume(&self, nonce: &ReceiptNonce) {
        let _ = std::fs::remove_file(self.dir.join(nonce.ack_file_name()));
    }

    /// Delete receipts older than `max_age`.
    ///
    /// Covers the leftovers `consume` cannot reach: a receipt for a dispatch
    /// that timed out, and the `ack-nokey.json` written when a prompt is
    /// submitted with no nonce stamped (an operator typing into the pane).
    /// Touches only `ack-*.json` — never the nonce file, and never anything a
    /// different mechanism put here.
    pub fn prune(&self, max_age: Duration) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let now = std::time::SystemTime::now();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // Case-sensitive on purpose, so clippy's suggestion to fold case
            // is declined: this directory holds files *this* code wrote, whose
            // name is `ack-<nonce>.json` exactly. A `ack-x.JSON` is something
            // else, and pruning is a delete — the narrow match is the point.
            #[allow(clippy::case_sensitive_file_extension_comparisons)]
            let ours = name.starts_with("ack-") && name.ends_with(".json");
            if !ours {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age > max_age);
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// What a receipt carried.
///
/// Deliberately thin. The hook reads `session_id` and `hook_event_name` from the
/// payload and copies nothing else: `prompt`, `cwd` and `transcript_path` are
/// briefing content and operator paths, and a receipt directory is not where
/// either belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckRecord {
    /// The nonce this receipt answers.
    pub nonce: ReceiptNonce,
    /// Claude Code's session id, when the payload carried one.
    pub session_id: Option<String>,
}

/// The shell command Claude Code runs on `UserPromptSubmit`.
///
/// # Why a `cs` subcommand by absolute path
///
/// Measured (RESULTS §"The hook implementation costs more than the mechanism"):
/// `/usr/bin/env python3 ack_hook.py` cost 368 ms median and 1068 ms at the max,
/// almost all of it a pyenv shim plus interpreter startup before a single line
/// of the hook ran. A compiled subcommand of the binary that is already built
/// pays neither, and — the part that matters more than the milliseconds — it
/// cannot be diverted by whatever version manager happens to own `python3` on
/// the host.
///
/// Environment is set inline rather than inherited: the hook's contract must not
/// depend on what the pane happened to export. Every value is shell-quoted, and
/// that is not decoration — an unquoted value containing a space silently
/// truncates the assignment and turns the rest into a command, which is how the
/// experiment spent three trials measuring a hook that had never run.
#[must_use]
pub fn hook_command(cs_bin: &Path, station: &ReceiptStation) -> String {
    let dir = shell_quote(&station.dir().to_string_lossy());
    let nonce_file = shell_quote(&station.nonce_file().to_string_lossy());
    let bin = shell_quote(&cs_bin.to_string_lossy());
    // `>/dev/null` is the *second* layer, not the guard. The binary replaces
    // file descriptor 1 before its first other statement (see
    // `cosmon_cli::briefing_receipt_hook`), because a hook's stdout is fed to
    // the model as context and a stray line becomes an unattributed instruction
    // in every dispatched briefing. This redirect covers the case the binary
    // cannot: an exec that fails, and whatever the shell says about it.
    format!(
        "{ENV_RECEIPT_DIR}={dir} {ENV_RECEIPT_NONCE_FILE}={nonce_file} \
         {bin} {HOOK_SUBCOMMAND} >/dev/null"
    )
}

/// Write a settings overlay registering the receipt hook on `UserPromptSubmit`.
///
/// The file is a **new** 0600 file in a directory cosmon owns; it never reads,
/// merges into, or rewrites the user, project, local or managed settings.
/// `claude --settings <file>` is additive and file-scoped, which is what makes
/// it safe to hand a worker a hook without touching anything an operator
/// configured. An operator hook already registered on the same event keeps
/// working — the experiment forced that case and got its receipt regardless.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the overlay cannot be written.
pub fn write_settings_overlay(
    path: &Path,
    cs_bin: &Path,
    station: &ReceiptStation,
) -> std::io::Result<()> {
    let doc = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": [{
                "hooks": [{
                    "type": "command",
                    "command": hook_command(cs_bin, station),
                    "timeout": 5,
                }]
            }]
        }
    });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let rendered = serde_json::to_string_pretty(&doc)?;
    std::fs::write(path, rendered)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// The hook's whole body: read the stamped nonce, write the receipt.
///
/// Split out of the CLI subcommand so the guards below are unit-testable without
/// spawning a process. The subcommand's own job is the one thing that cannot be
/// tested from in-process: muting file descriptor 1 before any other statement.
///
/// Four properties, each of which has a test that goes red without it:
///
/// - **the nonce is sanitized** before it is used as a filename, so a nonce file
///   holding `../../escape` writes inside the receipt directory and nowhere else;
/// - **an unkeyed submission is keyed `nokey`**, not dropped and not written as
///   `ack-.json` — an operator typing into the pane must be distinguishable from
///   a dispatch, not invisible;
/// - **the receipt is written temp-then-renamed**, so a poller can never read a
///   half-written file and call it a receipt;
/// - **nothing from the payload but `session_id` is copied.** The prompt is the
///   briefing.
///
/// Returns `false` when no receipt could be written. The caller ignores it: a
/// receipt hook must never be able to block a prompt, so every path exits 0.
#[must_use]
pub fn record_hook_ack(station: &ReceiptStation, payload: &str) -> bool {
    let nonce = std::fs::read_to_string(station.nonce_file())
        .ok()
        .and_then(|raw| ReceiptNonce::sanitize(raw.trim()))
        .unwrap_or_else(|| ReceiptNonce("nokey".to_owned()));

    let session_id = serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| {
            v.get("session_id")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        });

    let doc = serde_json::json!({
        "nonce": nonce.as_str(),
        "event": "UserPromptSubmit",
        "session_id": session_id,
    });
    let Ok(rendered) = serde_json::to_string(&doc) else {
        return false;
    };

    let tmp = station
        .dir()
        .join(format!(".ack-tmp-{}", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut fh = std::fs::File::create(&tmp)?;
        fh.write_all(rendered.as_bytes())?;
        fh.sync_all()?;
        std::fs::rename(&tmp, station.dir().join(nonce.ack_file_name()))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

/// The world [`await_submit_evidence`] drives, injected so the loop is testable
/// without a live tmux server or Claude TUI.
pub trait SubmitEnv {
    /// Send one carriage return to the worker's composer.
    fn press_submit(&mut self);
    /// Read the composer: `Some(true)` still holds our paste, `Some(false)`
    /// cleared, `None` could not be read. The third case is not a `false` —
    /// see [`ComposerState::Unobservable`](crate::tmux::ComposerState).
    fn composer_pending(&mut self) -> Option<bool>;
    /// Is this dispatch's receipt on disk yet?
    fn ack_present(&mut self) -> bool;
    /// Milliseconds since the first submit press.
    fn elapsed_ms(&mut self) -> u64;
    /// Wait.
    fn sleep(&mut self, d: Duration);
}

/// How long to wait, and how often to press.
#[derive(Debug, Clone, Copy)]
pub struct AwaitConfig {
    /// Ceiling on the whole wait. See [`ACK_DEADLINE_MS`].
    pub deadline_ms: u64,
    /// Minimum gap between two carriage returns.
    pub retry_interval_ms: u64,
    /// Gap between receipt polls. See [`ACK_POLL_INTERVAL_MS`].
    pub poll_interval_ms: u64,
    /// Carriage returns available to this dispatch, over and above the caller's
    /// first press. The existing auto-scaled submit budget.
    pub press_budget: u32,
}

impl Default for AwaitConfig {
    fn default() -> Self {
        Self {
            deadline_ms: ACK_DEADLINE_MS,
            retry_interval_ms: 300,
            poll_interval_ms: ACK_POLL_INTERVAL_MS,
            press_budget: 5,
        }
    }
}

/// Wait for the strongest evidence available that the paste was submitted.
///
/// The caller has already pasted and pressed submit once. This loop then:
///
/// - polls the receipt every [`AwaitConfig::poll_interval_ms`], and returns
///   [`SubmitEvidence::EventAck`] the instant one exists — the early exit the
///   whole mechanism is for, worth 3–4× on an idle pane;
/// - **consults the composer before every single re-press**, and returns
///   [`SubmitEvidence::ComposerCleared`] when it reads clear.
///
/// # Why the composer check is not optional tuning
///
/// Measured. On a pane that is already mid-response Claude Code *queues* the
/// pasted message: the composer empties within a second — the pane says `Press
/// up to edit queued messages` — while `UserPromptSubmit` does not fire until
/// the queue drains, five to six seconds later. A loop that presses until the
/// receipt arrives keeps pressing into an empty composer for that whole gap:
/// **109 carriage returns across 5 trials, median 23 per dispatch**, against 1
/// for the loop that looks first. Without this check the receipt would *cause*
/// the duplicate-carriage-return problem it was proposed to remove.
///
/// # Why the composer is re-read every cycle rather than latched
///
/// The prototype's first version set a flag on the first cleared reading and
/// never pressed again. One `capture-pane` that caught the composer mid-redraw
/// then disarmed the retry for the rest of the dispatch, and a trial sent a
/// single carriage return and still had the paste sitting in the composer eight
/// seconds later. A signal that says "stop" must be re-checked, not remembered —
/// which is why the composer reading below is a local, consumed once.
pub fn await_submit_evidence<E: SubmitEnv>(env: &mut E, cfg: &AwaitConfig) -> SubmitOutcome {
    let mut submits: u32 = 1; // the caller's first press
    let mut presses_left = cfg.press_budget;
    let mut last_press_ms: u64 = 0;
    let mut last_composer: Option<bool> = None;

    loop {
        if env.ack_present() {
            return SubmitOutcome {
                evidence: SubmitEvidence::EventAck,
                latency_ms: env.elapsed_ms(),
                submits_sent: submits,
                submits_after_evidence: 0,
                fallback_reason: None,
            };
        }

        let now = env.elapsed_ms();
        if now >= cfg.deadline_ms {
            break;
        }

        if now.saturating_sub(last_press_ms) >= cfg.retry_interval_ms {
            // Look before pressing. Every cycle, never latched.
            let pending = env.composer_pending();
            last_composer = pending;
            match pending {
                Some(false) => {
                    // The composer is empty: what we pasted is submitted or
                    // queued, and another carriage return can only land
                    // somewhere it was not meant to. This is the answer, and it
                    // is honest about being the weaker one.
                    return SubmitOutcome {
                        evidence: SubmitEvidence::ComposerCleared,
                        latency_ms: env.elapsed_ms(),
                        submits_sent: submits,
                        submits_after_evidence: 0,
                        fallback_reason: Some(FallbackReason::AckAbsentComposerCleared),
                    };
                }
                Some(true) => {
                    if presses_left == 0 {
                        break;
                    }
                    env.press_submit();
                    submits = submits.saturating_add(1);
                    presses_left -= 1;
                }
                // Could not read the pane. Evidence of nothing, and in
                // particular not a licence to press: an unreadable pane must
                // not manufacture a nudge. Keep waiting for the receipt, which
                // is the one signal a failed capture cannot corrupt.
                None => {}
            }
            last_press_ms = env.elapsed_ms();
        }

        env.sleep(Duration::from_millis(cfg.poll_interval_ms));
    }

    // Neither signal landed inside the deadline. Report which of the three
    // failure modes this was; never relabel it as a submission.
    let reason = match last_composer {
        Some(false) => FallbackReason::AckAbsentComposerCleared,
        Some(true) => FallbackReason::AckAbsentComposerPending,
        None => FallbackReason::AckAbsentComposerUnobservable,
    };
    SubmitOutcome {
        evidence: SubmitEvidence::Unobserved,
        latency_ms: env.elapsed_ms(),
        submits_sent: submits,
        submits_after_evidence: 0,
        fallback_reason: Some(reason),
    }
}

/// Shell-quote a value destined for a `VAR=value cmd` prefix.
///
/// Local rather than shared with [`crate::tmux`]'s copy because this one is
/// load-bearing for a *settings file* read by a different process: the quoting
/// bug it prevents (a space truncating the assignment) is exactly the one the
/// experiment hit, and it must not become someone else's refactor.
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_owned();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted world: the test says what the composer and the receipt do at
    /// each millisecond, and the loop is judged on what it pressed.
    struct Script {
        now_ms: u64,
        /// `(at_ms, pending)` — the composer's answer from `at_ms` onward.
        composer: Vec<(u64, Option<bool>)>,
        /// The receipt appears at this elapsed time. `None` = never.
        ack_at_ms: Option<u64>,
        presses: Vec<u64>,
    }

    impl Script {
        fn new(composer: Vec<(u64, Option<bool>)>, ack_at_ms: Option<u64>) -> Self {
            Self {
                now_ms: 0,
                composer,
                ack_at_ms,
                presses: Vec::new(),
            }
        }
    }

    impl SubmitEnv for Script {
        fn press_submit(&mut self) {
            self.presses.push(self.now_ms);
        }
        fn composer_pending(&mut self) -> Option<bool> {
            let mut answer = Some(true);
            for (at, value) in &self.composer {
                if self.now_ms >= *at {
                    answer = *value;
                }
            }
            answer
        }
        fn ack_present(&mut self) -> bool {
            self.ack_at_ms.is_some_and(|at| self.now_ms >= at)
        }
        fn elapsed_ms(&mut self) -> u64 {
            self.now_ms
        }
        fn sleep(&mut self, d: Duration) {
            self.now_ms += u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        }
    }

    fn cfg() -> AwaitConfig {
        AwaitConfig {
            deadline_ms: 12_000,
            retry_interval_ms: 300,
            poll_interval_ms: 50,
            press_budget: 5,
        }
    }

    /// The idle case the mechanism is for: the receipt beats the composer, and
    /// the loop returns on it without a second carriage return.
    #[test]
    fn a_receipt_arriving_first_is_the_answer_and_costs_no_extra_press() {
        // Composer still shows the paste at 300 ms (the TUI has not redrawn);
        // the receipt lands at 250 ms.
        let mut script = Script::new(vec![(0, Some(true))], Some(250));
        let out = await_submit_evidence(&mut script, &cfg());
        assert_eq!(out.evidence, SubmitEvidence::EventAck);
        assert_eq!(out.fallback_reason, None);
        assert_eq!(out.submits_sent, 1, "no re-press before the receipt landed");
        assert_eq!(out.submits_after_evidence, 0);
        assert!(script.presses.is_empty());
    }

    /// **The guard from Table 3.** A busy pane queues the paste: the composer
    /// empties at ~900 ms while the receipt does not arrive for 6 s. Without the
    /// composer check before each re-press this loop sends ~20 carriage returns
    /// into an empty composer. Remove the `Some(false)` arm and this test goes
    /// red on the press count.
    #[test]
    fn a_queued_briefing_does_not_get_hammered_while_the_receipt_is_pending() {
        let mut script = Script::new(vec![(0, Some(true)), (900, Some(false))], Some(6_000));
        let out = await_submit_evidence(&mut script, &cfg());
        assert_eq!(
            out.evidence,
            SubmitEvidence::ComposerCleared,
            "the composer clearing is the signal that arrives first on a busy pane"
        );
        assert_eq!(
            out.fallback_reason,
            Some(FallbackReason::AckAbsentComposerCleared)
        );
        assert!(
            script.presses.len() <= 3,
            "a queued briefing must not be hammered: {} presses at {:?}",
            script.presses.len(),
            script.presses
        );
    }

    /// The retry the receipt rescues: the first carriage return was swallowed,
    /// so the composer keeps showing the paste until a re-press lands it.
    #[test]
    fn a_swallowed_first_carriage_return_is_recovered_by_the_retry() {
        let mut script = Script::new(vec![(0, Some(true)), (1_000, Some(false))], Some(1_100));
        let out = await_submit_evidence(&mut script, &cfg());
        assert!(out.evidence.submitted());
        assert!(
            out.submits_sent > 1,
            "the loop must have pressed again while the composer still held the paste"
        );
    }

    /// An unreadable pane is not a licence to press. Turning `None` into a
    /// press — the pre-typed-state bug — makes this red.
    #[test]
    fn an_unreadable_composer_never_manufactures_a_nudge() {
        let mut script = Script::new(vec![(0, None)], None);
        let out = await_submit_evidence(&mut script, &cfg());
        assert_eq!(out.evidence, SubmitEvidence::Unobserved);
        assert_eq!(
            out.fallback_reason,
            Some(FallbackReason::AckAbsentComposerUnobservable)
        );
        assert_eq!(out.submits_sent, 1, "only the caller's own first press");
        assert!(script.presses.is_empty());
    }

    /// The deadline is what bounds a pane that never answers, and it is
    /// generous on purpose (a busy receipt arrives 5–6 s in, one at 8.1 s).
    #[test]
    fn the_deadline_is_generous_enough_for_a_queued_prompt() {
        assert!(
            ACK_DEADLINE_MS >= 8_000,
            "8 s was already marginal in the measurement: one busy trial in five \
             drained at 8.1 s and was demoted by the deadline rather than by \
             anything going wrong"
        );
    }

    /// A never-clearing composer spends the press budget and stops, rather than
    /// pressing for the whole deadline.
    #[test]
    fn a_never_clearing_composer_spends_the_budget_and_no_more() {
        let mut script = Script::new(vec![(0, Some(true))], None);
        let out = await_submit_evidence(
            &mut script,
            &AwaitConfig {
                press_budget: 4,
                ..cfg()
            },
        );
        assert_eq!(out.evidence, SubmitEvidence::Unobserved);
        assert_eq!(
            out.fallback_reason,
            Some(FallbackReason::AckAbsentComposerPending)
        );
        assert_eq!(
            out.submits_sent, 5,
            "one caller press plus a budget of four"
        );
    }

    #[test]
    fn a_hostile_nonce_cannot_escape_the_receipt_directory() {
        let nonce = ReceiptNonce::sanitize("../../../../tmp/escaped").expect("survives filtering");
        assert_eq!(nonce.as_str(), "tmpescaped");
        assert!(!nonce.ack_file_name().contains('/'));
    }

    #[test]
    fn a_nonce_that_is_entirely_unsafe_is_refused_rather_than_emptied() {
        assert_eq!(ReceiptNonce::sanitize("../../"), None);
        assert_eq!(ReceiptNonce::sanitize(""), None);
    }

    #[test]
    fn a_long_nonce_is_truncated_to_a_usable_filename() {
        let nonce = ReceiptNonce::sanitize(&"a".repeat(500)).expect("survives");
        assert_eq!(nonce.as_str().len(), 64);
    }

    #[test]
    fn minted_nonces_do_not_repeat() {
        let a = ReceiptNonce::mint();
        let b = ReceiptNonce::mint();
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), 16);
    }

    #[test]
    fn event_ack_and_composer_cleared_are_both_submitted_but_stay_distinct() {
        assert!(SubmitEvidence::EventAck.submitted());
        assert!(SubmitEvidence::ComposerCleared.submitted());
        assert!(!SubmitEvidence::Unobserved.submitted());
        assert_ne!(SubmitEvidence::EventAck, SubmitEvidence::ComposerCleared);
    }

    #[test]
    fn the_hook_writes_a_receipt_keyed_to_the_stamped_nonce() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let station = ReceiptStation::at(tmp.path());
        station.ensure().expect("ensure");
        let nonce = ReceiptNonce::mint();
        station.stamp(&nonce).expect("stamp");

        assert!(record_hook_ack(
            &station,
            r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit"}"#
        ));

        let ack = station.read_ack(&nonce).expect("receipt");
        assert_eq!(ack.nonce, nonce);
        assert_eq!(ack.session_id.as_deref(), Some("s-1"));
    }

    /// The confidentiality property: a receipt directory is not where briefing
    /// content lives. Copying the payload's `prompt` through makes this red.
    #[test]
    fn the_hook_never_copies_the_prompt_into_the_receipt() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let station = ReceiptStation::at(tmp.path());
        station.ensure().expect("ensure");
        let nonce = ReceiptNonce::mint();
        station.stamp(&nonce).expect("stamp");

        let secret = "PLEASE-DO-NOT-PERSIST-ME";
        assert!(record_hook_ack(
            &station,
            &format!(
                r#"{{"session_id":"s","prompt":"{secret}","cwd":"/home/op/secret",
                     "transcript_path":"/home/op/.claude/x.jsonl"}}"#
            )
        ));

        for entry in std::fs::read_dir(tmp.path()).expect("read_dir").flatten() {
            let body = std::fs::read_to_string(entry.path()).unwrap_or_default();
            assert!(
                !body.contains(secret),
                "prompt leaked into {:?}",
                entry.path()
            );
            assert!(
                !body.contains("/home/op"),
                "path leaked into {:?}",
                entry.path()
            );
        }
    }

    /// A prompt submitted with no nonce stamped — an operator typing into the
    /// pane — is keyed `nokey`, so it can never be mistaken for a dispatch's
    /// receipt.
    #[test]
    fn an_unkeyed_submission_is_keyed_nokey_and_answers_no_dispatch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let station = ReceiptStation::at(tmp.path());
        station.ensure().expect("ensure");

        assert!(record_hook_ack(&station, "{}"));
        assert!(tmp.path().join("ack-nokey.json").exists());

        let dispatch = ReceiptNonce::mint();
        assert!(
            station.read_ack(&dispatch).is_none(),
            "an unkeyed submission must not answer a dispatch's nonce"
        );
    }

    /// Every path exits without raising, including the ones a hostile or broken
    /// environment produces. A receipt hook must never be able to block a
    /// prompt.
    #[test]
    fn the_hook_survives_a_broken_environment_without_raising() {
        let station = ReceiptStation::at("/nonexistent/cosmon-receipts");
        assert!(!record_hook_ack(&station, "{}"));
        assert!(!record_hook_ack(&station, "not json at all"));
        assert!(!record_hook_ack(&station, ""));

        let tmp = tempfile::tempdir().expect("tempdir");
        let station = ReceiptStation::at(tmp.path());
        station.ensure().expect("ensure");
        // Non-object payloads, and a nonce file that is a directory.
        std::fs::create_dir(station.nonce_file()).expect("mkdir nonce");
        assert!(record_hook_ack(&station, "[1,2,3]"));
        assert!(tmp.path().join("ack-nokey.json").exists());
    }

    #[test]
    fn a_receipt_leaves_no_temporary_residue() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let station = ReceiptStation::at(tmp.path());
        station.ensure().expect("ensure");
        let nonce = ReceiptNonce::mint();
        station.stamp(&nonce).expect("stamp");
        assert!(record_hook_ack(&station, "{}"));

        for entry in std::fs::read_dir(tmp.path()).expect("read_dir").flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            assert!(
                !name.starts_with(".ack-tmp-") && !name.starts_with(".nonce-tmp"),
                "temporary residue left behind: {name}"
            );
        }
    }

    /// One file per dispatch, and a worker takes hundreds of them.
    #[test]
    fn consuming_a_receipt_removes_its_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let station = ReceiptStation::at(tmp.path());
        station.ensure().expect("ensure");
        let nonce = ReceiptNonce::mint();
        station.stamp(&nonce).expect("stamp");
        assert!(record_hook_ack(&station, "{}"));
        assert!(station.read_ack(&nonce).is_some());

        station.consume(&nonce);
        assert!(station.read_ack(&nonce).is_none());
        assert!(
            station.nonce_file().exists(),
            "consuming a receipt must not remove the nonce file"
        );
    }

    /// The half `consume` cannot reach: receipts for dispatches that timed out,
    /// and the `ack-nokey.json` an operator's own prompt leaves behind.
    #[test]
    fn pruning_removes_stale_receipts_and_nothing_else() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let station = ReceiptStation::at(tmp.path());
        station.ensure().expect("ensure");
        let nonce = ReceiptNonce::mint();
        station.stamp(&nonce).expect("stamp");
        assert!(record_hook_ack(&station, "{}"));
        std::fs::write(tmp.path().join("not-a-receipt.txt"), "keep me").expect("write");

        // Nothing is stale yet.
        station.prune(Duration::from_secs(300));
        assert!(station.read_ack(&nonce).is_some());

        // Everything is stale now.
        station.prune(Duration::from_secs(0));
        assert!(station.read_ack(&nonce).is_none());
        assert!(
            station.nonce_file().exists(),
            "pruning must never remove the nonce file"
        );
        assert!(
            tmp.path().join("not-a-receipt.txt").exists(),
            "pruning must touch only ack-*.json"
        );
    }

    /// The hook command must name the compiled binary by absolute path, and
    /// must quote every value it exports. An unquoted path with a space in it
    /// silently truncates the assignment and turns the rest into a command —
    /// which is how the experiment spent three trials measuring a hook that had
    /// never run.
    #[test]
    fn the_hook_command_is_the_compiled_binary_with_quoted_values() {
        let station = ReceiptStation::at("/var/folders/a b/receipts");
        let cmd = hook_command(Path::new("/usr/local/bin/cs"), &station);
        assert!(
            cmd.contains("/usr/local/bin/cs briefing-receipt-hook"),
            "{cmd}"
        );
        assert!(
            cmd.contains("COSMON_RECEIPT_DIR='/var/folders/a b/receipts'"),
            "a path with a space must be quoted: {cmd}"
        );
        assert!(
            !cmd.contains("python") && !cmd.contains("/usr/bin/env"),
            "the hook must not go through an interpreter or a shim: {cmd}"
        );
    }

    #[test]
    fn the_overlay_registers_exactly_one_user_prompt_submit_hook() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let station = ReceiptStation::at(tmp.path().join("receipts"));
        write_settings_overlay(&path, Path::new("/usr/local/bin/cs"), &station).expect("overlay");

        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        let hooks = doc["hooks"]["UserPromptSubmit"]
            .as_array()
            .expect("UserPromptSubmit array");
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0]["hooks"].as_array().expect("inner").len(), 1);
        assert!(doc["hooks"].get("PreToolUse").is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the overlay must be 0600");
        }
    }

    /// The overlay is a new file cosmon owns. It never reads, merges into, or
    /// rewrites anything an operator configured — `claude --settings` is
    /// additive and file-scoped, which is what makes that safe.
    #[test]
    fn writing_the_overlay_touches_no_operator_settings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let operator = tmp.path().join("settings.json");
        std::fs::write(&operator, r#"{"hooks":{"UserPromptSubmit":[]}}"#).expect("write");
        let before = std::fs::read_to_string(&operator).expect("read");

        let overlay = tmp.path().join("overlay").join("settings.json");
        let station = ReceiptStation::at(tmp.path().join("receipts"));
        write_settings_overlay(&overlay, Path::new("/usr/local/bin/cs"), &station).expect("write");

        assert_eq!(std::fs::read_to_string(&operator).expect("read"), before);
    }

    #[test]
    fn a_station_is_derived_from_the_worker_alone() {
        let worker = WorkerId::new("task-20260801-8620").expect("worker id");
        let a = ReceiptStation::for_worker(Path::new("/tmp/r"), &worker);
        let b = ReceiptStation::for_worker(Path::new("/tmp/r"), &worker);
        assert_eq!(a, b, "both ends must resolve the same directory");
        assert!(a.dir().ends_with("task-20260801-8620"));
    }

    /// A [`WorkerId`] is derived from operator-supplied text and becomes a path
    /// component here. The sanitizer is what keeps it one component; deleting it
    /// makes a name carrying path syntax reach outside the root.
    #[test]
    fn a_worker_name_carrying_path_syntax_cannot_escape_the_root() {
        let station = ReceiptStation {
            dir: Path::new("/tmp/r").join(
                ReceiptNonce::sanitize("../../etc")
                    .map_or_else(|| "worker".to_owned(), |n| n.as_str().to_owned()),
            ),
        };
        assert_eq!(station.dir(), Path::new("/tmp/r/etc"));
    }

    #[test]
    fn fallback_reasons_have_stable_strings() {
        assert_eq!(
            FallbackReason::AckAbsentComposerCleared.as_str(),
            "ack_absent_composer_cleared"
        );
        assert_eq!(
            FallbackReason::ReceiptStationUnavailable.as_str(),
            "receipt_station_unavailable"
        );
    }
}
