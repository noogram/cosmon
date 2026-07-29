// SPDX-License-Identifier: AGPL-3.0-only

//! Pre-grant Claude Code's three startup consent gates for an unattended worker
//! (COSMON-DEV issue #20, the `@jdthaler` container hang on v0.3.0).
//!
//! # The gates that stop a worker with nobody in front of it
//!
//! Claude Code asks three blocking, full-screen questions *before* it will
//! accept any work in a config directory / project it has not seen before:
//!
//! 0. the **first-run onboarding wizard** — `Let's get started.` /
//!    `Choose the text style that looks best with your terminal`, footed by
//!    `Syntax theme: Monokai Extended (ctrl+t to disable)`. It renders on any
//!    `CLAUDE_CONFIG_DIR` that has never completed onboarding, *before* the
//!    other two, and it is the screen the tester hit on Claude Code 2.1.220;
//! 1. the **folder-trust** dialog — `Quick safety check: Is this a project you
//!    created or one you trust?` with `❯ 1. Yes, I trust this folder`;
//! 2. the **bypass-permissions disclaimer** —
//!    `WARNING: Claude Code running in Bypass Permissions mode`, whose
//!    *default-highlighted* option is `1. No, exit`.
//!
//! A cosmon worker is a tmux pane nobody is looking at. Until this module
//! existed, cosmon answered both by *typing into the dialog* from
//! [`crate::readiness::wait_ready`] — a keystroke race against a TUI that is
//! still painting its first frame. When the keystroke is swallowed (a cold
//! arm64 container is exactly where that happens) the pane sits on the question
//! forever and the molecule stays `running` with nobody home. That is the
//! reported symptom.
//!
//! # `bypassPermissions` does **not** suppress the trust dialog
//!
//! Measured, not assumed (macOS, Claude Code 2.1.220, 2026-07-25): a fresh
//! `CLAUDE_CONFIG_DIR` whose `.claude.json` has `projects: {}`, launched as
//! `claude --permission-mode bypassPermissions` in an unseen directory, renders
//! the folder-trust dialog and waits. Folder trust is a property of the
//! *workspace*, orthogonal to the permission mode; no `--permission-mode`
//! value skips it. So the fix cannot be "make sure the worker really is in
//! bypass" — the trust has to be granted **before** the process starts.
//!
//! # Where Claude Code keeps the three answers
//!
//! In **two different files**, which is the trap this module exists to
//! remember. [`consent_paths`] resolves both.
//!
//! | gate | file | key |
//! |---|---|---|
//! | onboarding wizard | `<config dir>/.claude.json` | `hasCompletedOnboarding = true` |
//! | folder trust | `<config dir>/.claude.json` | `projects["<abs workspace>"].hasTrustDialogAccepted = true` |
//! | bypass disclaimer | `<config dir>/settings.json` | `skipDangerousModePermissionPrompt = true` |
//!
//! With `CLAUDE_CONFIG_DIR` set, both sit inside it. With it unset the config
//! file is `$HOME/.claude.json` while the settings file is
//! `$HOME/.claude/settings.json` — the asymmetry is Claude Code's, not ours.
//!
//! The disclaimer gate, decompiled from the shipped 2.1.220 binary, is
//! `if (D2()) return; if (PW() || Rt().bypassPermissionsModeAccepted) return;`
//! — where `PW()` reads `skipDangerousModePermissionPrompt` from user / local /
//! flag / policy settings and `Rt()` is the `.claude.json` accessor. The
//! `.claude.json` flag reads like the cheaper pre-grant and **is not**: measured
//! on 2.1.220, a config with `bypassPermissionsModeAccepted = true` still
//! renders the disclaimer (the flag is legacy, kept only for a one-way
//! migration into settings that does not run before the gate). Only the
//! settings key suppresses it. The writes below are exactly the ones measured
//! to work; the set was verified end to end — onboarding and trust pre-granted
//! plus the settings key, `claude --permission-mode bypassPermissions` in an
//! unseen directory and a pristine config dir boots straight to the composer
//! with no dialog at all.
//!
//! # `.claude.json` is not durable storage — re-assert on every spawn
//!
//! This is the load-bearing property of the whole module, and it is the one a
//! reader is most likely to assume the other way round.
//!
//! Claude Code does not *edit* `.claude.json`. It reads it at startup, keeps
//! the result in memory, and **rewrites the whole file from that in-memory
//! state** — dropping every key the running build does not recognise. Measured
//! on 2.1.220 (macOS, native install, 2026-07-26), one graceful TUI lifecycle
//! over a config seeded with four keys:
//!
//! | key seeded in `.claude.json` | after the process exits |
//! |---|---|
//! | `hasCompletedOnboarding: true` | **survives** |
//! | `projects[ws].hasTrustDialogAccepted: true` | **survives** |
//! | `theme: "dark"` | **stripped** |
//! | `bypassPermissionsModeAccepted: true` | **stripped** |
//!
//! `settings.json` was not touched at all, in the same run.
//!
//! Two keys survived *today*. That is a fact about 2.1.220's key list, not a
//! guarantee — the two that did not survive are exactly the two an earlier
//! version did recognise, so the survivor set is a moving target. The tester on
//! issue #20 measured `hasCompletedOnboarding` going to `null` across the same
//! cycle on his bench: same mechanism, different build state, opposite outcome
//! for that key. **A pre-grant written once may therefore be gone before the
//! next dispatch, and cosmon has no way to know which keys this build keeps.**
//!
//! There is no durable channel to move it to, which was measured too rather
//! than assumed:
//!
//! - `settings.json` — never rewritten by Claude Code, but it does **not**
//!   honour `hasCompletedOnboarding`: a config with the key in `settings.json`
//!   and absent from `.claude.json` still renders the wizard. Not an option.
//! - `claude config set -g hasCompletedOnboarding true` — the supported CLI
//!   for this is gone: `error: unknown option '-g'` on 2.1.220.
//! - an environment variable — none is exposed for onboarding state.
//!
//! So the answer is the third one, and it is a design property, not a
//! workaround: **[`pregrant_startup_consent`] is called before *every* spawn
//! and re-reads both files each time**, so a key a previous worker dropped is
//! simply re-written before the next worker starts. Idempotence is what makes
//! this cheap ([`ConsentPregrant::AlreadyGranted`] touches nothing), and
//! re-assertion is what makes it correct. Nothing in this module may be
//! "optimised" into a run-once install step; the acceptance test for it is two
//! *consecutive* dispatches on a pristine config dir, because a single green
//! dispatch cannot distinguish the two designs. See
//! `tests/claude_consent_live.rs`.
//!
//! # A pre-grant does **not** need a "matured" config directory
//!
//! Recorded because the opposite is a reasonable guess, it was seriously held,
//! and re-deriving it costs an afternoon.
//!
//! **This section is a recorded measurement, not a falsifier.** Nothing below
//! goes red on its own: it is a table of observed screens plus a script an
//! operator re-runs by hand. The only automated check anywhere near it is
//! `tests/claude_consent_live.rs`, and that one grades the *installed binary*
//! and refuses to run without it. Said in the negative because the distinction
//! was lost once already — the commit carrying this section was reviewed as a
//! falsifier and contains zero assertions, so a later reader inherited a
//! confidence the code never offered. A measurement earns its keep by saving
//! the afternoon; it does not earn the word "test".
//!
//! Issue #20's tester reached a composer exactly once in ~20 spawns, on a config
//! directory that some four prior `claude -p` runs had filled in (`machineID`,
//! `userID`, migration flags), and never from a fresh one. That supports a
//! specific reading: Claude Code treats a virgin directory as a first run,
//! rewrites it wholesale from memory before deciding what to render, and
//! destroys any consent seeded into it — so cosmon would have to *mature* the
//! directory (one throwaway `claude` invocation) before pre-granting.
//!
//! Measured instead, macOS 26.5.2 / arm64 / 2.1.220, three arms over one
//! workspace, graded by [`crate::readiness::classify_output`] rather than by eye:
//!
//! | config dir at launch | screen | classified |
//! |---|---|---|
//! | virgin, no pre-grant | first-run theme wizard | `Loading` — refused |
//! | virgin, **pre-grant only, never matured** | **composer** | **`Ready`** |
//! | matured by `claude -p`, then pre-grant | composer | `Ready` |
//!
//! The middle row is the answer: **maturation is not a precondition**, and the
//! bottom row buys nothing over it. The pre-grant also survives a maturing run
//! written over it, and survives a zero-gap teardown-and-relaunch (260/260
//! samples at 0.1 s). So no maturation step exists in the spawn path, and one
//! must not be added on the strength of the correlation above: it would cost a
//! subprocess with a timeout and a new typed failure per dispatch — a new door
//! of exactly the kind this issue is about — to fix something not measured to
//! be broken.
//!
//! Two things this does *not* license. Maturation is orthogonal to onboarding,
//! not a weaker form of it: a maturing run writes `machineID` and `userID` and
//! does **not** write `hasCompletedOnboarding`, so nothing here says a directory
//! can be pre-granted by using it. And the measurements are macOS-native; the
//! tester's bed is a Linux container, which is the axis in question.
//! `scripts/claude-pregrant-discriminator.sh` re-runs the three arms on any bed
//! with no credential involved, and `docs/benches/issue-20-maturation.md` holds
//! the raw panes and the half of the proof that needs an operator's credential.
//!
//! # Why cosmon pre-grants the wizard instead of answering it
//!
//! §8v (ADR-162) says a screen cosmon cannot certify is refused, and that
//! cosmon does not drift into piloting onboarding. Pre-granting the onboarding
//! key does not touch that boundary in either direction:
//!
//! - the classifier is unchanged — no marker is added, no new screen becomes
//!   `Ready`, and a wizard that renders anyway is still refused with the pane
//!   quoted. The closed default stands exactly as ratified;
//! - what changes is upstream of the classifier: the wizard is not *answered*,
//!   it is not *rendered*. Writing consent into a config file is the same act
//!   as the folder-trust pre-grant §8v was ratified alongside — a file write
//!   before the process exists, not a keystroke into a live TUI.
//!
//! The alternative — teaching the readiness loop to select a theme — is the one
//! §8v forbids, and it fails on its own terms too: every startup screen this
//! TUI paints is a menu, so naming the fifth door leaves the sixth open. That
//! is the corridor issue #20 walked through four times.
//!
//! # The footprint, stated plainly
//!
//! `skipDangerousModePermissionPrompt` is a **user-scope** setting, and when the
//! operator exports `CLAUDE_CONFIG_DIR` (the `claude-account` / `cb` layout) the
//! worker's config dir is the same one their own interactive sessions read. So
//! this write is visible outside the fleet: that operator stops seeing the
//! bypass-mode disclaimer in their own `claude` sessions too.
//!
//! There is no narrower place to put it — the per-config `.claude.json` twin is
//! ignored (above) — and the practical cost is nil, because the disclaimer is
//! one-time consent the operator has necessarily already given for that account.
//! It is recorded here rather than left for someone to discover: a module that
//! edits the operator's settings should say so out loud. The trust key, by
//! contrast, is scoped to the one worktree path.
//!
//! # Fail-closed
//!
//! [`pregrant_startup_consent`] returns `Err` when it cannot leave the config
//! file in the pre-granted state. Callers must **refuse the spawn** on that
//! error rather than launch anyway: a worker that hangs mutely on a dialog is
//! strictly worse than a dispatch that fails with a reason, because the mute
//! one consumes a molecule slot and looks healthy. This asymmetry is the whole
//! design rule of the module.

use std::path::{Path, PathBuf};

/// Why a startup-consent pre-grant could not be completed.
///
/// Every variant is a spawn-refusal cause: the caller has no safe way to
/// continue, because the worker it is about to launch would stop on a dialog
/// nobody can answer.
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// Neither `CLAUDE_CONFIG_DIR` nor `HOME` is set, so the config file Claude
    /// Code will read cannot be named — there is nothing to pre-grant.
    #[error(
        "cannot locate Claude Code's config file: neither CLAUDE_CONFIG_DIR nor HOME is set in \
         the dispatcher environment"
    )]
    UnknownConfigHome,

    /// The workspace path could not be made absolute. Claude Code keys folder
    /// trust on the absolute directory it resolves at startup, so a relative
    /// key would silently fail to match.
    #[error("cannot resolve workspace {path} to an absolute path: {source}")]
    UnresolvableWorkspace {
        /// The workspace directory as handed in.
        path: String,
        /// The underlying resolution failure.
        source: std::io::Error,
    },

    /// The existing config file could not be read.
    #[error("cannot read Claude Code config {path}: {source}")]
    Read {
        /// The config file path.
        path: String,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The existing config file is not a JSON object. Overwriting it would
    /// destroy operator state (accounts, MCP servers, per-project history), so
    /// this refuses instead.
    #[error("Claude Code config {path} is not a JSON object; refusing to overwrite it")]
    NotAnObject {
        /// The config file path.
        path: String,
    },

    /// The existing config file is unparseable JSON — same refusal as
    /// [`Self::NotAnObject`], for the same reason.
    #[error("Claude Code config {path} is not valid JSON: {source}")]
    Parse {
        /// The config file path.
        path: String,
        /// The underlying deserialization failure.
        source: serde_json::Error,
    },

    /// The pre-granted config could not be written back.
    #[error("cannot write Claude Code config {path}: {source}")]
    Write {
        /// The config file path.
        path: String,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
}

/// What [`pregrant_startup_consent`] had to change.
///
/// Reported so a caller can log the difference between "cosmon granted the
/// trust" and "the operator's config already had it" without re-reading the
/// file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentPregrant {
    /// At least one of the two flags was missing and has been written.
    Granted,
    /// Both flags were already set; the file was not touched.
    AlreadyGranted,
}

/// The pair of files [`pregrant_startup_consent`] writes.
///
/// Kept as one value because the two paths are resolved from the same inputs
/// and must never drift apart: pre-granting trust without the disclaimer key
/// (or vice versa) still leaves the worker stopped on a dialog, so the caller
/// should not be able to hold one without the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentPaths {
    /// `<config dir>/.claude.json` — holds the per-project folder trust flag.
    pub config_file: PathBuf,
    /// `<config dir>/settings.json` — holds the disclaimer-skip flag Claude
    /// Code actually honours (see the module docs on why the `.claude.json`
    /// twin of this flag does not work).
    pub settings_file: PathBuf,
}

/// Resolve the two consent files for a worker, given its config dir and an
/// environment.
///
/// `config_dir` is the resolved `CLAUDE_CONFIG_DIR` when the spawn path has one
/// (cosmon resolves it per worker for multi-account fleets), in which case both
/// files sit inside it. `None` falls back to Claude Code's default layout, whose
/// two halves live in *different* directories: `$HOME/.claude.json` and
/// `$HOME/.claude/settings.json`. `env_lookup` is injected so this stays
/// testable without mutating the process environment.
///
/// # Errors
///
/// [`TrustError::UnknownConfigHome`] when neither source names a directory —
/// there is no defensible guess, and a pre-grant written to the wrong file
/// would report success while the worker still hangs.
pub fn consent_paths<F>(config_dir: Option<&str>, env_lookup: F) -> Result<ConsentPaths, TrustError>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(dir) = config_dir.filter(|d| !d.is_empty()) {
        let dir = Path::new(dir);
        return Ok(ConsentPaths {
            config_file: dir.join(".claude.json"),
            settings_file: dir.join("settings.json"),
        });
    }
    match env_lookup("HOME").filter(|h| !h.is_empty()) {
        Some(home) => {
            let home = Path::new(&home);
            Ok(ConsentPaths {
                config_file: home.join(".claude.json"),
                settings_file: home.join(".claude").join("settings.json"),
            })
        }
        None => Err(TrustError::UnknownConfigHome),
    }
}

/// Pre-grant the first-run onboarding wizard, folder trust for `workspace`, and
/// the bypass-permissions disclaimer, in the two files named by `paths`.
///
/// Read-modify-write on each file: every key Claude Code (or the operator)
/// already wrote is preserved; only `hasCompletedOnboarding` and
/// `projects.<workspace>.hasTrustDialogAccepted` in the config file and
/// `skipDangerousModePermissionPrompt` in the settings file are asserted. A
/// missing file is created with just those keys — Claude Code fills the rest at
/// startup.
///
/// **Call this before every spawn.** The config file is rewritten wholesale by
/// each Claude Code process from its own in-memory state, so a pre-grant is not
/// storage that stays put; see the module docs.
///
/// Each write is atomic (temp file in the same directory, then rename), so a
/// crash mid-write cannot leave the operator with a truncated `.claude.json` —
/// the file that also holds their account and MCP state.
///
/// # Errors
///
/// Any [`TrustError`]. Callers **must** treat an error as a spawn refusal; see
/// the module docs on fail-closed.
pub fn pregrant_startup_consent(
    paths: &ConsentPaths,
    workspace: &Path,
) -> Result<ConsentPregrant, TrustError> {
    // Claude Code keys trust on the absolute directory it resolves at startup;
    // `canonicalize` also resolves the symlinks a container bind-mount adds, so
    // the key matches what the worker process will actually look up.
    let workspace_key = std::fs::canonicalize(workspace)
        .map_err(|source| TrustError::UnresolvableWorkspace {
            path: workspace.to_string_lossy().into_owned(),
            source,
        })?
        .to_string_lossy()
        .into_owned();

    let config = grant_onboarding_and_trust(&paths.config_file, workspace_key)?;
    let disclaimer = grant_disclaimer_skip(&paths.settings_file)?;

    // "Already granted" only when BOTH files were already in place: a caller
    // logging `AlreadyGranted` must be able to read it as "nothing to do", and
    // one of the gates being open is not that.
    Ok(match (config, disclaimer) {
        (ConsentPregrant::AlreadyGranted, ConsentPregrant::AlreadyGranted) => {
            ConsentPregrant::AlreadyGranted
        }
        _ => ConsentPregrant::Granted,
    })
}

/// Assert both `.claude.json` keys — `hasCompletedOnboarding` at the root and
/// `projects.<workspace_key>.hasTrustDialogAccepted` — in one read-modify-write.
///
/// One pass, not two, because they live in the same file: writing it twice
/// would double the window in which a concurrently-exiting Claude Code process
/// could interleave, for no gain.
fn grant_onboarding_and_trust(
    config_path: &Path,
    workspace_key: String,
) -> Result<ConsentPregrant, TrustError> {
    let mut root = read_json_object(config_path)?;
    let not_an_object = || TrustError::NotAnObject {
        path: config_path.to_string_lossy().into_owned(),
    };

    // The onboarding wizard is gate 0: it renders *before* the trust dialog, so
    // pre-granting trust without it leaves the worker parked one screen earlier
    // — which is precisely the 2.1.220 report this key answers.
    let onboarding_missing = root.get(ONBOARDING_KEY) != Some(&serde_json::Value::Bool(true));
    if onboarding_missing {
        root.insert(ONBOARDING_KEY.to_owned(), serde_json::Value::Bool(true));
    }

    // `projects` is an object keyed by absolute directory. A non-object value
    // there is corrupt config, not a shape to merge into.
    let projects = root
        .entry(PROJECTS_KEY.to_owned())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(not_an_object)?;
    let entry = projects
        .entry(workspace_key)
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(not_an_object)?;

    let trust_missing = entry.get(TRUST_KEY) != Some(&serde_json::Value::Bool(true));
    if trust_missing {
        entry.insert(TRUST_KEY.to_owned(), serde_json::Value::Bool(true));
    }

    if !onboarding_missing && !trust_missing {
        return Ok(ConsentPregrant::AlreadyGranted);
    }
    write_json_atomically(config_path, &serde_json::Value::Object(root))?;
    Ok(ConsentPregrant::Granted)
}

/// Assert `skipDangerousModePermissionPrompt = true` in the `settings.json` at
/// `settings_path` — the flag measured to suppress the bypass disclaimer.
fn grant_disclaimer_skip(settings_path: &Path) -> Result<ConsentPregrant, TrustError> {
    let mut root = read_json_object(settings_path)?;
    if root.get(SKIP_DISCLAIMER_KEY) == Some(&serde_json::Value::Bool(true)) {
        return Ok(ConsentPregrant::AlreadyGranted);
    }
    root.insert(
        SKIP_DISCLAIMER_KEY.to_owned(),
        serde_json::Value::Bool(true),
    );
    write_json_atomically(settings_path, &serde_json::Value::Object(root))?;
    Ok(ConsentPregrant::Granted)
}

/// The `.claude.json` root flag that suppresses the first-run onboarding
/// wizard. Root-scoped, not per-project: onboarding is a property of the config
/// directory, so one grant covers every workspace that config dir ever opens.
///
/// Measured on 2.1.220: this key alone skips the wizard — no `theme` key is
/// required anywhere, and the same key placed in `settings.json` instead does
/// nothing at all (the wizard still renders).
const ONBOARDING_KEY: &str = "hasCompletedOnboarding";
/// The `.claude.json` key holding per-project state, keyed by absolute dir.
const PROJECTS_KEY: &str = "projects";
/// The per-project folder-trust flag Claude Code checks before the dialog.
const TRUST_KEY: &str = "hasTrustDialogAccepted";
/// The settings flag that suppresses the bypass-permissions disclaimer. Lives
/// in `settings.json`, NOT in `.claude.json` — see the module docs.
const SKIP_DISCLAIMER_KEY: &str = "skipDangerousModePermissionPrompt";

/// Read `config_path` as a JSON object, or an empty object when it is absent.
///
/// An absent file is the normal first-spawn case in a fresh container; an
/// unparseable or non-object one is operator state we refuse to clobber.
fn read_json_object(
    config_path: &Path,
) -> Result<serde_json::Map<String, serde_json::Value>, TrustError> {
    let raw = match std::fs::read_to_string(config_path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(serde_json::Map::new()),
        Err(source) => {
            return Err(TrustError::Read {
                path: config_path.to_string_lossy().into_owned(),
                source,
            })
        }
    };
    // A zero-byte config is what a killed `claude` leaves behind (the
    // `.claude.json.tmp.*` files next to a real one are the same artefact);
    // treat it as absent rather than as a parse failure that refuses a spawn.
    if raw.trim().is_empty() {
        return Ok(serde_json::Map::new());
    }
    match serde_json::from_str(&raw) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        Ok(_) => Err(TrustError::NotAnObject {
            path: config_path.to_string_lossy().into_owned(),
        }),
        Err(source) => Err(TrustError::Parse {
            path: config_path.to_string_lossy().into_owned(),
            source,
        }),
    }
}

/// Write `value` to `config_path` via a temp file in the same directory plus a
/// rename, so a reader never observes a partial config.
fn write_json_atomically(config_path: &Path, value: &serde_json::Value) -> Result<(), TrustError> {
    let err = |source: std::io::Error| TrustError::Write {
        path: config_path.to_string_lossy().into_owned(),
        source,
    };
    let dir = config_path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(err)?;
    let serialized = serde_json::to_vec(value).map_err(|e| TrustError::Write {
        path: config_path.to_string_lossy().into_owned(),
        source: std::io::Error::other(e),
    })?;
    // `tempfile` in the same directory keeps the rename on one filesystem.
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(err)?;
    std::io::Write::write_all(&mut tmp, &serialized).map_err(err)?;
    tmp.persist(config_path)
        .map_err(|e| TrustError::Write {
            path: config_path.to_string_lossy().into_owned(),
            source: e.error,
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `ConsentPaths` rooted at `dir`, the `CLAUDE_CONFIG_DIR` shape.
    fn paths_in(dir: &Path) -> ConsentPaths {
        consent_paths(Some(&dir.to_string_lossy()), |_| None).expect("resolves")
    }

    /// Read a JSON file back, panicking with the path on failure.
    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_str(
            &std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}")),
        )
        .unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
    }

    /// The container symptom, as a unit test: a fresh config dir and an unseen
    /// workspace must come out of the pre-grant with *both* startup gates
    /// answered, so the worker never renders either dialog.
    ///
    /// The two assertions are deliberately in different files. Asserting only
    /// the trust key would have passed against the first draft of this module,
    /// which wrote the legacy `.claude.json` disclaimer flag that Claude Code
    /// 2.1.220 ignores — a green test over a worker still stopped on the
    /// disclaimer.
    #[test]
    fn fresh_config_gets_both_gates_pregranted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path().join("worktree");
        std::fs::create_dir(&ws).expect("mkdir");
        let paths = paths_in(&dir.path().join("cfg"));

        let outcome = pregrant_startup_consent(&paths, &ws).expect("pre-grant succeeds");
        assert_eq!(outcome, ConsentPregrant::Granted);

        let key = std::fs::canonicalize(&ws)
            .expect("canonicalize")
            .to_string_lossy()
            .into_owned();
        let cfg = read_json(&paths.config_file);
        assert_eq!(cfg["hasCompletedOnboarding"], true);
        assert_eq!(cfg["projects"][&key]["hasTrustDialogAccepted"], true);
        assert_eq!(
            read_json(&paths.settings_file)["skipDangerousModePermissionPrompt"],
            true
        );
    }

    /// The gate the 2.1.220 report added: a config dir that has never completed
    /// onboarding opens on the theme wizard, one screen *before* the trust
    /// dialog. Trust alone therefore pre-grants nothing observable — the worker
    /// still stops, just earlier.
    #[test]
    fn onboarding_is_granted_even_when_trust_already_is() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path().join("worktree");
        std::fs::create_dir(&ws).expect("mkdir");
        let paths = paths_in(dir.path());
        let key = std::fs::canonicalize(&ws)
            .expect("canonicalize")
            .to_string_lossy()
            .into_owned();
        std::fs::write(
            &paths.config_file,
            serde_json::json!({"projects": {key.clone(): {"hasTrustDialogAccepted": true}}})
                .to_string(),
        )
        .expect("seed");

        assert_eq!(
            pregrant_startup_consent(&paths, &ws).expect("pre-grant"),
            ConsentPregrant::Granted
        );
        assert_eq!(
            read_json(&paths.config_file)["hasCompletedOnboarding"],
            true
        );
    }

    /// **The durability contract.** Claude Code rewrites `.claude.json` from its
    /// own in-memory state on exit and drops keys the running build does not
    /// recognise — measured on 2.1.220, where `theme` and
    /// `bypassPermissionsModeAccepted` vanished across one graceful lifecycle
    /// while other keys survived. A pre-grant is therefore not storage: it is an
    /// assertion that must be re-made before every spawn.
    ///
    /// This test simulates the worst case — the config comes back with *both*
    /// granted keys stripped and the rest of the operator's state intact — and
    /// pins that the next pre-grant restores them without a human in the loop.
    /// It is the hermetic half of `tests/claude_consent_live.rs`, which proves
    /// the same property against the installed binary.
    #[test]
    fn a_stripped_config_is_re_granted_by_the_next_spawn() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path().join("worktree");
        std::fs::create_dir(&ws).expect("mkdir");
        let paths = paths_in(dir.path());
        let key = std::fs::canonicalize(&ws)
            .expect("canonicalize")
            .to_string_lossy()
            .into_owned();

        pregrant_startup_consent(&paths, &ws).expect("first spawn");

        // What a Claude Code process leaves behind when it drops both keys.
        std::fs::write(
            &paths.config_file,
            serde_json::json!({
                "numStartups": 1,
                "projects": {key.clone(): {"lastSessionId": "…"}},
            })
            .to_string(),
        )
        .expect("simulate the rewrite");

        assert_eq!(
            pregrant_startup_consent(&paths, &ws).expect("second spawn"),
            ConsentPregrant::Granted,
            "a stripped config must be detected as needing the grant again"
        );
        let cfg = read_json(&paths.config_file);
        assert_eq!(cfg["hasCompletedOnboarding"], true);
        assert_eq!(cfg["projects"][&key]["hasTrustDialogAccepted"], true);
        // The state the process wrote in between is not collateral damage.
        assert_eq!(cfg["numStartups"], 1);
        assert_eq!(cfg["projects"][&key]["lastSessionId"], "…");
    }

    /// Read-modify-write, not overwrite: the pre-grant must not cost the
    /// operator their account, MCP servers, another project's trust, or their
    /// settings — the two files it touches hold all of that.
    #[test]
    fn existing_operator_state_survives_the_pregrant() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path().join("worktree");
        std::fs::create_dir(&ws).expect("mkdir");
        let paths = paths_in(dir.path());
        std::fs::write(
            &paths.config_file,
            serde_json::json!({
                "oauthAccount": {"emailAddress": "op@example.invalid"},
                "mcpServers": {"neurion": {"command": "neurion"}},
                "projects": {"/somewhere/else": {"hasTrustDialogAccepted": true}},
            })
            .to_string(),
        )
        .expect("seed config");
        std::fs::write(
            &paths.settings_file,
            serde_json::json!({"model": "claude-opus-5", "env": {"FOO": "bar"}}).to_string(),
        )
        .expect("seed settings");

        pregrant_startup_consent(&paths, &ws).expect("pre-grant succeeds");

        let cfg = read_json(&paths.config_file);
        assert_eq!(cfg["oauthAccount"]["emailAddress"], "op@example.invalid");
        assert_eq!(cfg["mcpServers"]["neurion"]["command"], "neurion");
        assert_eq!(
            cfg["projects"]["/somewhere/else"]["hasTrustDialogAccepted"],
            true
        );
        let settings = read_json(&paths.settings_file);
        assert_eq!(settings["model"], "claude-opus-5");
        assert_eq!(settings["env"]["FOO"], "bar");
        assert_eq!(settings["skipDangerousModePermissionPrompt"], true);
    }

    /// Idempotent: a second spawn into the same worktree reports
    /// `AlreadyGranted` and leaves both files byte-identical, so the pre-grant
    /// cannot churn the operator's config once per dispatch.
    #[test]
    fn second_pregrant_is_a_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path().join("worktree");
        std::fs::create_dir(&ws).expect("mkdir");
        let paths = paths_in(dir.path());

        pregrant_startup_consent(&paths, &ws).expect("first");
        let config_after_first = std::fs::read(&paths.config_file).expect("read");
        let settings_after_first = std::fs::read(&paths.settings_file).expect("read");

        let outcome = pregrant_startup_consent(&paths, &ws).expect("second");

        assert_eq!(outcome, ConsentPregrant::AlreadyGranted);
        assert_eq!(
            config_after_first,
            std::fs::read(&paths.config_file).expect("read")
        );
        assert_eq!(
            settings_after_first,
            std::fs::read(&paths.settings_file).expect("read")
        );
    }

    /// A half-granted state must still report `Granted`, never `AlreadyGranted`:
    /// one open gate out of two is exactly the state that hangs a worker, so it
    /// must not read back as "nothing to do".
    #[test]
    fn trust_already_set_but_disclaimer_missing_reports_granted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path().join("worktree");
        std::fs::create_dir(&ws).expect("mkdir");
        let paths = paths_in(dir.path());
        let key = std::fs::canonicalize(&ws)
            .expect("canonicalize")
            .to_string_lossy()
            .into_owned();
        std::fs::write(
            &paths.config_file,
            serde_json::json!({"projects": {key: {"hasTrustDialogAccepted": true}}}).to_string(),
        )
        .expect("seed");

        assert_eq!(
            pregrant_startup_consent(&paths, &ws).expect("pre-grant"),
            ConsentPregrant::Granted
        );
        assert_eq!(
            read_json(&paths.settings_file)["skipDangerousModePermissionPrompt"],
            true
        );
    }

    /// Fail-closed on corrupt operator state: refusing the spawn is the correct
    /// outcome, because the alternative is either clobbering the config or
    /// launching a worker that will stop on the dialog.
    #[test]
    fn unparseable_config_refuses_instead_of_clobbering() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path().join("worktree");
        std::fs::create_dir(&ws).expect("mkdir");
        let paths = paths_in(dir.path());
        std::fs::write(&paths.config_file, "{ not json").expect("seed");

        let err = pregrant_startup_consent(&paths, &ws).expect_err("must refuse");
        assert!(matches!(err, TrustError::Parse { .. }), "{err:?}");
        // The corrupt bytes are still there — we did not "fix" it by truncating.
        assert_eq!(
            std::fs::read_to_string(&paths.config_file).expect("read"),
            "{ not json"
        );
    }

    /// The same refusal for a corrupt `settings.json`. Trust may already have
    /// been written when this fires; that is harmless (trust alone changes no
    /// behaviour) and the caller still refuses the spawn.
    #[test]
    fn unparseable_settings_refuses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path().join("worktree");
        std::fs::create_dir(&ws).expect("mkdir");
        let paths = paths_in(dir.path());
        std::fs::write(&paths.settings_file, "[]").expect("seed");

        let err = pregrant_startup_consent(&paths, &ws).expect_err("must refuse");
        assert!(matches!(err, TrustError::NotAnObject { .. }), "{err:?}");
    }

    /// A zero-byte `.claude.json` is a killed-`claude` artefact, not corruption:
    /// it must be treated as absent so a container that lost a worker mid-write
    /// can still dispatch the next one.
    #[test]
    fn empty_config_is_treated_as_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path().join("worktree");
        std::fs::create_dir(&ws).expect("mkdir");
        let paths = paths_in(dir.path());
        std::fs::write(&paths.config_file, "").expect("seed");

        assert_eq!(
            pregrant_startup_consent(&paths, &ws).expect("pre-grant succeeds"),
            ConsentPregrant::Granted
        );
    }

    /// A workspace that does not exist cannot be canonicalized, and a relative
    /// or non-existent key would silently fail to match what the worker looks
    /// up — so this refuses rather than writing a key that grants nothing.
    #[test]
    fn missing_workspace_refuses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_in(dir.path());
        let err = pregrant_startup_consent(&paths, &dir.path().join("nope")).expect_err("refuses");
        assert!(
            matches!(err, TrustError::UnresolvableWorkspace { .. }),
            "{err:?}"
        );
    }

    /// `CLAUDE_CONFIG_DIR` wins over `HOME`, and puts BOTH files inside it —
    /// that is what makes the pre-grant land in the same per-account config the
    /// worker will actually read on a multi-account fleet.
    #[test]
    fn config_dir_beats_home_for_both_files() {
        let paths = consent_paths(Some("/accounts/a"), |k| {
            (k == "HOME").then(|| "/home/op".to_owned())
        })
        .expect("resolves");
        assert_eq!(paths.config_file, Path::new("/accounts/a/.claude.json"));
        assert_eq!(paths.settings_file, Path::new("/accounts/a/settings.json"));
    }

    /// With no `CLAUDE_CONFIG_DIR` the two files live in different directories:
    /// `$HOME/.claude.json` and `$HOME/.claude/settings.json`. Collapsing them
    /// into one directory is the obvious wrong guess, so it is pinned here.
    #[test]
    fn default_layout_splits_the_two_files() {
        let paths = consent_paths(None, |k| (k == "HOME").then(|| "/home/op".to_owned()))
            .expect("resolves");
        assert_eq!(paths.config_file, Path::new("/home/op/.claude.json"));
        assert_eq!(
            paths.settings_file,
            Path::new("/home/op/.claude/settings.json")
        );
    }

    /// Neither source set → refuse. There is no defensible guess: writing to
    /// the wrong file would report success while the worker still hangs.
    #[test]
    fn no_config_home_refuses() {
        let err = consent_paths(None, |_| None).expect_err("refuses");
        assert!(matches!(err, TrustError::UnknownConfigHome), "{err:?}");
    }

    /// An empty `CLAUDE_CONFIG_DIR` (an operator `export CLAUDE_CONFIG_DIR=`)
    /// must be treated as unset, not as the relative path `.claude.json` — the
    /// same empty-value guard the spawn env prefix applies.
    #[test]
    fn empty_config_dir_is_treated_as_unset() {
        let paths = consent_paths(Some(""), |k| (k == "HOME").then(|| "/home/op".to_owned()))
            .expect("resolves");
        assert_eq!(paths.config_file, Path::new("/home/op/.claude.json"));
    }
}
