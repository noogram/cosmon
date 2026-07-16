// SPDX-License-Identifier: AGPL-3.0-only

//! Sensorium loader — five-organ disk reader powering the `cs peek
//! --snapshot` vital strip (`ADR-109 (sensorium-strip)`).
//!
//! Walks `<state_dir>/sensorium/` to compute the [`Sensorium`]
//! aggregate:
//!
//! ```text
//! .cosmon/state/sensorium/
//! ├── inbox.ndjson           # peau: one row per landed signal
//! ├── heartbeat.ndjson       # cœur: one row per beat {ts, kind, moved}
//! ├── <galaxy>/SOUL.md       # visage: identity (frontmatter `name:`)
//! ├── notes/*.md             # carnet: notes with `decay_at:` frontmatter
//! └── outbox/*.md            # voix: drafts with `permission: pending|...`
//! ```
//!
//! Every read is **tolerant of absence** — a missing file or
//! malformed row collapses to the zero baseline rather than erroring.
//! That preserves the silence rule (`responses/jr.md`): nothing
//! happened ⇒ nothing rendered.
//!
//! This module lives in the crate's `lib.rs` rather than under
//! `src/cmd/` so external integration tests can compute the strip
//! without shelling out to the `cs` binary; the
//! `cs sensorium` CLI command wraps this loader thinly.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use cosmon_observability::sensorium::{HeartbeatKind, Sensorium, HEARTBEAT_WINDOW};

/// Compute the [`Sensorium`] from disk.
///
/// Every read is best-effort; the strip is advisory, not load-bearing.
/// A torn write, a missing file, or a malformed frontmatter degrades
/// silently to the zero baseline for the affected organ.
#[must_use]
pub fn load_sensorium(state_dir: &Path) -> Sensorium {
    let root = state_dir.join("sensorium");
    let now = Utc::now();
    let autopilot_off = autopilot_off_marker_exists();

    let peau_signals_24h = count_inbox_within(&root.join("inbox.ndjson"), now, Duration::hours(24));
    let heartbeat = load_heartbeat(&root.join("heartbeat.ndjson"));
    let (visage_galaxy, visage_seal_drift) = load_visage(&root);
    let carnet_count = count_md_files(&root.join("notes"));
    let carnet_decay_6h = count_decay_within(&root.join("notes"), now, Duration::hours(6));
    let voix_awaiting = count_pending_outbox(&root.join("outbox"));

    Sensorium {
        peau_signals_24h,
        heartbeat,
        visage_galaxy,
        visage_seal_drift,
        carnet_count,
        carnet_decay_6h,
        voix_awaiting,
        autopilot_off,
    }
}

/// Resolve `~/.cosmon/autopilot.off` and report whether the file
/// exists. Returns `false` when `$HOME` is unset rather than panicking
/// — the kill-switch is a UI hint, not a security boundary.
fn autopilot_off_marker_exists() -> bool {
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    PathBuf::from(home)
        .join(".cosmon")
        .join("autopilot.off")
        .exists()
}

fn count_inbox_within(path: &Path, now: DateTime<Utc>, window: Duration) -> u32 {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    let cutoff = now - window;
    let mut count: u32 = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let ts = v.get("ts").and_then(|t| t.as_str()).unwrap_or_default();
        let Ok(ts) = DateTime::parse_from_rfc3339(ts) else {
            continue;
        };
        if ts.with_timezone(&Utc) >= cutoff {
            count = count.saturating_add(1);
        }
    }
    count
}

fn load_heartbeat(path: &Path) -> [HeartbeatKind; HEARTBEAT_WINDOW] {
    let Ok(text) = std::fs::read_to_string(path) else {
        return [HeartbeatKind::Resting; HEARTBEAT_WINDOW];
    };
    let mut beats: Vec<HeartbeatKind> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let moved_any = v
            .get("moved")
            .and_then(|m| m.as_array())
            .is_some_and(|arr| !arr.is_empty());
        beats.push(if moved_any {
            HeartbeatKind::Live
        } else {
            HeartbeatKind::Resting
        });
    }
    let mut out = [HeartbeatKind::Resting; HEARTBEAT_WINDOW];
    let take = beats.len().min(HEARTBEAT_WINDOW);
    let start = beats.len() - take;
    for (i, beat) in beats[start..].iter().enumerate() {
        out[HEARTBEAT_WINDOW - take + i] = *beat;
    }
    out
}

fn load_visage(root: &Path) -> (Option<String>, bool) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return (None, false);
    };
    let mut candidates: BTreeSet<PathBuf> = BTreeSet::new();
    for entry in rd.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let soul = p.join("SOUL.md");
        if soul.is_file() {
            candidates.insert(soul);
        }
    }
    let Some(soul) = candidates.into_iter().next() else {
        return (None, false);
    };
    let Ok(text) = std::fs::read_to_string(&soul) else {
        return (None, false);
    };
    let name = parse_frontmatter_field(&text, "name");
    let drift = parse_frontmatter_field(&text, "seal_drift")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"));
    (name, drift)
}

fn count_md_files(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut count: u64 = 0;
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")) {
            count += 1;
        }
    }
    count
}

fn count_decay_within(dir: &Path, now: DateTime<Utc>, window: Duration) -> Option<u32> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return None;
    };
    let horizon = now + window;
    let mut count: u32 = 0;
    let mut saw_any = false;
    for entry in rd.flatten() {
        let p = entry.path();
        if !p.is_file() || !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")) {
            continue;
        }
        saw_any = true;
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let Some(decay_str) = parse_frontmatter_field(&text, "decay_at") else {
            continue;
        };
        let Ok(decay_at) = DateTime::parse_from_rfc3339(decay_str.trim()) else {
            continue;
        };
        let decay_at = decay_at.with_timezone(&Utc);
        if decay_at >= now && decay_at <= horizon {
            count = count.saturating_add(1);
        }
    }
    if saw_any && count > 0 {
        Some(count)
    } else {
        None
    }
}

fn count_pending_outbox(dir: &Path) -> u32 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut count: u32 = 0;
    for entry in rd.flatten() {
        let p = entry.path();
        if !p.is_file() || !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let Some(perm) = parse_frontmatter_field(&text, "permission") else {
            continue;
        };
        if perm.trim().eq_ignore_ascii_case("pending") {
            count = count.saturating_add(1);
        }
    }
    count
}

/// Parse a `key: value` line from a YAML-style frontmatter block at
/// the top of `text`. Returns `None` when the file does not begin with
/// `---\n` or when the key is absent. The parser is intentionally
/// minimal — we only need flat string fields (`name:`, `decay_at:`,
/// `permission:`, `seal_drift:`).
fn parse_frontmatter_field(text: &str, key: &str) -> Option<String> {
    let mut lines = text.lines();
    let first = lines.next()?;
    if first.trim() != "---" {
        return None;
    }
    let prefix = format!("{key}:");
    for line in lines {
        if line.trim() == "---" {
            return None;
        }
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let mut v = rest.trim().to_owned();
            if (v.starts_with('"') && v.ends_with('"') && v.len() >= 2)
                || (v.starts_with('\'') && v.ends_with('\'') && v.len() >= 2)
            {
                v = v[1..v.len() - 1].to_owned();
            }
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn empty_state_dir_yields_default_sensorium() {
        let tmp = TempDir::new().unwrap();
        let s = load_sensorium(tmp.path());
        let s = Sensorium {
            autopilot_off: false,
            ..s
        };
        assert!(s.is_empty(), "expected empty sensorium, got {s:?}");
    }

    #[test]
    fn inbox_signals_within_24h_counted() {
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("sensorium").join("inbox.ndjson");
        std::fs::create_dir_all(inbox.parent().unwrap()).unwrap();
        let recent = Utc::now() - Duration::hours(1);
        let old = Utc::now() - Duration::hours(48);
        let lines = format!(
            "{{\"ts\":\"{}\",\"channel\":\"whatsapp\"}}\n\
             {{\"ts\":\"{}\",\"channel\":\"imessage\"}}\n",
            recent.to_rfc3339(),
            old.to_rfc3339(),
        );
        std::fs::write(&inbox, lines).unwrap();
        let s = load_sensorium(tmp.path());
        assert_eq!(s.peau_signals_24h, 1);
    }

    #[test]
    fn heartbeat_live_when_moved_array_nonempty() {
        let tmp = TempDir::new().unwrap();
        let hb = tmp.path().join("sensorium").join("heartbeat.ndjson");
        std::fs::create_dir_all(hb.parent().unwrap()).unwrap();
        let lines = "\
            {\"ts\":\"2026-05-22T00:00:00Z\",\"kind\":\"patrol\",\"moved\":[]}\n\
            {\"ts\":\"2026-05-22T00:01:00Z\",\"kind\":\"patrol\",\"moved\":[\"task-1\"]}\n\
            {\"ts\":\"2026-05-22T00:02:00Z\",\"kind\":\"patrol\",\"moved\":[]}\n";
        std::fs::write(&hb, lines).unwrap();
        let s = load_sensorium(tmp.path());
        assert!(matches!(s.heartbeat[7], HeartbeatKind::Resting));
        assert!(matches!(s.heartbeat[8], HeartbeatKind::Live));
        assert!(matches!(s.heartbeat[9], HeartbeatKind::Resting));
    }

    #[test]
    fn visage_reads_first_galaxy_soul() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("sensorium").join("cosmon");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SOUL.md"), "---\nname: cosmon\n---\nBody text\n").unwrap();
        let s = load_sensorium(tmp.path());
        assert_eq!(s.visage_galaxy.as_deref(), Some("cosmon"));
        assert!(!s.visage_seal_drift);
    }

    #[test]
    fn visage_seal_drift_flag_honored() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("sensorium").join("cosmon");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SOUL.md"),
            "---\nname: cosmon\nseal_drift: true\n---\n",
        )
        .unwrap();
        let s = load_sensorium(tmp.path());
        assert!(s.visage_seal_drift);
    }

    #[test]
    fn carnet_counts_markdown_files() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("sensorium").join("notes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.md"), "---\n---\nA").unwrap();
        std::fs::write(dir.join("b.md"), "---\n---\nB").unwrap();
        std::fs::write(dir.join("ignore.txt"), "skip").unwrap();
        let s = load_sensorium(tmp.path());
        assert_eq!(s.carnet_count, 2);
    }

    #[test]
    fn carnet_decay_counts_notes_within_6h() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("sensorium").join("notes");
        std::fs::create_dir_all(&dir).unwrap();
        let soon = (Utc::now() + Duration::hours(2)).to_rfc3339();
        let later = (Utc::now() + Duration::hours(24)).to_rfc3339();
        std::fs::write(dir.join("soon.md"), format!("---\ndecay_at: {soon}\n---\n")).unwrap();
        std::fs::write(
            dir.join("later.md"),
            format!("---\ndecay_at: {later}\n---\n"),
        )
        .unwrap();
        let s = load_sensorium(tmp.path());
        assert_eq!(s.carnet_decay_6h, Some(1));
    }

    #[test]
    fn voix_counts_pending_outbox() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("sensorium").join("outbox");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("draft-a.md"), "---\npermission: pending\n---\n").unwrap();
        std::fs::write(dir.join("draft-b.md"), "---\npermission: granted\n---\n").unwrap();
        std::fs::write(dir.join("draft-c.md"), "---\npermission: pending\n---\n").unwrap();
        let s = load_sensorium(tmp.path());
        assert_eq!(s.voix_awaiting, 2);
    }

    #[test]
    fn malformed_files_collapse_to_zero() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("sensorium");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("inbox.ndjson"), "not-json\n{garbled\n").unwrap();
        std::fs::write(root.join("heartbeat.ndjson"), "also-not-json\n").unwrap();
        let s = load_sensorium(tmp.path());
        assert_eq!(s.peau_signals_24h, 0);
        assert!(s
            .heartbeat
            .iter()
            .all(|h| matches!(h, HeartbeatKind::Resting)));
    }

    #[test]
    fn frontmatter_parser_handles_quoted_values() {
        let text = "---\nname: \"cosmon\"\n---\n";
        assert_eq!(
            parse_frontmatter_field(text, "name").as_deref(),
            Some("cosmon"),
        );
    }

    #[test]
    fn frontmatter_parser_returns_none_without_block() {
        assert!(parse_frontmatter_field("no frontmatter here", "name").is_none());
    }
}
