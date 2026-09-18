// SPDX-License-Identifier: AGPL-3.0-only

//! `cs opt-in-share` — first-run consent prompt for developer-share telemetry.
//!
//! A fresh user should see, on their very first `cs` invocation, a small
//! prompt asking whether they agree to share encrypted bundles with the
//! cosmon developers. The answer — accept or decline — is persisted once to
//! `~/.config/cosmon/consent.toml` and never asked again.
//!
//! # Why the strings are English
//!
//! Every other string `cs` prints is English: `--help`, every error, every
//! command's rendering. This path was the exception — the prompt and its
//! three outcome lines were French, a leftover from the operator-b
//! onboarding brief they were first written for. A newcomer walking the
//! published install route on an English substrate met French for the first
//! and only time here, at the one moment they are asked to make a decision
//! about their own data (noogram/cosmon#76). A consent question the reader
//! cannot read is not consent, so the consent path now speaks the language of
//! the surface it lives on. [`parse_yes_no`] still accepts `o`/`oui`, because
//! taking an answer someone already learned to give costs nothing.
//!
//! Design constraints (from the MVP operator-b brief, delib fe35 §c):
//!
//! * **Deny by default.** If the consent file is missing, no share occurs.
//! * **Explicit trace either way.** Accept writes `accepted_at`; decline
//!   writes `declined_at`. The file's presence + one of those two keys is the
//!   durable proof of the operator's choice.
//! * **Non-interactive = decline.** When the question cannot be *answered*
//!   (see [`can_ask_interactively`]), we skip the prompt and store
//!   `declined_at` without asking. This keeps every unattended run safe.
//! * **No trace in the user's project.** Consent lives under
//!   `~/.config/cosmon/`, never inside the project's `.cosmon/` directory,
//!   so sharing toggles don't leak into `git log`.
//!
//! # Where this question may be asked
//!
//! Not on the dispatch path. `cs tackle` used to call [`ensure_consent`] as
//! its first act; that placement is the one place a blocking question is
//! guaranteed to be unanswerable, because dispatch is exactly what an
//! orchestrator wraps in `OUT="$(cs tackle …)"`. The question then prints
//! into a variable nobody reads while stdin is still the inherited terminal
//! — a mute hang, structurally the same failure as the four container doors
//! of noogram/cosmon#20, except this one was ours. The hook now lives on
//! `cs init` (the explicit, once-per-galaxy interactive moment) and on this
//! command invoked alone. See ADR-163 and architectural invariant §8w.
//!
//! The age recipient is read from `~/.config/cosmon/default-recipient.age`
//! (shipped by the cosmon installer). Its value is embedded in the consent
//! record so a future audit can detect silent key rotations.

use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::Context;

/// Current consent-record schema version. Bumped when the shape changes so
/// `cs opt-in-share --status` can reason about older files without panicking.
pub const CONSENT_VERSION: u32 = 1;

/// Relative path from the config base dir to the consent file.
pub const CONSENT_FILE: &str = "cosmon/consent.toml";

/// Relative path from the config base dir to the shipped age recipient.
pub const RECIPIENT_FILE: &str = "cosmon/default-recipient.age";

/// Prompt text shown on stdout at first run, in the CLI's own language. The
/// wording is deliberate: it names the encryption, the sole recipient, and
/// the no-trace-in-commits guarantee, then asks a single yes/no question with
/// a deny-by-default marker (`[y/N]` — the capital is the default).
pub const PROMPT: &str = "\
Share diagnostic information with the cosmon developers?
Bundles are age-encrypted; only the Noogram maintainer can read them.
Changes to your project: no trace of cosmon in your commits. [y/N]";

/// Arguments for the `opt-in-share` subcommand.
#[derive(clap::Args, Default)]
pub struct Args {
    /// Print the current consent state (accepted / declined / none) and exit.
    #[arg(long)]
    pub status: bool,

    /// Bypass the TTY prompt and persist a declined record (non-interactive).
    #[arg(long, conflicts_with_all = ["accept", "status"])]
    pub decline: bool,

    /// Bypass the TTY prompt and persist an accepted record (non-interactive).
    #[arg(long, conflicts_with_all = ["decline", "status"])]
    pub accept: bool,
}

/// Persisted consent record. Either `accepted_at` or `declined_at` is Some;
/// never both. The `recipient_age_pubkey` field captures the age recipient
/// the user consented to at the time of the answer — rotating the key later
/// SHOULD re-trigger the prompt (a later enhancement).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsentRecord {
    /// Schema version. Always [`CONSENT_VERSION`] when freshly written.
    pub version: u32,
    /// Timestamp of an explicit accept, if the user opted in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_at: Option<DateTime<Utc>>,
    /// Timestamp of an explicit decline (or implicit non-interactive skip).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declined_at: Option<DateTime<Utc>>,
    /// The age recipient in force at the time of the answer.
    pub recipient_age_pubkey: String,
}

impl ConsentRecord {
    /// Build an accepted record stamped now.
    #[must_use]
    pub fn accepted(recipient: String) -> Self {
        Self {
            version: CONSENT_VERSION,
            accepted_at: Some(Utc::now()),
            declined_at: None,
            recipient_age_pubkey: recipient,
        }
    }

    /// Build a declined record stamped now.
    #[must_use]
    pub fn declined(recipient: String) -> Self {
        Self {
            version: CONSENT_VERSION,
            accepted_at: None,
            declined_at: Some(Utc::now()),
            recipient_age_pubkey: recipient,
        }
    }

    /// Convenience predicate: the operator actively accepted.
    #[must_use]
    pub fn is_accepted(&self) -> bool {
        self.accepted_at.is_some()
    }
}

/// Resolve the config base directory, honouring `COSMON_CONFIG_HOME` for
/// test isolation. Falls back to [`dirs::config_dir`] and finally to
/// `~/.config/` when the platform dir isn't available.
pub fn config_base_dir() -> PathBuf {
    if let Ok(p) = std::env::var("COSMON_CONFIG_HOME") {
        return PathBuf::from(p);
    }
    if let Some(p) = dirs::config_dir() {
        return p;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home).join(".config")
}

/// Path to the consent file under the resolved config base.
pub fn consent_path() -> PathBuf {
    config_base_dir().join(CONSENT_FILE)
}

/// Path to the default age recipient file.
pub fn recipient_path() -> PathBuf {
    config_base_dir().join(RECIPIENT_FILE)
}

/// Load the consent record, if one exists.
///
/// # Errors
/// Fails when the file exists but is unreadable or malformed. A missing file
/// is not an error — callers treat `Ok(None)` as "deny by default, never
/// prompted".
pub fn load_consent() -> anyhow::Result<Option<ConsentRecord>> {
    let path = consent_path();
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
    let record: ConsentRecord = toml::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
    Ok(Some(record))
}

/// Persist a consent record atomically (temp file + rename).
///
/// # Errors
/// Fails on filesystem errors (permission denied, disk full, …).
pub fn save_consent(record: &ConsentRecord) -> anyhow::Result<PathBuf> {
    let path = consent_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", parent.display()))?;
    }
    let body = toml::to_string_pretty(record)
        .map_err(|e| anyhow::anyhow!("failed to serialise consent record: {e}"))?;
    // Atomic write: write-to-tmp then rename, so a crash never leaves a
    // half-written TOML file on disk that would panic on the next `cs` run.
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, body).map_err(|e| anyhow::anyhow!("failed to write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path)
        .map_err(|e| anyhow::anyhow!("failed to commit {}: {e}", path.display()))?;
    Ok(path)
}

/// Read the shipped age recipient. Returns a best-effort string with
/// whitespace trimmed, or a placeholder when the file is missing (we still
/// record the answer so the user isn't re-prompted every run).
#[must_use]
pub fn read_recipient() -> String {
    fs::read_to_string(recipient_path()).map_or_else(|_| String::new(), |s| s.trim().to_owned())
}

/// Read a single y/n answer from stdin. Anything that isn't `y`/`yes`/`o`/
/// `oui` (case-insensitive) is treated as a decline — the prompt is
/// deny-by-default, so ambiguous input falls through to the safer answer.
///
/// The French `o`/`oui` stay accepted although the prompt now asks `[y/N]`.
/// They cost one match arm, and dropping them would silently turn a habit
/// somebody formed against the old prompt into a decline they did not mean —
/// the one direction of this change that could lose an answer.
fn parse_yes_no(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "o" | "oui" | "y" | "yes"
    )
}

/// Can a first-run question actually be **answered** right now?
///
/// The predicate this guard used to test was `stdin().is_terminal()` — "is a
/// terminal attached?". That is not the same question. A prompt is answerable
/// only when a human can *see* it and *type at* it, and those two halves come
/// apart in exactly the case that hangs: an orchestrator captures the child's
/// stdout (`OUT="$(cs …)"`) while stdin stays the inherited TTY. The question
/// is then printed into a variable nobody reads, on an input nobody watches.
/// No keystroke can arrive and no output can warn — a mute hang. Requiring
/// **both** stdin and stdout to be terminals closes it: a captured stdout
/// auto-declines down the same path a missing TTY already took.
///
/// stderr is deliberately **not** part of the conjunction. It is the channel
/// we write the auto-decline trace on, and it is routinely redirected to a
/// log file by supervisors that leave the interactive pair intact; folding it
/// in would suppress a question the operator can see and answer perfectly
/// well. The prompt is written to stdout, so stdout is the surface that must
/// be visible — stderr's state says nothing about that.
///
/// # Why the conjunction is a separate function
///
/// It is the part with a truth table, and a truth table can be asserted
/// exhaustively. Reading the process's real fds cannot be: a unit test runs
/// inside whatever fds cargo handed the test binary, and cargo hands it a
/// non-terminal stdin *and* a non-terminal stdout. A test that asserts
/// `!can_ask_interactively()` in that harness therefore stays green with the
/// stdout half deleted — it never reaches the second term. That test existed
/// here, claiming to prove the fix; it proved that cargo captures stdout.
///
/// So the two halves are tested by the two things that can each see one of
/// them: [`answerable`] below, exhaustively and in-process, and
/// `tests/consent_non_blocking.rs`, which allocates a real pty for stdin and a
/// real pipe for stdout and requires the binary to *terminate*.
fn can_ask_interactively() -> bool {
    answerable(io::stdin().is_terminal(), io::stdout().is_terminal())
}

/// The rule itself: a question is answerable only where it can be both seen
/// and typed at.
const fn answerable(stdin_is_terminal: bool, stdout_is_terminal: bool) -> bool {
    stdin_is_terminal && stdout_is_terminal
}

/// Print the prompt, read one line from stdin, and return the accept bit.
fn prompt_on_tty() -> anyhow::Result<bool> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{PROMPT}")?;
    write!(handle, "> ")?;
    handle.flush()?;
    drop(handle);
    let mut buf = String::new();
    io::stdin()
        .read_line(&mut buf)
        .map_err(|e| anyhow::anyhow!("failed to read answer: {e}"))?;
    Ok(parse_yes_no(&buf))
}

/// Either/or outcome of a first-run consent decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Operator explicitly accepted on a TTY (or via `--accept`).
    Accepted,
    /// Operator explicitly declined on a TTY (or via `--decline`).
    Declined,
    /// The question could not be answered where it was asked — no TTY, or a
    /// TTY on stdin with a captured stdout — so it was not asked at all, and
    /// deny-by-default was recorded. See [`can_ask_interactively`].
    SkippedNoTty,
}

/// Ensure a consent record exists for this user.
///
/// * If one already exists, this is a no-op: returns `Ok(None)`.
/// * Otherwise, prompts (only when [`can_ask_interactively`] holds) or
///   auto-declines, and persists the answer.
///
/// Callers that only need to *check* consent without prompting should use
/// [`load_consent`] instead. Callers must be an explicitly interactive
/// moment — never the dispatch path (see the module docs and ADR-163) — and
/// should surface [`Decision::SkippedNoTty`] on stderr so the auto-decline
/// leaves a trace the operator can find later.
///
/// # Errors
/// Fails on filesystem I/O errors when persisting the record.
pub fn ensure_consent() -> anyhow::Result<Option<Decision>> {
    ensure_consent_where(can_ask_interactively())
}

/// [`ensure_consent`] with the interactivity of the environment supplied
/// rather than read from the process's own file descriptors.
///
/// This exists because the ambient tty-ness of a `cargo test` process is not
/// the test author's to choose: piped into a log it is false, run in a live
/// terminal it is true. A test that called [`ensure_consent`] to exercise the
/// auto-decline branch therefore asserted about the runner's terminal, not
/// about the code — and on an interactive bench it printed a real consent
/// question and blocked on a real read. The condition under test is part of
/// the fixture, so it is passed in.
///
/// # Errors
/// Fails on filesystem I/O errors when persisting the record.
fn ensure_consent_where(answerable_here: bool) -> anyhow::Result<Option<Decision>> {
    if load_consent()?.is_some() {
        return Ok(None);
    }
    let recipient = read_recipient();
    let decision = if answerable_here {
        if prompt_on_tty()? {
            Decision::Accepted
        } else {
            Decision::Declined
        }
    } else {
        Decision::SkippedNoTty
    };
    let record = match decision {
        Decision::Accepted => ConsentRecord::accepted(recipient),
        Decision::Declined | Decision::SkippedNoTty => ConsentRecord::declined(recipient),
    };
    save_consent(&record)?;
    Ok(Some(decision))
}

/// Execute the `opt-in-share` subcommand.
///
/// # Errors
/// Fails on filesystem I/O errors when reading or persisting consent.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    if args.status {
        return render_status(ctx);
    }

    let existing = load_consent()?;
    if let Some(record) = existing {
        render_already_decided(ctx, &record);
        return Ok(());
    }

    let recipient = read_recipient();
    let (decision, record) = if args.accept {
        (Decision::Accepted, ConsentRecord::accepted(recipient))
    } else if args.decline {
        (Decision::Declined, ConsentRecord::declined(recipient))
    } else if can_ask_interactively() {
        if prompt_on_tty()? {
            (Decision::Accepted, ConsentRecord::accepted(recipient))
        } else {
            (Decision::Declined, ConsentRecord::declined(recipient))
        }
    } else {
        (Decision::SkippedNoTty, ConsentRecord::declined(recipient))
    };

    let path = save_consent(&record)?;
    render_decision(ctx, decision, &record, &path);
    Ok(())
}

fn render_status(ctx: &Context) -> anyhow::Result<()> {
    let record = load_consent()?;
    let path = consent_path();
    if ctx.json {
        let out = serde_json::json!({
            "command": "opt-in-share",
            "mode": "status",
            "path": path.to_string_lossy(),
            "recorded": record.is_some(),
            "accepted": record.as_ref().is_some_and(ConsentRecord::is_accepted),
            "record": record,
        });
        println!("{out}");
    } else {
        match record {
            None => println!("no consent on record (deny-by-default) — run `cs opt-in-share`"),
            Some(r) if r.is_accepted() => println!(
                "opt-in-share: accepted at {}",
                r.accepted_at.map(|t| t.to_rfc3339()).unwrap_or_default()
            ),
            Some(r) => println!(
                "opt-in-share: declined at {}",
                r.declined_at.map(|t| t.to_rfc3339()).unwrap_or_default()
            ),
        }
    }
    Ok(())
}

fn render_already_decided(ctx: &Context, record: &ConsentRecord) {
    let path = consent_path();
    if ctx.json {
        let out = serde_json::json!({
            "command": "opt-in-share",
            "mode": "already-decided",
            "path": path.to_string_lossy(),
            "record": record,
        });
        println!("{out}");
    } else if record.is_accepted() {
        println!(
            "opt-in-share: already accepted at {} (edit {} to change)",
            record
                .accepted_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_default(),
            path.display()
        );
    } else {
        println!(
            "opt-in-share: already declined at {} (edit {} to change)",
            record
                .declined_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_default(),
            path.display()
        );
    }
}

fn render_decision(
    ctx: &Context,
    decision: Decision,
    record: &ConsentRecord,
    path: &std::path::Path,
) {
    if ctx.json {
        let mode = match decision {
            Decision::Accepted => "accepted",
            Decision::Declined => "declined",
            Decision::SkippedNoTty => "skipped-no-tty",
        };
        let out = serde_json::json!({
            "command": "opt-in-share",
            "mode": mode,
            "path": path.to_string_lossy(),
            "record": record,
        });
        println!("{out}");
    } else {
        match decision {
            Decision::Accepted => println!(
                "opt-in-share: acceptance recorded ({} → {})",
                record.recipient_age_pubkey,
                path.display()
            ),
            Decision::Declined => println!(
                "opt-in-share: decline recorded (nothing is shared, {})",
                path.display()
            ),
            Decision::SkippedNoTty => println!(
                "opt-in-share: {} — declined by default ({})",
                skip_reason(),
                path.display()
            ),
        }
    }
    if decision == Decision::SkippedNoTty {
        warn_skipped_on_stderr(path);
    }
}

/// Which half of the interactive pair was missing, in the operator's words.
///
/// Only meaningful for [`Decision::SkippedNoTty`]. `stdin non-tty` keeps its
/// historical spelling: it is the fragment operators grep container logs for,
/// it was already English, and it names a POSIX condition rather than reading
/// as prose. `stdout captured` is the case ADR-163 added, and it names itself
/// because `stdin non-tty` would be a lie there.
fn skip_reason() -> &'static str {
    skip_reason_for(io::stdin().is_terminal())
}

/// The rule behind [`skip_reason`], with the fd read lifted out so both
/// branches are reachable from a test — the same seam as [`answerable`],
/// for the same reason: a `cargo test` process cannot choose its own stdin.
const fn skip_reason_for(stdin_is_terminal: bool) -> &'static str {
    if stdin_is_terminal {
        "stdout captured"
    } else {
        "stdin non-tty"
    }
}

/// Leave a trace of an auto-decline on stderr, but only when the normal
/// stdout rendering could not have been seen.
///
/// A question that declines itself must not do so silently. When stdout is a
/// terminal the operator has already read the decision there and a second
/// copy is noise; when stdout is captured or redirected — the very situation
/// that forced the auto-decline — stderr is the one channel still likely to
/// reach a human or a log, so the trace goes there, naming the remedy.
pub fn warn_skipped_on_stderr(path: &std::path::Path) {
    if io::stdout().is_terminal() {
        return;
    }
    eprintln!(
        "opt-in-share: {} — the question cannot be answered here, so a decline \
         was recorded by default ({}). To decide explicitly, run \
         `cs opt-in-share --accept` or `cs opt-in-share --decline`.",
        skip_reason(),
        path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;

    /// Tests in this module mutate `COSMON_CONFIG_HOME` (a process-global
    /// env var) and each one expects exclusive ownership. Cargo runs tests
    /// in parallel by default, so we serialise the env-var-mutating tests
    /// behind a shared mutex rather than pulling in `serial_test`.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        // Poisoning is harmless here — the critical section is "set env
        // var and run assertions"; a panic downstream does not corrupt the
        // (unit) guard state. Unwrap into the inner guard either way.
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Scope `COSMON_CONFIG_HOME` to a temp dir for the duration of the
    /// test. Restores the previous value on drop so tests don't leak into
    /// each other.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn toml_roundtrip_accepted() {
        let original = ConsentRecord::accepted("age1test".to_owned());
        let body = toml::to_string_pretty(&original).expect("serialise");
        let round: ConsentRecord = toml::from_str(&body).expect("parse");
        assert_eq!(round, original);
        assert!(round.is_accepted());
        assert!(round.declined_at.is_none());
    }

    #[test]
    fn toml_roundtrip_declined() {
        let original = ConsentRecord::declined("age1test".to_owned());
        let body = toml::to_string_pretty(&original).expect("serialise");
        let round: ConsentRecord = toml::from_str(&body).expect("parse");
        assert_eq!(round, original);
        assert!(!round.is_accepted());
        assert!(round.accepted_at.is_none());
    }

    #[test]
    fn save_then_load_returns_same_record() {
        let _lock = env_lock();
        let tmp = TempDir::new().expect("tempdir");
        let _g = EnvGuard::set("COSMON_CONFIG_HOME", tmp.path());

        assert!(load_consent().expect("load-empty").is_none());

        let record = ConsentRecord::accepted("age1roundtrip".to_owned());
        let path = save_consent(&record).expect("save");
        assert!(path.exists(), "consent file should exist after save");

        let loaded = load_consent().expect("load").expect("record present");
        assert_eq!(loaded, record);
    }

    /// Both spellings, on purpose: `y`/`yes` is what the prompt now asks for,
    /// and `o`/`oui` is what the French prompt asked for until
    /// noogram/cosmon#76. Delete the French arm and an operator who learned
    /// the old answer gets a silent decline instead of the accept they typed.
    #[test]
    fn parse_yes_no_accepts_en_and_legacy_fr_variants() {
        assert!(parse_yes_no("o"));
        assert!(parse_yes_no("O\n"));
        assert!(parse_yes_no("oui"));
        assert!(parse_yes_no("Oui"));
        assert!(parse_yes_no("y"));
        assert!(parse_yes_no("YES"));
    }

    #[test]
    fn parse_yes_no_rejects_everything_else() {
        // Deny-by-default: empty / whitespace / n / non / garbage → false.
        assert!(!parse_yes_no(""));
        assert!(!parse_yes_no(" \n"));
        assert!(!parse_yes_no("n"));
        assert!(!parse_yes_no("non"));
        assert!(!parse_yes_no("maybe"));
    }

    #[test]
    fn ensure_consent_is_noop_when_record_already_exists() {
        let _lock = env_lock();
        let tmp = TempDir::new().expect("tempdir");
        let _g = EnvGuard::set("COSMON_CONFIG_HOME", tmp.path());

        let pre = ConsentRecord::accepted("age1preset".to_owned());
        save_consent(&pre).expect("preset");

        // `true` on purpose: the existing record must short-circuit *before*
        // the question, including where the question would be answerable. A
        // regression here would prompt, so the assertion has to be made in the
        // condition that would prompt — injected, never inherited.
        let outcome = ensure_consent_where(true).expect("ensure");
        assert!(
            outcome.is_none(),
            "ensure_consent must not re-decide when a record exists"
        );

        let after = load_consent().expect("load").expect("still present");
        assert_eq!(after, pre, "ensure_consent must not mutate existing record");
    }

    /// The whole truth table, including the one row that is the bug.
    ///
    /// This replaces an assertion that read the *harness's* fds and concluded
    /// something about the conjunction. It could not: cargo gives the test
    /// binary a non-terminal stdin too, so the pre-fix `stdin().is_terminal()`
    /// guard already returned `false` there and the test passed against the
    /// broken build. Stating the rows explicitly is what makes the second term
    /// load-bearing — delete it from [`answerable`] and the second row goes
    /// red, which is the only reason this test is worth having.
    #[test]
    fn a_captured_stdout_is_never_an_answerable_question() {
        assert!(answerable(true, true), "a real terminal on both ends: ask");
        assert!(
            !answerable(true, false),
            "the container hang: a TTY on stdin, stdout captured by \
             `OUT=$(cs …)`. The question is printed into a variable nobody \
             reads, on an input nobody watches — it must not be asked",
        );
        assert!(!answerable(false, true), "no stdin to answer on");
        assert!(!answerable(false, false), "no terminal at all");
    }

    /// The unanswerable-here branch, with the "here" injected.
    ///
    /// This used to call [`ensure_consent`], which reads the *harness's* fds.
    /// Piped — every CI lane, every bench — those are not terminals and the
    /// test passed. On a live interactive terminal they are, so the test asked
    /// the operator a real consent question and blocked reading the answer: at
    /// `--test-threads=1` it stole the terminal, and under default parallelism
    /// libtest swallowed the prompt and the run hung forever (issue #43). The
    /// tty condition is a fixture, so it goes through the seam.
    #[test]
    fn ensure_consent_on_non_tty_stores_declined() {
        let _lock = env_lock();
        let tmp = TempDir::new().expect("tempdir");
        let _g = EnvGuard::set("COSMON_CONFIG_HOME", tmp.path());

        let outcome = ensure_consent_where(false)
            .expect("ensure")
            .expect("decided");
        assert_eq!(outcome, Decision::SkippedNoTty);

        let record = load_consent().expect("load").expect("present");
        assert!(record.declined_at.is_some());
        assert!(record.accepted_at.is_none());
        assert_eq!(record.version, CONSENT_VERSION);
    }

    /// The prompt's first clause is load-bearing outside this file.
    ///
    /// `tests/consent_non_blocking.rs` proves that the question is *not*
    /// printed where it cannot be answered, and it can only do that by
    /// grepping stdout for the question. `cmd` is a binary module, so that
    /// test cannot import [`PROMPT`] and carries the substring as a literal.
    /// Reword the first line here and that assertion silently stops looking
    /// for anything — it would pass against a build that prints the question
    /// into the captured stdout, which is the bug ADR-163 closed. This test
    /// is the tripwire: it goes red first and names the file to update.
    #[test]
    fn prompt_opens_with_the_clause_the_pty_test_greps_for() {
        assert!(
            PROMPT.starts_with("Share diagnostic information"),
            "tests/consent_non_blocking.rs greps stdout for this clause; \
             update both together, got: {PROMPT}"
        );
    }

    /// The two named consent strings speak the CLI's language
    /// (noogram/cosmon#76).
    ///
    /// A newcomer on the published install route met French exactly once, at
    /// the one prompt asking about their own data. What this guards is a
    /// *re-introduction*: words that only the old French strings had.
    /// Accented letters are not the test — `→` and `—` are legitimately
    /// non-ASCII and the accepted-record line uses one.
    ///
    /// Scope, stated so the gap is visible: this covers [`PROMPT`] and both
    /// branches of [`skip_reason_for`], the two strings that exist as named
    /// items. The three outcome lines are `println!` format literals inside
    /// [`render_decision`] and cannot be named from here; asserting a *copy*
    /// of them would test the copy. They are covered by the `opt_in_share.rs`
    /// integration tests, which read the binary's real stdout.
    #[test]
    fn named_consent_strings_do_not_revert_to_french() {
        let surfaces = [PROMPT, skip_reason_for(true), skip_reason_for(false)];
        for marker in [
            "Acceptez",
            "enregistr",
            "refus",
            "chiffr",
            "aucune trace",
            "posable",
            "sortie captur",
            "[o/N]",
        ] {
            for surface in surfaces {
                assert!(
                    !surface.contains(marker),
                    "French wording {marker:?} is back on the consent surface: {surface}"
                );
            }
        }
    }

    /// Both halves of the skip reason, named by the condition that produces
    /// them. `stdin non-tty` is deliberately unchanged — it is the fragment
    /// operators grep container logs for (ADR-163, the container guide).
    #[test]
    fn skip_reason_names_the_missing_half() {
        assert_eq!(
            skip_reason_for(true),
            "stdout captured",
            "a TTY on stdin means it was stdout that was captured"
        );
        assert_eq!(
            skip_reason_for(false),
            "stdin non-tty",
            "historical spelling: operators grep logs for it"
        );
    }

    #[test]
    fn consent_path_honours_cosmon_config_home() {
        let _lock = env_lock();
        let tmp = TempDir::new().expect("tempdir");
        let _g = EnvGuard::set("COSMON_CONFIG_HOME", tmp.path());
        let path = consent_path();
        assert!(path.starts_with(tmp.path()));
        assert!(path.ends_with("cosmon/consent.toml"));
    }
}
