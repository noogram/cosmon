// SPDX-License-Identifier: AGPL-3.0-only

//! `cs opt-in-share` — first-run consent prompt for developer-share telemetry.
//!
//! operator-b (or any fresh user) should see, on her very first `cs` invocation,
//! a small French prompt asking whether she agrees to share encrypted bundles
//! with the cosmon developers. The answer — accept or decline — is persisted
//! once to `~/.config/cosmon/consent.toml` and never asked again.
//!
//! Design constraints (from the MVP operator-b brief, delib fe35 §c):
//!
//! * **Deny by default.** If the consent file is missing, no share occurs.
//! * **Explicit trace either way.** Accept writes `accepted_at`; decline
//!   writes `declined_at`. The file's presence + one of those two keys is the
//!   durable proof of the operator's choice.
//! * **Non-interactive = decline.** When stdin is not a TTY (CI, scripts,
//!   worker shells), we skip the prompt and store `declined_at` without
//!   asking. This keeps the hook in `cs tackle` safe for unattended runs.
//! * **No trace in the user's project.** Consent lives under
//!   `~/.config/cosmon/`, never inside the project's `.cosmon/` directory,
//!   so sharing toggles don't leak into `git log`.
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

/// Prompt text shown on stdout at first run. Kept in French to match the
/// operator-b onboarding surface. The wording is deliberate: it names the
/// encryption, the sole recipient, and the no-trace-in-commits guarantee,
/// then asks a single yes/no question with a deny-by-default marker.
pub const PROMPT_FR: &str = "\
Acceptez-vous de partager des informations avec les développeurs cosmon ?
Les bundles seront chiffrés age, seul le mainteneur Noogram pourra les lire.
Modifications à votre projet : aucune trace de cosmon dans vos commits. [o/N]";

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

/// Read a single y/n answer from stdin. Anything that isn't `o`/`oui`/`y`/
/// `yes` (case-insensitive) is treated as a decline — the prompt is
/// deny-by-default, so ambiguous input falls through to the safer answer.
fn parse_yes_no(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "o" | "oui" | "y" | "yes"
    )
}

/// Print the prompt, read one line from stdin, and return the accept bit.
fn prompt_on_tty() -> anyhow::Result<bool> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{PROMPT_FR}")?;
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
    /// No TTY available — treated as decline (deny-by-default).
    SkippedNoTty,
}

/// Ensure a consent record exists for this user.
///
/// * If one already exists, this is a no-op: returns `Ok(None)`.
/// * Otherwise, prompts (when stdin is a TTY) or auto-declines (when not)
///   and persists the answer.
///
/// Callers that only need to *check* consent without prompting should use
/// [`load_consent`] instead.
///
/// # Errors
/// Fails on filesystem I/O errors when persisting the record.
pub fn ensure_consent() -> anyhow::Result<Option<Decision>> {
    if load_consent()?.is_some() {
        return Ok(None);
    }
    let recipient = read_recipient();
    let decision = if io::stdin().is_terminal() {
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
    } else if io::stdin().is_terminal() {
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
                "opt-in-share: acceptation enregistrée ({} → {})",
                record.recipient_age_pubkey,
                path.display()
            ),
            Decision::Declined => println!(
                "opt-in-share: refus enregistré (aucun partage, {})",
                path.display()
            ),
            Decision::SkippedNoTty => println!(
                "opt-in-share: stdin non-tty — refus par défaut enregistré ({})",
                path.display()
            ),
        }
    }
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

    #[test]
    fn parse_yes_no_accepts_fr_and_en_variants() {
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

        let outcome = ensure_consent().expect("ensure");
        assert!(
            outcome.is_none(),
            "ensure_consent must not re-decide when a record exists"
        );

        let after = load_consent().expect("load").expect("still present");
        assert_eq!(after, pre, "ensure_consent must not mutate existing record");
    }

    #[test]
    fn ensure_consent_on_non_tty_stores_declined() {
        let _lock = env_lock();
        let tmp = TempDir::new().expect("tempdir");
        let _g = EnvGuard::set("COSMON_CONFIG_HOME", tmp.path());

        // cargo test runs with stdin attached to a pipe, so is_terminal()
        // returns false — this branch is exactly what we want to exercise.
        let outcome = ensure_consent().expect("ensure").expect("decided");
        assert_eq!(outcome, Decision::SkippedNoTty);

        let record = load_consent().expect("load").expect("present");
        assert!(record.declined_at.is_some());
        assert!(record.accepted_at.is_none());
        assert_eq!(record.version, CONSENT_VERSION);
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
