// SPDX-License-Identifier: AGPL-3.0-only

//! `cs doctor supervision` — detect double-supervision drift.
//!
//! ## Why this probe exists
//!
//! Cosmon owns a **single source of truth for process supervision**: the
//! pair of config files `~/.config/cosmon/patrols.toml` (short-lived
//! cron-like gestures, fired by `cosmon-scheduler`) and
//! `~/.config/cosmon/daemons.toml` (long-running dogs, kept alive by
//! `cosmon-daemon-supervisor`). When a binary is migrated *off* a legacy
//! macOS LaunchAgent and *onto* one of these tablets, the contract is:
//! the binary is supervised **here, and nowhere else**. The LaunchAgent
//! `.plist` must be unloaded and removed.
//!
//! ## The incident this probe was born from (2026-06-20)
//!
//! `mailroom-sync` was migrated to a cosmon patrol on 2026-04-19, but
//! the live `~/Library/LaunchAgents/com.you.mailroom-sync.plist` was
//! never `launchctl unload`ed nor deleted — the migration archived a *repo
//! copy* and added the patrol stanza, but left the real installed plist on
//! disk and loaded. For two months the double-supervision was silent
//! (both cadences were 900 s, so the duplicate ticks looked like one). On
//! 2026-06-20 a session editing the LaunchAgent's `StartInterval` (900 →
//! 300) made the two schedulers diverge, exposing the latent fork. The
//! root cause was not a *creator* — no script or doctor *re-wrote* the
//! plist; the plist had survived the migration. The structural gap was:
//! **nothing cross-referenced the LaunchAgents directory against the
//! cosmon supervision roster**, so a binary owned by two schedulers at
//! once was invisible until its cadences diverged.
//!
//! ## What this probe does
//!
//! Read-only. It loads the cosmon roster (patrols + daemons), scans
//! `~/Library/LaunchAgents/com.you.*.plist` (and the system
//! `/Library/LaunchAgents`), and flags — as a blocking `Error` — any
//! LaunchAgent whose program binary (or `com.you.<name>` label) is
//! *also* declared in the roster. A binary belongs to exactly one
//! supervisor; an overlap is the "retired-but-resurrected" plist.
//!
//! This is the cosmon-side, DRY enforcement of the rule "a binary migrated
//! to patrol/daemon MUST NOT carry a LaunchAgent": the roster files are
//! the registry, no separate ledger is invented (`docs/architectural-invariants.md`
//! §8b — *propose mechanisms of verification, do not impose them*). The
//! companion install-time refusal (an install script declining to write a
//! plist for a roster binary) lives in each host galaxy's install tooling;
//! this probe is the federation-wide detector that the security patrol/CI
//! can run so the drift can never re-accumulate silently.

// "LaunchAgent" / "LaunchAgents" / "StartInterval" are Apple proper nouns,
// not code identifiers; backticking them in prose reads as code. Allow the
// doc-markdown lint for this module rather than mis-typeset product names.
#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_scheduler::environment::shellexpand_home;

use super::findings::{Finding, ProbeReport, Severity};
use super::Context;

const PROBE: &str = "supervision";

/// The macOS LaunchAgent label prefix this house uses.
const LABEL_PREFIX: &str = "com.you.";

/// Arguments for `cs doctor supervision`.
///
/// All three inputs default to the canonical cosmon locations; the
/// overrides exist so the probe is exercisable end-to-end in tests
/// (point them at fixture files / a temp `LaunchAgents` dir).
#[derive(clap::Args, Default)]
pub struct Args {
    /// Override the patrols config (`~/.config/cosmon/patrols.toml`).
    #[arg(long)]
    pub patrols: Option<PathBuf>,
    /// Override the daemons config (`~/.config/cosmon/daemons.toml`).
    #[arg(long)]
    pub daemons: Option<PathBuf>,
    /// Override the LaunchAgents directory to scan.
    #[arg(long)]
    pub launch_agents_dir: Option<PathBuf>,
}

/// One binary that cosmon's scheduler or daemon-supervisor owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisedBinary {
    /// Patrol / daemon `name` from the config.
    pub name: String,
    /// `"patrol"` or `"daemon"` — which tablet declares it.
    pub source: &'static str,
    /// Basename of the supervised binary (e.g. `mailroom-sync`).
    pub binary: String,
    /// Whether the entry is currently `enabled`.
    pub enabled: bool,
}

/// One installed LaunchAgent `.plist` on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledAgent {
    /// The plist `Label` (e.g. `com.you.mailroom-sync`).
    pub label: String,
    /// Path to the `.plist` file.
    pub plist_path: PathBuf,
    /// Basename of `ProgramArguments[0]`, if readable.
    pub program: Option<String>,
}

/// Final path component of a program/binary path, as an owned `String`.
fn basename(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map_or_else(|| p.to_string(), |s| s.to_string_lossy().into_owned())
}

/// Shared script interpreters whose basename does not identify a single
/// supervised unit. A patrol running `bash <script>` and an unrelated
/// LaunchAgent also running `bash <other-script>` share a basename but are
/// not the same process — matching on these would be a false positive. For
/// these, only the `com.you.<name>` label match counts.
fn is_generic_interpreter(binary: &str) -> bool {
    matches!(
        binary,
        "bash"
            | "sh"
            | "zsh"
            | "dash"
            | "fish"
            | "env"
            | "python"
            | "python3"
            | "ruby"
            | "node"
            | "perl"
            | "osascript"
    )
}

/// **Pure core.** Cross-reference the cosmon roster against installed
/// LaunchAgents and return one `Error` finding per conflict.
///
/// A conflict holds when an installed agent either:
/// - runs the same (non-interpreter) binary basename as a roster entry, or
/// - carries the `com.you.<name>` label of a roster entry.
///
/// The binary-basename match is the authoritative signal (it is the same
/// executable run by two supervisors); the label match is the corroborating
/// signal for the common naming convention. Either is sufficient to flag.
/// Shared interpreters ([`is_generic_interpreter`]) are excluded from the
/// binary path — they identify no single unit, so only their label counts.
#[must_use]
pub fn detect_conflicts(roster: &[SupervisedBinary], agents: &[InstalledAgent]) -> Vec<Finding> {
    let mut out = Vec::new();
    for agent in agents {
        for bin in roster {
            let binary_match = !is_generic_interpreter(&bin.binary)
                && agent.program.as_deref() == Some(bin.binary.as_str());
            let label_match = agent.label == format!("{LABEL_PREFIX}{}", bin.name);
            if !(binary_match || label_match) {
                continue;
            }
            let why = if binary_match {
                format!("runs the same binary `{}`", bin.binary)
            } else {
                format!("carries the `{}` label", agent.label)
            };
            out.push(
                Finding::new(
                    PROBE,
                    Severity::Error,
                    format!(
                        "`{}` is double-supervised: cosmon {} `{}` AND LaunchAgent `{}`",
                        bin.binary, bin.source, bin.name, agent.label
                    ),
                )
                .with_path(agent.plist_path.clone())
                .with_detail(format!(
                    "cosmon roster: {} `{}` (enabled={}); the LaunchAgent {}.\n\
                     A binary belongs to exactly one supervisor — this is the \
                     retired-but-resurrected plist (cf. mailroom-sync, 2026-06-20).",
                    bin.source, bin.name, bin.enabled, why
                ))
                .with_remediation(format!(
                    "cosmon's {source}.toml is the sole supervisor. Unload and remove the plist:\n\
                     \tlaunchctl bootout gui/$(id -u)/{label}\n\
                     \trm {path}\n\
                     If instead the LaunchAgent should win, delete the `{name}` stanza from \
                     ~/.config/cosmon/{source}s.toml.",
                    source = bin.source,
                    label = agent.label,
                    path = agent.plist_path.display(),
                    name = bin.name,
                )),
            );
            break; // one finding per agent is enough
        }
    }
    out
}

/// Default path for the patrol config (`~/.config/cosmon/patrols.toml`).
fn default_patrols_path() -> PathBuf {
    PathBuf::from(shellexpand_home("~/.config/cosmon/patrols.toml").into_owned())
}

/// Default path for the daemons config (`~/.config/cosmon/daemons.toml`).
fn default_daemons_path() -> PathBuf {
    PathBuf::from(shellexpand_home("~/.config/cosmon/daemons.toml").into_owned())
}

/// Default LaunchAgents directories to scan (user + system).
fn default_launch_agents_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let user = shellexpand_home("~/Library/LaunchAgents").into_owned();
    if user != "~/Library/LaunchAgents" {
        dirs.push(PathBuf::from(user));
    }
    dirs.push(PathBuf::from("/Library/LaunchAgents"));
    dirs
}

/// Load the cosmon supervision roster from both tablets. Missing or
/// invalid files yield an empty contribution plus a `Warning` finding
/// (never fatal — a half-readable roster still catches real conflicts).
fn load_roster(patrols: &Path, daemons: &Path) -> (Vec<SupervisedBinary>, Vec<Finding>) {
    let mut roster = Vec::new();
    let mut warnings = Vec::new();

    if patrols.exists() {
        match cosmon_scheduler::config::Config::load(patrols) {
            Ok(cfg) => {
                for p in &cfg.patrols {
                    if let Some(prog) = p.command.first() {
                        roster.push(SupervisedBinary {
                            name: p.name.clone(),
                            source: "patrol",
                            binary: basename(prog),
                            enabled: p.enabled,
                        });
                    }
                }
            }
            Err(e) => warnings.push(Finding::new(
                PROBE,
                Severity::Warning,
                format!("could not parse {}: {e}", patrols.display()),
            )),
        }
    }

    if daemons.exists() {
        match cosmon_daemon_supervisor::config::Config::load(daemons) {
            Ok(cfg) => {
                for d in &cfg.daemons {
                    roster.push(SupervisedBinary {
                        name: d.name.clone(),
                        source: "daemon",
                        binary: basename(&d.binary),
                        enabled: d.enabled,
                    });
                }
            }
            Err(e) => warnings.push(Finding::new(
                PROBE,
                Severity::Warning,
                format!("could not parse {}: {e}", daemons.display()),
            )),
        }
    }

    (roster, warnings)
}

/// Parse `Label` and basename of `ProgramArguments[0]` out of a plist via
/// `plutil`. Returns `None` (skip the file) if plutil is unavailable or the
/// plist is malformed — a single bad plist must not abort the probe.
fn read_plist(path: &Path) -> Option<InstalledAgent> {
    let out = Command::new("plutil")
        .args(["-convert", "json", "-o", "-", "--"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let label = v.get("Label").and_then(|x| x.as_str())?.to_string();
    let program = v
        .get("ProgramArguments")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|x| x.as_str())
        .map(basename);
    Some(InstalledAgent {
        label,
        plist_path: path.to_path_buf(),
        program,
    })
}

/// Scan one or more LaunchAgents directories for `com.you.*.plist`.
fn scan_agents(dirs: &[PathBuf]) -> Vec<InstalledAgent> {
    let mut agents = Vec::new();
    for dir in dirs {
        // absent dir (e.g. Linux CI) → nothing to scan
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for ent in entries.flatten() {
            let p = ent.path();
            let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let is_plist = p
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("plist"));
            if !name.starts_with(LABEL_PREFIX) || !is_plist {
                continue;
            }
            if let Some(agent) = read_plist(&p) {
                agents.push(agent);
            }
        }
    }
    agents
}

/// Run the supervision probe with the given (possibly overridden) inputs.
///
/// # Errors
/// Never returns `Err` today — config/plist failures degrade to findings —
/// but the signature is fallible to match the other probes and to leave
/// room for a future hard failure.
#[allow(clippy::unnecessary_wraps)]
pub fn scan(args: &Args) -> anyhow::Result<ProbeReport> {
    let patrols = args.patrols.clone().unwrap_or_else(default_patrols_path);
    let daemons = args.daemons.clone().unwrap_or_else(default_daemons_path);
    let dirs = match &args.launch_agents_dir {
        Some(d) => vec![d.clone()],
        None => default_launch_agents_dirs(),
    };

    let (roster, mut warnings) = load_roster(&patrols, &daemons);
    let agents = scan_agents(&dirs);

    let mut report = ProbeReport::new(PROBE);
    report.scanned = agents.len();
    report
        .findings
        .append(&mut detect_conflicts(&roster, &agents));
    report.findings.append(&mut warnings);
    Ok(report)
}

/// Execute `cs doctor supervision`.
///
/// # Errors
/// Propagates errors from [`scan`].
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let report = scan(args)?;
    super::emit_report_and_exit(ctx, &[report])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patrol(name: &str, binary: &str, enabled: bool) -> SupervisedBinary {
        SupervisedBinary {
            name: name.to_string(),
            source: "patrol",
            binary: binary.to_string(),
            enabled,
        }
    }

    fn agent(label: &str, program: Option<&str>) -> InstalledAgent {
        InstalledAgent {
            label: label.to_string(),
            plist_path: PathBuf::from(format!("/tmp/{label}.plist")),
            program: program.map(str::to_string),
        }
    }

    #[test]
    fn flags_binary_double_supervised_by_patrol_and_launchagent() {
        // The exact 2026-06-20 shape: patrol owns mailroom-sync AND a
        // LaunchAgent runs the same binary.
        let roster = vec![patrol("mailroom-sync", "mailroom-sync", true)];
        let agents = vec![agent("com.you.mailroom-sync", Some("mailroom-sync"))];
        let findings = detect_conflicts(&roster, &agents);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].title.contains("double-supervised"));
        assert!(findings[0].title.contains("mailroom-sync"));
    }

    #[test]
    fn flags_on_label_match_even_when_program_differs() {
        // A plist whose binary path differs (e.g. a wrapper) but whose
        // label still matches the roster name is the same logical conflict.
        let roster = vec![patrol("mailroom-sync", "mailroom-sync-wrapper", true)];
        let agents = vec![agent("com.you.mailroom-sync", Some("/usr/bin/env"))];
        let findings = detect_conflicts(&roster, &agents);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
    }

    #[test]
    fn clean_when_launchagent_not_in_roster() {
        // A LaunchAgent for a binary cosmon does NOT supervise is fine —
        // the probe must not flag every plist, only the doubly-supervised.
        let roster = vec![patrol("mailroom-sync", "mailroom-sync", true)];
        let agents = vec![agent("com.you.notification-bot", Some("notification-bot"))];
        assert!(detect_conflicts(&roster, &agents).is_empty());
    }

    #[test]
    fn clean_when_no_launchagents() {
        let roster = vec![patrol("mailroom-sync", "mailroom-sync", true)];
        assert!(detect_conflicts(&roster, &[]).is_empty());
    }

    #[test]
    fn disabled_roster_entry_still_flags_overlap() {
        // Even a disabled patrol means cosmon is the *declared* supervisor;
        // a co-existing LaunchAgent is still a fork to surface (the enabled
        // state is reported in the detail, not used to suppress).
        let roster = vec![patrol("mailroom-sync", "mailroom-sync", false)];
        let agents = vec![agent("com.you.mailroom-sync", Some("mailroom-sync"))];
        let findings = detect_conflicts(&roster, &agents);
        assert_eq!(findings.len(), 1);
        assert!(findings[0]
            .detail
            .as_ref()
            .is_some_and(|d| d.contains("enabled=false")));
    }

    #[test]
    fn one_finding_per_agent_even_with_multiple_roster_hits() {
        // Defensive: if the same binary appears in both tablets, we still
        // emit a single finding for the one offending plist.
        let roster = vec![
            patrol("mailroom-sync", "mailroom-sync", true),
            SupervisedBinary {
                name: "mailroom-sync".to_string(),
                source: "daemon",
                binary: "mailroom-sync".to_string(),
                enabled: true,
            },
        ];
        let agents = vec![agent("com.you.mailroom-sync", Some("mailroom-sync"))];
        assert_eq!(detect_conflicts(&roster, &agents).len(), 1);
    }

    #[test]
    fn shared_interpreter_does_not_false_positive() {
        // A patrol running `bash <script>` and an unrelated LaunchAgent also
        // running `bash <other>` share the `bash` basename but are distinct
        // units — labels differ, so no conflict. (Observed live 2026-06-23:
        // `chronicle-lint-weekly` vs `scheduled-send.fabien-nda`.)
        let roster = vec![patrol("chronicle-lint-weekly", "bash", true)];
        let agents = vec![agent("com.you.scheduled-send.fabien-nda", Some("bash"))];
        assert!(detect_conflicts(&roster, &agents).is_empty());
    }

    #[test]
    fn shared_interpreter_still_flags_on_label_match() {
        // If the interpreter-run patrol and the LaunchAgent share the
        // `com.you.<name>` label, that IS the same unit — flag it.
        let roster = vec![patrol("chronicle-lint-weekly", "bash", true)];
        let agents = vec![agent("com.you.chronicle-lint-weekly", Some("bash"))];
        assert_eq!(detect_conflicts(&roster, &agents).len(), 1);
    }

    #[test]
    fn basename_extracts_final_component() {
        assert_eq!(
            basename("/Users/you/.local/bin/mailroom-sync"),
            "mailroom-sync"
        );
        assert_eq!(basename("mailroom-sync"), "mailroom-sync");
    }

    #[test]
    fn scan_with_absent_dirs_is_clean() {
        // End-to-end: nonexistent config + LaunchAgents dir → empty report,
        // no panic (the Linux-CI / fresh-machine path).
        let args = Args {
            patrols: Some(PathBuf::from("/nonexistent/patrols.toml")),
            daemons: Some(PathBuf::from("/nonexistent/daemons.toml")),
            launch_agents_dir: Some(PathBuf::from("/nonexistent/LaunchAgents")),
        };
        let report = scan(&args).unwrap();
        assert_eq!(report.scanned, 0);
        assert!(report.findings.is_empty());
    }
}
