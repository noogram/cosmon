// SPDX-License-Identifier: AGPL-3.0-only

//! `cs presence` — live-session registry and log-channel pull.
//!
//! The presence registry (ADR-038 follow-up) lives on disk under
//! `.cosmon/state/presence/`.
//! Each live session owns one `<sid>.json` snapshot that advertises the
//! session's galaxy, cwd, pid, current molecule, and a free-form
//! headline — plus a `<sid>.log` / `<sid>.seek` pair carrying the
//! whisper pull channel.
//!
//! Four subcommands ship together:
//!
//! - `ping` — upsert this session's snapshot (C-PRESENCE-CORE).
//! - `ls` — scan the directory and render live peers.
//! - `gc` — sweep stale snapshots whose pids no longer exist.
//! - `poll` — pull new whisper log lines since the last read.
//!
//! Composition: all four share a single `PresenceStore` pointed at
//! `<state_root>/presence/`. Layout is stable — writers
//! (`cs whisper --to-session`) and readers (`cs presence poll`) can
//! share the same path helpers.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use cosmon_core::id::{MoleculeId, SessionId};
use cosmon_core::presence::Presence;
use cosmon_filestore::PresenceStore;

use super::Context;

/// Top-level arguments for `cs presence`.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: Sub,
}

/// Presence subcommands — see module doc for the full picture.
#[derive(clap::Subcommand)]
pub enum Sub {
    /// Upsert this session's presence snapshot.
    Ping(PingArgs),
    /// List live sessions known to this galaxy's presence directory.
    Ls(LsArgs),
    /// Remove stale snapshots whose pids are no longer alive.
    Gc,
    /// Print unread log lines for a session and bump the seek pointer.
    Poll(PollArgs),
}

/// Arguments for `cs presence ping`.
#[derive(clap::Args)]
pub struct PingArgs {
    /// Session id to write. Defaults to `$COSMON_SESSION_ID`; falls
    /// back to a tty-derived stable id if unset.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
    /// One-line description of what the session is doing.
    #[arg(long)]
    pub headline: Option<String>,
    /// Molecule currently under this session's attention.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub molecule: Option<MoleculeId>,
    /// Override the galaxy label (default: `cosmon`).
    #[arg(long, default_value = "cosmon")]
    pub galaxy: String,
}

/// Arguments for `cs presence ls`.
#[derive(clap::Args)]
pub struct LsArgs {
    /// Emit NDJSON instead of a human-readable table.
    #[arg(long)]
    pub json: bool,
    /// Include stale (but not yet garbage-collected) snapshots.
    #[arg(long)]
    pub all: bool,
    /// Filter to one galaxy (default: show every galaxy present).
    #[arg(long)]
    pub galaxy: Option<String>,
}

/// Arguments for `cs presence poll`.
#[derive(clap::Args)]
pub struct PollArgs {
    /// Session id whose log to poll. Defaults to `$COSMON_SESSION_ID`
    /// when unset — the runtime exports this on session start.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
}

/// Dispatch a `cs presence <sub>` invocation.
///
/// # Errors
/// Propagates filesystem errors and "no session id" when the operator
/// neither passed `--session` nor exported `$COSMON_SESSION_ID`.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        Sub::Ping(a) => run_ping(ctx, a),
        Sub::Ls(a) => run_ls(ctx, a),
        Sub::Gc => run_gc(ctx),
        Sub::Poll(a) => run_poll(ctx, a),
    }
}

fn state_root(ctx: &Context) -> PathBuf {
    ctx.config.clone().unwrap_or_else(super::default_state_dir)
}

fn store(ctx: &Context) -> PresenceStore {
    PresenceStore::new(state_root(ctx))
}

fn run_ping(ctx: &Context, args: &PingArgs) -> anyhow::Result<()> {
    let session_id = resolve_or_derive_sid(args.session.as_deref())?;
    let now = Utc::now();
    let store = store(ctx);

    // Preserve `started_at` across subsequent pings so the registry
    // records genuine session age, not last-heartbeat age. If the
    // previous snapshot is gone or corrupt we treat this as a fresh
    // start — silent fallback, not an error.
    let prior_started_at = load_prior(&store, &session_id).map(|p| p.started_at);

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let presence = Presence {
        session_id: session_id.clone(),
        galaxy: args.galaxy.clone(),
        cwd,
        pid: std::process::id(),
        started_at: prior_started_at.unwrap_or(now),
        heartbeat_at: now,
        current_molecule: args.molecule.clone(),
        headline: args.headline.clone().unwrap_or_default(),
        tty: current_tty(),
    };
    store.upsert(&presence)?;

    if ctx.json {
        let v = serde_json::to_value(&presence)?;
        println!("{v}");
    } else {
        println!(
            "presence ping: {sid} in {galaxy} (pid={pid})",
            sid = presence.session_id.as_str(),
            galaxy = presence.galaxy,
            pid = presence.pid,
        );
    }
    Ok(())
}

fn run_ls(ctx: &Context, args: &LsArgs) -> anyhow::Result<()> {
    let store = store(ctx);
    let now = Utc::now();
    let mut rows = store.scan()?;
    if !args.all {
        rows.retain(|p| p.is_live(now));
    }
    if let Some(ref galaxy) = args.galaxy {
        rows.retain(|p| &p.galaxy == galaxy);
    }
    // Stable order, oldest heartbeat last.
    rows.sort_by(|a, b| b.heartbeat_at.cmp(&a.heartbeat_at));

    if args.json || ctx.json {
        let mut out = std::io::stdout().lock();
        for p in &rows {
            let v = serde_json::to_value(p)?;
            writeln!(out, "{v}")?;
        }
        return Ok(());
    }

    if rows.is_empty() {
        println!("(no live sessions)");
        return Ok(());
    }
    let header = format!(
        "{:<26}  {:<12}  {:>6}  {:>8}  HEADLINE",
        "SESSION", "GALAXY", "PID", "AGE",
    );
    println!("{header}");
    for p in &rows {
        println!(
            "{:<26}  {:<12}  {:>6}  {:>8}  {}",
            p.session_id.as_str(),
            p.galaxy,
            p.pid,
            format_age(now, p.heartbeat_at),
            p.headline,
        );
    }
    Ok(())
}

fn run_gc(ctx: &Context) -> anyhow::Result<()> {
    let removed = store(ctx).gc()?;
    if ctx.json {
        let v = serde_json::json!({ "removed": removed });
        println!("{v}");
    } else {
        println!("presence gc: removed {removed} stale snapshot(s)");
    }
    Ok(())
}

fn run_poll(ctx: &Context, args: &PollArgs) -> anyhow::Result<()> {
    let sid = resolve_sid_for_poll(args)?;
    let store = store(ctx);
    let log_path = store.log_path(&sid);
    let seek_path = store.seek_path(&sid);

    let content = fs::read_to_string(&log_path).unwrap_or_default();
    let seek = read_seek(&seek_path);
    let end = content.len();
    let tail = if seek < end { &content[seek..] } else { "" };

    if !tail.is_empty() {
        fs::create_dir_all(store.dir()).map_err(|e| {
            anyhow::anyhow!(
                "failed to create presence dir {}: {e}",
                store.dir().display()
            )
        })?;
        fs::write(&seek_path, end.to_string())
            .map_err(|e| anyhow::anyhow!("failed to write seek {}: {e}", seek_path.display()))?;
    }

    if ctx.json {
        let lines: Vec<&str> = tail.lines().collect();
        let out = serde_json::json!({
            "session": sid.as_str(),
            "bytes": tail.len(),
            "lines": lines,
            "seek": end,
        });
        println!("{out}");
    } else {
        print!("{tail}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Session id resolution
// ---------------------------------------------------------------------------

/// Resolve a session id for `ping`: explicit `--session` wins, then
/// `$COSMON_SESSION_ID`, then `$CLAUDE_SESSION_ID`, then a stable
/// tty-hash fallback so two shells in different tabs get distinct ids.
fn resolve_or_derive_sid(explicit: Option<&str>) -> anyhow::Result<SessionId> {
    if let Some(s) = explicit {
        return Ok(SessionId::new(s)?);
    }
    for env_var in ["COSMON_SESSION_ID", "CLAUDE_SESSION_ID"] {
        if let Ok(s) = std::env::var(env_var) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Ok(SessionId::new(trimmed)?);
            }
        }
    }
    Ok(SessionId::new(derive_stable_sid())?)
}

/// Resolve a session id for `poll` — same precedence as `ping` but the
/// tty-hash fallback is not used (poll is always driven by a hook that
/// already knows the id).
fn resolve_sid_for_poll(args: &PollArgs) -> anyhow::Result<SessionId> {
    if let Some(s) = &args.session {
        return Ok(SessionId::new(s.clone())?);
    }
    for env_var in ["COSMON_SESSION_ID", "CLAUDE_SESSION_ID"] {
        if let Ok(s) = std::env::var(env_var) {
            if !s.trim().is_empty() {
                return Ok(SessionId::new(s)?);
            }
        }
    }
    Err(anyhow::anyhow!(
        "no session id — pass --session <SID> or export COSMON_SESSION_ID"
    ))
}

/// Derive a stable session id from `(tty, boot_epoch)` when no
/// environment override is present. Produces `session-<12-hex>` so it
/// looks distinct from a Claude-provided UUID and survives CLI
/// invocations from the same shell.
fn derive_stable_sid() -> String {
    use sha2::{Digest, Sha256};
    let tty = current_tty().unwrap_or_else(|| "unknown-tty".to_owned());
    let boot = boot_epoch_seconds().unwrap_or(0);
    let mut h = Sha256::new();
    h.update(tty.as_bytes());
    h.update(b":");
    h.update(boot.to_le_bytes());
    let digest = h.finalize();
    let mut hex = String::with_capacity(12);
    for b in digest.iter().take(6) {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    format!("session-{hex}")
}

fn current_tty() -> Option<String> {
    // `tty(1)` is POSIX-standard; the output is one line such as
    // `/dev/ttys012`. When stdin is not a tty it prints "not a tty"
    // and exits 1, which we surface as `None`.
    let out = std::process::Command::new("tty").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8(out.stdout).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn boot_epoch_seconds() -> Option<u64> {
    // macOS: `sysctl -n kern.boottime` → `{ sec = 1745..., usec = ... }`.
    // Linux: `/proc/stat` exposes `btime <epoch>`. Both fail gracefully:
    // a `None` here just means the sid falls back on pure-tty hashing,
    // still stable within a single boot.
    if let Ok(content) = fs::read_to_string("/proc/stat") {
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("btime ") {
                if let Ok(n) = rest.trim().parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    let out = std::process::Command::new("sysctl")
        .args(["-n", "kern.boottime"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    // Parse "{ sec = 1745689200, usec = 12345 } ..." defensively.
    let sec_idx = s.find("sec = ")?;
    let tail = &s[sec_idx + "sec = ".len()..];
    let end = tail.find(',').unwrap_or(tail.len());
    tail[..end].trim().parse::<u64>().ok()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_prior(store: &PresenceStore, sid: &SessionId) -> Option<Presence> {
    let path = store.snapshot_path(sid);
    let data = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

fn read_seek(path: &Path) -> usize {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0)
}

fn format_age(now: DateTime<Utc>, heartbeat: DateTime<Utc>) -> String {
    let d = now - heartbeat;
    let secs = d.num_seconds();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::presence::STALE_AFTER;
    use tempfile::tempdir;

    fn ctx_for(dir: &Path) -> Context {
        Context {
            verbose: false,
            json: false,
            config: Some(dir.to_path_buf()),
        }
    }

    #[test]
    fn read_seek_missing_returns_zero() {
        let dir = tempdir().unwrap();
        assert_eq!(read_seek(&dir.path().join("nope.seek")), 0);
    }

    #[test]
    fn read_seek_unparseable_returns_zero() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("bad.seek");
        fs::write(&p, "not-a-number").unwrap();
        assert_eq!(read_seek(&p), 0);
    }

    #[test]
    fn read_seek_valid() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ok.seek");
        fs::write(&p, "42").unwrap();
        assert_eq!(read_seek(&p), 42);
    }

    #[test]
    fn ping_writes_snapshot_and_ls_reads_it_back() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        run_ping(
            &ctx,
            &PingArgs {
                session: Some("session-alpha".to_owned()),
                headline: Some("writing tests".to_owned()),
                molecule: None,
                galaxy: "cosmon".to_owned(),
            },
        )
        .unwrap();

        let loaded = PresenceStore::new(dir.path()).scan().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].session_id.as_str(), "session-alpha");
        assert_eq!(loaded[0].galaxy, "cosmon");
        assert_eq!(loaded[0].headline, "writing tests");

        // ls should at least run without error.
        run_ls(
            &ctx,
            &LsArgs {
                json: true,
                all: false,
                galaxy: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn ping_preserves_started_at_across_bumps() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        let args = PingArgs {
            session: Some("session-sticky".to_owned()),
            headline: None,
            molecule: None,
            galaxy: "cosmon".to_owned(),
        };
        run_ping(&ctx, &args).unwrap();
        let first = PresenceStore::new(dir.path()).scan().unwrap()[0].clone();
        std::thread::sleep(std::time::Duration::from_millis(10));
        run_ping(&ctx, &args).unwrap();
        let second = PresenceStore::new(dir.path()).scan().unwrap()[0].clone();
        assert_eq!(first.started_at, second.started_at);
        assert!(second.heartbeat_at >= first.heartbeat_at);
    }

    #[test]
    fn gc_runs_on_empty_dir() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        run_gc(&ctx).unwrap();
    }

    #[test]
    fn ls_all_flag_includes_stale() {
        let dir = tempdir().unwrap();
        let store = PresenceStore::new(dir.path());
        // Hand-write a stale-but-alive snapshot so we exercise the filter.
        let old = Utc::now() - STALE_AFTER - chrono::Duration::minutes(5);
        let p = Presence {
            session_id: SessionId::new("session-stale").unwrap(),
            galaxy: "cosmon".to_owned(),
            cwd: PathBuf::from("/tmp"),
            pid: std::process::id(),
            started_at: old,
            heartbeat_at: old,
            current_molecule: None,
            headline: "stale".to_owned(),
            tty: None,
        };
        store.upsert(&p).unwrap();

        let ctx = ctx_for(dir.path());
        // Default ls filters it out.
        run_ls(
            &ctx,
            &LsArgs {
                json: true,
                all: false,
                galaxy: None,
            },
        )
        .unwrap();
        // --all includes it.
        run_ls(
            &ctx,
            &LsArgs {
                json: true,
                all: true,
                galaxy: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn poll_with_no_presence_dir_is_clean() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        run_poll(
            &ctx,
            &PollArgs {
                session: Some("session-test".to_owned()),
            },
        )
        .unwrap();
    }

    #[test]
    fn poll_emits_unread_tail_and_bumps_seek() {
        let dir = tempdir().unwrap();
        let presence = dir.path().join("presence");
        fs::create_dir_all(&presence).unwrap();
        let log = presence.join("session-test.log");
        fs::write(&log, "first\nsecond\n").unwrap();

        let ctx = ctx_for(dir.path());
        run_poll(
            &ctx,
            &PollArgs {
                session: Some("session-test".to_owned()),
            },
        )
        .unwrap();

        let seek = fs::read_to_string(presence.join("session-test.seek")).unwrap();
        assert_eq!(seek, "first\nsecond\n".len().to_string());

        // Second poll returns nothing; seek stays put.
        run_poll(
            &ctx,
            &PollArgs {
                session: Some("session-test".to_owned()),
            },
        )
        .unwrap();
        let seek2 = fs::read_to_string(presence.join("session-test.seek")).unwrap();
        assert_eq!(seek, seek2);
    }

    #[test]
    fn resolve_for_poll_prefers_explicit() {
        let args = PollArgs {
            session: Some("explicit".to_owned()),
        };
        assert_eq!(resolve_sid_for_poll(&args).unwrap().as_str(), "explicit");
    }

    #[test]
    fn derive_stable_sid_is_nonempty_and_prefixed() {
        let s = derive_stable_sid();
        assert!(s.starts_with("session-"));
        assert!(s.len() > "session-".len());
    }

    // Sanity: the presence_dir layout the CLI reads matches what the
    // PresenceStore produces. A refactor that diverges the two must
    // fail here.
    #[test]
    fn presence_log_filename_contract() {
        let store = PresenceStore::new(PathBuf::from("/tmp/state"));
        let sid = SessionId::new("session-2026-04-24T10-00-00Z").unwrap();
        assert_eq!(
            store.log_path(&sid).to_string_lossy(),
            "/tmp/state/presence/session-2026-04-24T10-00-00Z.log"
        );
        assert_eq!(
            store.snapshot_path(&sid).to_string_lossy(),
            "/tmp/state/presence/session-2026-04-24T10-00-00Z.json"
        );
    }

    #[test]
    fn format_age_labels() {
        let now = Utc::now();
        assert_eq!(format_age(now, now), "0s");
        assert_eq!(format_age(now, now - chrono::Duration::seconds(45)), "45s");
        assert_eq!(format_age(now, now - chrono::Duration::minutes(3)), "3m");
        assert_eq!(format_age(now, now - chrono::Duration::hours(2)), "2h");
    }
}
