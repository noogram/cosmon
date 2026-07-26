//! The third door: refuse a TUI worker that has no credential to work with
//! (COSMON-DEV issue #20, the login-door half reported by `@jdthaler`).
//!
//! # What door 3 actually is — measured, and not what the report said
//!
//! Doors 1 and 2 ([`crate::claude_trust`]) are blocking dialogs. Door 3 is
//! **not**. Measured on macOS against the installed Claude Code 2.1.220 on
//! 2026-07-25, with both consent gates pre-granted and a fresh
//! `CLAUDE_CONFIG_DIR` that holds no credential:
//!
//! ```text
//!   ⏵⏵ bypass permissions on (shift+tab to cycle)     Not logged in · Run /login
//! ```
//!
//! The TUI boots all the way to the **composer**. It renders no login selector
//! and blocks on nothing. It will accept a pasted briefing and then never
//! produce a token. So the reported symptom ("waits on the login selector") is
//! right about the *consequence* and wrong about the *shape*, and the wrong
//! shape matters: there is no dialog to type into, no keystroke that can rescue
//! it, and every liveness signal cosmon has says the worker is healthy. Of the
//! three doors this is the quietest failure, which is exactly why it needs a
//! pre-spawn refusal rather than a runtime probe.
//!
//! # What counts as a usable credential — established by measurement
//!
//! The report named a `credentials.json` in `CLAUDE_CONFIG_DIR`. That name does
//! not exist. Four arms, each a fresh isolated `CLAUDE_CONFIG_DIR` with both
//! consent gates pre-granted, launched under tmux and read back from the pane:
//!
//! | arm | environment / disk | measured outcome |
//! |---|---|---|
//! | A | nothing | composer, footer `Not logged in · Run /login` |
//! | B | `CLAUDE_CODE_OAUTH_TOKEN` set | composer, **no** footer — authenticated |
//! | C | `<config dir>/.credentials.json` present | composer, no footer — authenticated |
//! | D | `ANTHROPIC_API_KEY` set | **blocking dialog**, default `❯ 2. No (recommended)` |
//!
//! Three findings, all load-bearing for the check below:
//!
//! 1. The file is **`.credentials.json`** — dot-prefixed — inside the *resolved
//!    config dir*, not `credentials.json`. A check written from the report's
//!    file name would refuse every dispatch on every machine.
//! 2. `CLAUDE_CODE_OAUTH_TOKEN` **does** satisfy the TUI (arm B), contradicting
//!    the report. It is therefore accepted here. What it does not do is make the
//!    *headless* and *TUI* paths differ — both take it.
//! 3. `ANTHROPIC_API_KEY` is **not** a usable TUI credential: it opens a fourth
//!    door (arm D), a consent dialog whose default answer is *No*. It is
//!    recorded in the refusal so the remedy can name it, never counted as a
//!    credential. Pre-granting *that* dialog is a separate piece of work; this
//!    module's job is to stop lying about it.
//!
//! # And the one that no file check can see: the OS keychain
//!
//! On macOS the credential normally lives in the login keychain, not on disk.
//! The developer machine this was written on has **no `.credentials.json`
//! anywhere** and a working fleet. A file-presence check alone would have
//! refused every dispatch on it — the failure mode a "just look for the file"
//! implementation walks straight into.
//!
//! Decompiled from the shipped 2.1.220 binary, the credential store is
//! `keychain-with-plaintext-fallback`: a `security find-generic-password` read
//! first, the `.credentials.json` plaintext backend second. The keychain item is
//! named
//!
//! ```text
//! service = "Claude Code-credentials" + ("-" + sha256(<config dir>)[..8]  if CLAUDE_CONFIG_DIR is set)
//! account = $USER   (or "claude-code-user" when USER is unset or exotic)
//! ```
//!
//! and that derivation is not a reading of the source — it was **confirmed by
//! probing the name it predicts against the real keychain**, which found the
//! live item for this machine's account config dir. [`keychain_service_name`]
//! reproduces it.
//!
//! # The layout shift, checked rather than assumed
//!
//! [`crate::claude_trust`] documents that Claude Code splits its two consent
//! files across directories when `CLAUDE_CONFIG_DIR` is unset. The question for
//! this file had to be asked again rather than inherited, and the answer is that
//! `.credentials.json` follows `settings.json`, not `.claude.json`:
//!
//! | `CLAUDE_CONFIG_DIR` | credentials file |
//! |---|---|
//! | set | `$CLAUDE_CONFIG_DIR/.credentials.json` |
//! | unset | `$HOME/.claude/.credentials.json` |
//!
//! So it is *never* `$HOME/.credentials.json`. `CLAUDE_SECURESTORAGE_CONFIG_DIR`
//! overrides the storage dir when present (an empty value meaning
//! `$HOME/.claude`) and is the hash input for the keychain service name;
//! [`credentials_file`] and [`keychain_service_name`] both honour it.
//!
//! # The secret is never touched
//!
//! This module answers *is there a usable credential*, never *what is it*. It
//! never opens the credentials file — presence, regular-file-ness, non-emptiness
//! and the target uid's read bits come from `stat(2)` alone. The keychain probe
//! runs `security find-generic-password` **without `-w`**, so the password is
//! not even printed to a pipe, and the probe port returns a `bool`: there is no
//! type in this module capable of carrying a secret. No error message, log line,
//! or molecule artefact can therefore leak a token fragment, because no code
//! path ever holds one.
//!
//! # Scope: the TUI path only
//!
//! The refusal fires on the spawn paths that launch an **interactive** `claude`
//! — `cs tackle --adapter claude`, and the `cs thaw` / patrol-respawn path when
//! it spawns a bare TUI. A headless `claude -p` is deliberately left alone: it
//! *exits*, non-zero and immediately, when it has no credential, which cosmon
//! already classifies through `adapter_exit`. Door 3 is a doctrine about mute
//! hangs; a process that dies with a status is not one, and refusing it here
//! would trade a loud failure for a differently-loud failure while adding a way
//! to wrongly block a working dispatch. The two paths are named at their call
//! sites so the asymmetry is a decision, not an oversight.
//!
//! # Fail-closed
//!
//! [`check_tui_credentials`] returns `Err` when it can find no usable
//! credential. Callers must **refuse the spawn**, for the same reason the
//! consent pre-grant is fail-closed: a worker that looks healthy and produces
//! nothing costs more than a dispatch that fails with a remedy.

use std::path::{Path, PathBuf};

/// Where a TUI worker's credential was found.
///
/// Carries only *locators* — a path, a keychain item name — never a secret.
/// There is deliberately no variant that can hold token bytes, so no caller can
/// log one by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    /// `CLAUDE_CODE_OAUTH_TOKEN` was set in the environment the worker
    /// inherits. Measured (arm B) to satisfy the TUI on 2.1.220.
    EnvOauthToken,
    /// An OS keychain item exists for the resolved config dir.
    Keychain {
        /// The keychain service name that matched. A path hash, not a secret.
        service: String,
    },
    /// A `.credentials.json` the worker's uid can read.
    File {
        /// The credentials file path.
        path: PathBuf,
    },
}

/// Why a TUI worker must not be spawned.
///
/// Every variant is a spawn-refusal cause. None of them carries credential
/// content; the most specific thing any of them names is a filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoginRefusal {
    /// No credential source at all: no environment token, no keychain item, no
    /// credentials file. The worker would boot to a composer reading
    /// `Not logged in · Run /login` and never produce a token.
    #[error(
        "no usable Claude Code credential for the interactive worker: \
         {credentials_file} does not exist{keychain_clause}, and \
         CLAUDE_CODE_OAUTH_TOKEN is not set{api_key_clause}"
    )]
    NoCredential {
        /// The `.credentials.json` path that was probed and found absent.
        credentials_file: String,
        /// Rendered tail naming the keychain item that was probed and found
        /// absent. A path digest, not a secret.
        keychain_clause: String,
        /// Rendered tail naming the `ANTHROPIC_API_KEY` trap when that variable
        /// is the *only* thing set, or empty otherwise.
        api_key_clause: String,
    },

    /// The credentials file exists but the uid the worker will run as cannot
    /// read it — the container shape where the image is built as root and then
    /// dropped to an unprivileged `USER`, leaving a root-owned `0600` file.
    #[error(
        "Claude Code credentials at {path} exist but uid {uid} cannot read them \
         (the file is present, its owner or mode is wrong for the worker's uid)"
    )]
    FileUnusableByUid {
        /// The credentials file path.
        path: String,
        /// The uid the worker would have run as.
        uid: u32,
    },

    /// Neither `CLAUDE_CONFIG_DIR` nor `HOME` is set, so the credentials file
    /// Claude Code will read cannot even be named.
    #[error(
        "cannot locate Claude Code's credentials file: neither CLAUDE_CONFIG_DIR nor HOME is set \
         in the dispatcher environment"
    )]
    UnknownConfigHome,
}

impl LoginRefusal {
    /// The operator-facing remedy for this refusal, in the register of the two
    /// consent refusals: name the closed door, distinguish the case, and say the
    /// one thing that has to change.
    ///
    /// Kept beside the error rather than inlined at the two call sites so both
    /// spawn paths say the same words — the tackle and thaw refusals drifting
    /// apart is how an operator learns to distrust the message.
    #[must_use]
    pub fn remedy(&self) -> String {
        match self {
            Self::NoCredential { .. } => "\
Provision a credential the interactive worker can use, by either: (a) exporting \
CLAUDE_CODE_OAUTH_TOKEN into the dispatcher's environment; (b) running `claude` \
once interactively under the same CLAUDE_CONFIG_DIR and completing `/login`, \
which writes the credential to the OS keychain or to \
$CLAUDE_CONFIG_DIR/.credentials.json; or (c) mounting an existing \
.credentials.json into that directory in the container image. Note that \
ANTHROPIC_API_KEY is NOT a substitute — measured on Claude Code 2.1.220 it opens \
its own consent dialog whose default answer is `No`, which is a fresh mute hang."
                .to_owned(),
            Self::FileUnusableByUid { path, uid } => format!(
                "Fix the ownership or mode of {path} for the uid the worker runs as \
                 (in a container, `chown {uid} {path} && chmod 600 {path}`). The file being \
                 present is not enough: the worker reads it as uid {uid}, and a credentials \
                 file copied in as root during the image build is unreadable after `USER` \
                 drops privileges."
            ),
            Self::UnknownConfigHome => "\
Export CLAUDE_CONFIG_DIR (or HOME) in the dispatcher's environment so the \
credentials file Claude Code will read can be named."
                .to_owned(),
        }
    }
}

/// Resolve the `.credentials.json` path a TUI worker will read.
///
/// `config_dir` is the resolved `CLAUDE_CONFIG_DIR` when the spawn path has one,
/// in which case the file sits inside it. `None` falls back to Claude Code's
/// default layout, `$HOME/.claude/.credentials.json` — note the `.claude/`
/// segment: this file follows `settings.json`, **not** `.claude.json`, which
/// sits directly in `$HOME`. `CLAUDE_SECURESTORAGE_CONFIG_DIR`, when present,
/// overrides both (an empty value meaning `$HOME/.claude`), matching the
/// `storageDir` resolution in the shipped binary.
///
/// `env_lookup` is injected so this stays testable without mutating the process
/// environment.
///
/// # Errors
///
/// [`LoginRefusal::UnknownConfigHome`] when no source names a directory. There
/// is no defensible guess: probing the wrong path would report "no credential"
/// for a perfectly provisioned worker.
pub fn credentials_file<F>(config_dir: Option<&str>, env_lookup: F) -> Result<PathBuf, LoginRefusal>
where
    F: Fn(&str) -> Option<String>,
{
    Ok(storage_dir(config_dir, env_lookup)?.join(CREDENTIALS_FILE))
}

/// The directory Claude Code's plaintext credential backend uses, and whose
/// name is hashed into the keychain service name.
fn storage_dir<F>(config_dir: Option<&str>, env_lookup: F) -> Result<PathBuf, LoginRefusal>
where
    F: Fn(&str) -> Option<String>,
{
    // The secure-storage override wins outright, and an *empty* value is
    // meaningful there rather than "unset": the binary reads it as "use the
    // default `$HOME/.claude`, and use no keychain suffix".
    if let Some(override_dir) = env_lookup(SECURESTORAGE_DIR_ENV) {
        return if override_dir.is_empty() {
            default_storage_dir(env_lookup)
        } else {
            Ok(PathBuf::from(override_dir))
        };
    }
    match config_dir.filter(|d| !d.is_empty()) {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => default_storage_dir(env_lookup),
    }
}

/// `$HOME/.claude` — the storage dir with nothing overriding it.
fn default_storage_dir<F>(env_lookup: F) -> Result<PathBuf, LoginRefusal>
where
    F: Fn(&str) -> Option<String>,
{
    match env_lookup("HOME").filter(|h| !h.is_empty()) {
        Some(home) => Ok(Path::new(&home).join(".claude")),
        None => Err(LoginRefusal::UnknownConfigHome),
    }
}

/// The keychain service name Claude Code stores a credential under for the
/// config dir `config_dir` resolves to.
///
/// `Claude Code-credentials`, plus `-<sha256(dir)[..8]>` when a config dir is
/// explicitly selected — the suffix is what keeps a multi-account fleet's
/// per-account credentials apart, and its absence is what makes the default
/// (no `CLAUDE_CONFIG_DIR`) item shared. Always resolvable: unlike the
/// credentials *file*, the item name needs no `HOME`, so there is always
/// something to probe.
///
/// The derivation was confirmed against the live keychain, not merely read out
/// of the binary: the predicted name found this machine's real item.
#[must_use]
pub fn keychain_service_name<F>(config_dir: Option<&str>, env_lookup: F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    use sha2::Digest as _;
    use std::fmt::Write as _;

    // Which string is hashed, and whether a suffix applies at all, follow the
    // same three-way precedence as `storage_dir`.
    let hashed = match env_lookup(SECURESTORAGE_DIR_ENV) {
        // Override present but empty: the default item, no suffix.
        Some(dir) if dir.is_empty() => return KEYCHAIN_SERVICE_BASE.to_owned(),
        Some(dir) => dir,
        None => match config_dir.filter(|d| !d.is_empty()) {
            Some(dir) => dir.to_owned(),
            // No explicit selection: the unsuffixed default item.
            None => return KEYCHAIN_SERVICE_BASE.to_owned(),
        },
    };
    let digest = sha2::Sha256::digest(hashed.as_bytes());
    // First four bytes, lowercase hex — the eight characters Claude Code takes
    // from the digest.
    digest
        .iter()
        .take(4)
        .fold(format!("{KEYCHAIN_SERVICE_BASE}-"), |mut name, byte| {
            let _ = write!(name, "{byte:02x}");
            name
        })
}

/// The keychain account name Claude Code stores its credential under: `$USER`,
/// falling back to a fixed name when that is unset or not a plain identifier.
#[must_use]
pub fn keychain_account<F>(env_lookup: F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    match env_lookup("USER") {
        Some(user)
            if !user.is_empty()
                && user
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')) =>
        {
            user
        }
        _ => KEYCHAIN_ACCOUNT_FALLBACK.to_owned(),
    }
}

/// Decide whether an interactive (TUI) `claude` worker has a credential to work
/// with, before anything is spawned.
///
/// `worker_uid` is the uid that will actually *read* the credential — the demote
/// target on a root dispatch, the dispatcher's own uid otherwise. Passing the
/// dispatcher's uid on a demoting spawn would check the wrong reader and miss
/// precisely the root-owned-file case [`LoginRefusal::FileUnusableByUid`]
/// exists for.
///
/// `keychain_present` is the injected keychain probe: `(service, account) ->
/// bool`. Production passes [`security_keychain_probe`]; a host with no keychain
/// backend (every Linux container) passes [`no_keychain`], and tests pass a
/// closure. It returns a `bool` on purpose — the port has no way to hand a
/// secret back.
///
/// Sources are consulted in the order the shipped binary consults them, so a
/// `Ok` verdict means "the same lookup the worker will run finds something".
///
/// # Errors
///
/// A [`LoginRefusal`]. Callers **must** treat it as a spawn refusal; see the
/// module docs on fail-closed.
pub fn check_tui_credentials<E, K>(
    config_dir: Option<&str>,
    worker_uid: u32,
    env_lookup: E,
    keychain_present: K,
) -> Result<CredentialSource, LoginRefusal>
where
    E: Fn(&str) -> Option<String>,
    K: Fn(&str, &str) -> bool,
{
    // 1. The environment token. Measured (arm B) to authenticate the TUI, and
    //    checked first because it is free and shadows the disk entirely.
    if env_lookup(OAUTH_TOKEN_ENV).is_some_and(|t| !t.is_empty()) {
        return Ok(CredentialSource::EnvOauthToken);
    }

    // 2. The OS keychain — the primary backend, and the reason a file-only
    //    check would refuse every dispatch on a macOS developer machine.
    let service = keychain_service_name(config_dir, &env_lookup);
    if keychain_present(&service, &keychain_account(&env_lookup)) {
        return Ok(CredentialSource::Keychain { service });
    }

    // 3. The plaintext fallback — the container case. `stat(2)` only: this
    //    never opens the file, so the secret is never in this process.
    let path = credentials_file(config_dir, &env_lookup)?;
    match file_credential_state(&path, worker_uid) {
        FileState::Usable => return Ok(CredentialSource::File { path }),
        FileState::PresentButUnreadable => {
            return Err(LoginRefusal::FileUnusableByUid {
                path: path.to_string_lossy().into_owned(),
                uid: worker_uid,
            })
        }
        FileState::Absent => {}
    }

    Err(LoginRefusal::NoCredential {
        credentials_file: path.to_string_lossy().into_owned(),
        keychain_clause: format!(", no keychain item `{service}` exists"),
        // Named only when it is the *only* thing set, because that is the exact
        // belief that costs an operator an afternoon: a container exporting an
        // API key looks provisioned and is not (arm D — its own dialog,
        // defaulting to `No`).
        api_key_clause: if env_lookup(API_KEY_ENV).is_some_and(|k| !k.is_empty()) {
            " (ANTHROPIC_API_KEY is set, but it is not a TUI credential — it \
             opens its own consent dialog instead)"
                .to_owned()
        } else {
            String::new()
        },
    })
}

/// What `stat(2)` says about a candidate credentials file, from the point of
/// view of the uid that will read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileState {
    /// A non-empty regular file `worker_uid` can read.
    Usable,
    /// A regular file that exists but is not readable by `worker_uid`.
    PresentButUnreadable,
    /// Nothing usable there — missing, empty, or not a regular file.
    Absent,
}

/// Classify `path` for `uid` without opening it.
///
/// A zero-byte file counts as [`FileState::Absent`]: it is the artefact a killed
/// `claude` leaves behind mid-write (the same reasoning
/// [`crate::claude_trust`] applies to a truncated `.claude.json`) and it
/// authenticates nothing, so reporting it as a credential would put the mute
/// worker straight back.
fn file_credential_state(path: &Path, uid: u32) -> FileState {
    use std::os::unix::fs::MetadataExt as _;

    // `metadata` follows symlinks, which is the question that matters: a
    // dangling link is an absent credential, and a link to a real file is the
    // multi-account layout (`cb` symlinks parts of a config dir together).
    let Ok(meta) = std::fs::metadata(path) else {
        return FileState::Absent;
    };
    if !meta.is_file() || meta.len() == 0 {
        return FileState::Absent;
    }

    // Read bit, in the permission triple that applies to this uid.
    let shift = if meta.uid() == uid {
        0
    } else if meta.gid() == uid {
        3
    } else {
        6
    };
    let read_mask = 0o400 >> shift;
    if meta.mode() & read_mask != read_mask {
        return FileState::PresentButUnreadable;
    }
    // A readable leaf under a directory the uid cannot traverse is not
    // readable, and `stat`ing only the leaf as a privileged dispatcher hides
    // that completely — the same trap
    // [`crate::demote_provisioning::path_usable_by_uid`] documents. That helper
    // cannot be reused here: it demands the `x` bit on its target, which a
    // `0600` credentials file correctly does not have.
    if !ancestors_traversable(path, uid) {
        return FileState::PresentButUnreadable;
    }
    FileState::Usable
}

/// Whether every existing ancestor directory of `path` grants `uid` the search
/// bit. A credentials file the worker cannot walk to is not a credential.
fn ancestors_traversable(path: &Path, uid: u32) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    let mut ancestor = path.parent();
    while let Some(dir) = ancestor {
        if let Ok(meta) = std::fs::metadata(dir) {
            let shift = if meta.uid() == uid {
                0
            } else if meta.gid() == uid {
                3
            } else {
                6
            };
            let mask = 0o100 >> shift;
            if meta.mode() & mask != mask {
                return false;
            }
        }
        ancestor = dir.parent();
    }
    true
}

/// The production keychain probe: does an item named `(service, account)` exist?
///
/// Runs `security find-generic-password` **without `-w`**, so the stored
/// password is never written to a pipe this process reads — only the item's
/// attributes are, and those are discarded. The return value is a `bool`, which
/// is the whole point: presence is the question, and no secret can travel back
/// through this signature.
///
/// A missing `security` binary (every Linux host) is reported as "absent"
/// rather than as an error: on those hosts the keychain backend genuinely does
/// not exist, and the plaintext file is the real answer.
#[must_use]
pub fn security_keychain_probe(service: &str, account: &str) -> bool {
    std::process::Command::new("security")
        .args(["find-generic-password", "-a", account, "-s", service])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A keychain probe for hosts that have none: always "absent".
///
/// Exists so a caller states which world it is in rather than passing a bare
/// closure whose meaning has to be read.
#[must_use]
pub fn no_keychain(_service: &str, _account: &str) -> bool {
    false
}

/// The plaintext credential file name — dot-prefixed, which the issue report
/// got wrong and which a check written from the report would have missed.
const CREDENTIALS_FILE: &str = ".credentials.json";
/// The keychain service name before the per-config-dir suffix.
const KEYCHAIN_SERVICE_BASE: &str = "Claude Code-credentials";
/// The keychain account used when `USER` is unset or not a plain identifier.
const KEYCHAIN_ACCOUNT_FALLBACK: &str = "claude-code-user";
/// Environment variable holding an OAuth token the TUI accepts (measured).
const OAUTH_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// Environment variable that looks like a credential and is not one (arm D).
const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
/// Override for the credential storage dir, honoured by the shipped binary.
const SECURESTORAGE_DIR_ENV: &str = "CLAUDE_SECURESTORAGE_CONFIG_DIR";

#[cfg(test)]
mod tests {
    use super::*;

    /// An environment with exactly the given keys set.
    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    /// The uid this test process runs as — the one that really can read the
    /// files it creates.
    fn own_uid() -> u32 {
        nix::unistd::Uid::effective().as_raw()
    }

    /// The container symptom, as a unit test: a `CLAUDE_CONFIG_DIR` with no
    /// credential anywhere must be REFUSED before any spawn, not launched.
    ///
    /// This is the test that fails for the right reason before the fix exists:
    /// the pre-fix code path had no refusal at all, so a worker was spawned into
    /// a composer reading `Not logged in · Run /login`.
    #[test]
    fn tui_path_with_no_credential_refuses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().to_string_lossy().into_owned();

        let err = check_tui_credentials(Some(&cfg), own_uid(), env_of(&[]), no_keychain)
            .expect_err("must refuse");

        match &err {
            LoginRefusal::NoCredential {
                credentials_file, ..
            } => assert!(
                credentials_file.ends_with("/.credentials.json"),
                "must name the dot-prefixed file: {credentials_file}"
            ),
            other => panic!("wrong refusal: {other:?}"),
        }
        // The remedy has to say the thing the tester believed and that cost the
        // time: an env API key is not a TUI credential.
        assert!(
            err.remedy().contains("ANTHROPIC_API_KEY"),
            "{}",
            err.remedy()
        );
    }

    /// The measured arm-B property: `CLAUDE_CODE_OAUTH_TOKEN` authenticates the
    /// TUI on 2.1.220, so a dispatch carrying one must NOT be blocked. This is
    /// the "an unaffected path is not refused" half of the contract — a check
    /// that refuses here would break every token-provisioned container.
    #[test]
    fn env_oauth_token_is_accepted_and_not_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().to_string_lossy().into_owned();

        let source = check_tui_credentials(
            Some(&cfg),
            own_uid(),
            env_of(&[("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-whatever")]),
            no_keychain,
        )
        .expect("token is a credential");

        assert_eq!(source, CredentialSource::EnvOauthToken);
    }

    /// An empty `CLAUDE_CODE_OAUTH_TOKEN` (an operator `export
    /// CLAUDE_CODE_OAUTH_TOKEN=`) is not a credential — the shipped binary
    /// treats the empty string as unset, and accepting it here would wave
    /// through exactly the dispatch this module exists to stop.
    #[test]
    fn empty_env_token_is_not_a_credential() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().to_string_lossy().into_owned();

        let err = check_tui_credentials(
            Some(&cfg),
            own_uid(),
            env_of(&[("CLAUDE_CODE_OAUTH_TOKEN", "")]),
            no_keychain,
        )
        .expect_err("must refuse");

        assert!(matches!(err, LoginRefusal::NoCredential { .. }), "{err:?}");
    }

    /// The macOS shape, and the false-refusal this module was nearly written
    /// with: a keychain item and NO file on disk is a fully working worker.
    /// A file-only check would have refused every dispatch on the machine this
    /// was developed on.
    #[test]
    fn keychain_item_alone_is_a_credential() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().to_string_lossy().into_owned();
        let expected = keychain_service_name(Some(&cfg), env_of(&[]));

        let source = check_tui_credentials(Some(&cfg), own_uid(), env_of(&[]), |service, _| {
            service == expected
        })
        .expect("keychain item is a credential");

        assert_eq!(source, CredentialSource::Keychain { service: expected });
    }

    /// The measured arm-C shape: a `.credentials.json` in the config dir is a
    /// credential. Its *contents* are never read — the file here is not even
    /// valid JSON, and that is the point: this check must not become a parser
    /// that touches the secret.
    #[test]
    fn credentials_file_is_a_credential_without_being_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().to_string_lossy().into_owned();
        let path = dir.path().join(".credentials.json");
        std::fs::write(&path, "not json, and never parsed").expect("seed");

        let source = check_tui_credentials(Some(&cfg), own_uid(), env_of(&[]), no_keychain)
            .expect("file is a credential");

        assert_eq!(source, CredentialSource::File { path });
    }

    /// A zero-byte credentials file is the killed-`claude` artefact, not a
    /// credential: treating it as one would spawn the mute worker again.
    #[test]
    fn empty_credentials_file_is_not_a_credential() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().to_string_lossy().into_owned();
        std::fs::write(dir.path().join(".credentials.json"), "").expect("seed");

        let err = check_tui_credentials(Some(&cfg), own_uid(), env_of(&[]), no_keychain)
            .expect_err("must refuse");

        assert!(matches!(err, LoginRefusal::NoCredential { .. }), "{err:?}");
    }

    /// Present-but-unusable is its own refusal, with its own remedy: the
    /// root-built / unprivileged-`USER` container. Distinguishing it from
    /// "absent" is what makes the message actionable — `chown` and `provision`
    /// are different fixes.
    #[test]
    fn present_but_unreadable_by_target_uid_is_a_distinct_refusal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().to_string_lossy().into_owned();
        let path = dir.path().join(".credentials.json");
        std::fs::write(&path, "opaque").expect("seed");

        // A uid that is neither owner nor group, against a mode with no
        // other-read bit — the shape of a root-owned 0600 file after `USER`.
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .expect("chmod");
        let foreign = own_uid() + 4242;

        let err = check_tui_credentials(Some(&cfg), foreign, env_of(&[]), no_keychain)
            .expect_err("must refuse");

        match &err {
            LoginRefusal::FileUnusableByUid { uid, .. } => assert_eq!(*uid, foreign),
            other => panic!("wrong refusal: {other:?}"),
        }
        assert!(err.remedy().contains("chown"), "{}", err.remedy());
    }

    /// `ANTHROPIC_API_KEY` is not a credential (measured arm D: it opens its
    /// own dialog, default `No`), and the refusal must SAY so — that belief is
    /// what the report shows costs an operator time.
    #[test]
    fn api_key_alone_still_refuses_and_names_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().to_string_lossy().into_owned();

        let err = check_tui_credentials(
            Some(&cfg),
            own_uid(),
            env_of(&[("ANTHROPIC_API_KEY", "sk-ant-api03-whatever")]),
            no_keychain,
        )
        .expect_err("must refuse");

        assert!(
            err.to_string().contains("ANTHROPIC_API_KEY"),
            "{}",
            err.to_string()
        );
    }

    /// No refusal message, at any point, may carry credential bytes. Asserted
    /// against a token-shaped env value and a file whose contents are a
    /// distinctive secret: neither may appear in the error or the remedy.
    #[test]
    fn no_refusal_message_carries_secret_material() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().to_string_lossy().into_owned();
        let path = dir.path().join(".credentials.json");
        std::fs::write(&path, "SECRET-MATERIAL-MUST-NOT-LEAK").expect("seed");
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .expect("chmod");

        let err = check_tui_credentials(Some(&cfg), own_uid() + 4242, env_of(&[]), no_keychain)
            .expect_err("must refuse");

        let rendered = format!("{err} {}", err.remedy());
        assert!(!rendered.contains("SECRET-MATERIAL"), "{rendered}");
    }

    /// The layout shift, pinned. With `CLAUDE_CONFIG_DIR` unset the credentials
    /// file is `$HOME/.claude/.credentials.json` — it follows `settings.json`
    /// into the `.claude/` directory, NOT `.claude.json` into `$HOME`.
    /// `$HOME/.credentials.json` is the obvious wrong guess.
    #[test]
    fn default_layout_puts_credentials_under_dot_claude() {
        let path = credentials_file(None, env_of(&[("HOME", "/home/op")])).expect("resolves");
        assert_eq!(path, Path::new("/home/op/.claude/.credentials.json"));
    }

    /// `CLAUDE_CONFIG_DIR` wins, and the file sits directly inside it.
    #[test]
    fn config_dir_beats_home() {
        let path = credentials_file(Some("/accounts/a"), env_of(&[("HOME", "/home/op")]))
            .expect("resolves");
        assert_eq!(path, Path::new("/accounts/a/.credentials.json"));
    }

    /// `CLAUDE_SECURESTORAGE_CONFIG_DIR` overrides even an explicit config dir,
    /// and an EMPTY value means `$HOME/.claude` rather than "unset" — the
    /// asymmetry the shipped binary has and a reimplementation would miss.
    #[test]
    fn securestorage_override_wins_and_empty_means_default() {
        assert_eq!(
            credentials_file(
                Some("/accounts/a"),
                env_of(&[("CLAUDE_SECURESTORAGE_CONFIG_DIR", "/vault")])
            )
            .expect("resolves"),
            Path::new("/vault/.credentials.json")
        );
        assert_eq!(
            credentials_file(
                Some("/accounts/a"),
                env_of(&[
                    ("CLAUDE_SECURESTORAGE_CONFIG_DIR", ""),
                    ("HOME", "/home/op")
                ])
            )
            .expect("resolves"),
            Path::new("/home/op/.claude/.credentials.json")
        );
    }

    /// Neither source set → refuse rather than guess. Probing the wrong path
    /// would report "no credential" for a provisioned worker.
    #[test]
    fn no_config_home_refuses() {
        let err = credentials_file(None, env_of(&[])).expect_err("refuses");
        assert!(matches!(err, LoginRefusal::UnknownConfigHome), "{err:?}");
    }

    /// The keychain service name is suffixed by the config dir's hash when one
    /// is selected and unsuffixed when none is — the property that keeps a
    /// multi-account fleet's items apart. Pinned against the derivation that was
    /// confirmed against a real keychain item.
    #[test]
    fn keychain_service_name_is_suffixed_per_config_dir() {
        let default = keychain_service_name(None, env_of(&[]));
        assert_eq!(default, "Claude Code-credentials");

        let a = keychain_service_name(Some("/accounts/a"), env_of(&[]));
        let b = keychain_service_name(Some("/accounts/b"), env_of(&[]));
        assert!(a.starts_with("Claude Code-credentials-"), "{a}");
        assert_eq!(a.len(), "Claude Code-credentials-".len() + 8);
        assert_ne!(a, b, "two accounts must not share one keychain item");
    }

    /// The account name is `$USER`, with a fixed fallback for an unset or
    /// non-identifier value — probing the wrong account is a false "absent",
    /// which is a false refusal.
    #[test]
    fn keychain_account_falls_back_for_exotic_user() {
        assert_eq!(keychain_account(env_of(&[("USER", "op")])), "op");
        assert_eq!(keychain_account(env_of(&[])), "claude-code-user");
        assert_eq!(
            keychain_account(env_of(&[("USER", "op with spaces")])),
            "claude-code-user"
        );
    }

    /// The production probe must never surface a secret. It cannot, by
    /// signature — but the flag it runs with is the load-bearing part, so the
    /// absence of `-w` is asserted here rather than left to a reader: on a host
    /// with no `security` binary and on one with it, the answer is a bool and
    /// the process's stdout is discarded.
    #[test]
    fn production_probe_returns_only_a_bool() {
        // A service name nothing can have. Whatever the host, this is `false`
        // and nothing was printed.
        assert!(!security_keychain_probe(
            "cosmon-nonexistent-service-8f2a1c",
            "cosmon-nonexistent-account-8f2a1c"
        ));
    }
}
