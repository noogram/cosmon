// SPDX-License-Identifier: AGPL-3.0-only

//! Fleet replay — timeline visualization of a cosmon run.
//!
//! Produces a project-agnostic JSON payload (`Vec<ReplayMolecule>`) and a
//! self-contained HTML page (D3.js swim-lane chart) that any project can
//! use to replay its fleet runs. The HTML is embedded via `include_str!`
//! so `cs replay` and the Horizon `/replay` view share the same bundle.
//!
//! The HTML template exposes two placeholders:
//!
//! - `/* __COSMON_REPLAY_DATA__ */` — replaced by a JSON array (embedded
//!   mode, for standalone files) or by `null` (fetch mode).
//! - `/* __COSMON_REPLAY_EVENTS_URL__ */` — replaced by a JSON string
//!   literal for the fetch URL when data is not embedded.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A single molecule prepared for the replay timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayMolecule {
    /// Molecule identifier.
    pub id: String,
    /// Lane/role — used by the HTML to pick a color and label.
    pub role: String,
    /// Short topic or title for the row label and tooltip.
    pub topic: String,
    /// When the molecule was created.
    pub nucleated_at: Option<DateTime<Utc>>,
    /// When the worker was spawned (from `worker_spawned` event).
    pub spawned_at: Option<DateTime<Utc>>,
    /// When the molecule completed (from `molecule_completed` event).
    pub completed_at: Option<DateTime<Utc>>,
    /// Timestamps of `molecule_evolved` events, sorted.
    pub steps: Vec<DateTime<Utc>>,
    /// Total number of steps for the molecule's formula.
    pub total_steps: usize,
    /// Upstream molecule IDs that blocked this one.
    pub blocked_by: Vec<String>,
    /// Current molecule status (e.g. `completed`, `running`).
    pub status: String,
}

/// The embedded HTML template with replacement markers.
pub const REPLAY_HTML_TEMPLATE: &str = include_str!("../assets/fleet-replay.html");

/// Build the replay timeline by scanning the state directory.
///
/// `state_dir` is `.cosmon/state/`. Reads `events.jsonl` plus every
/// molecule `state.json` under `fleets/*/molecules/<id>/` and (for
/// backward compat) flat `molecules/<id>/`.
///
/// # Errors
///
/// Returns an error if the state directory cannot be read.
pub fn build_events(state_dir: &Path) -> std::io::Result<Vec<ReplayMolecule>> {
    let events_path = state_dir.join("events.jsonl");
    let mol_events = load_events_indexed(&events_path)?;
    let molecules = scan_molecules(state_dir)?;

    let mut out: Vec<ReplayMolecule> = molecules
        .into_iter()
        .map(|mol| project(&mol, &mol_events))
        .collect();

    out.sort_by_key(|a| a.nucleated_at);
    Ok(out)
}

/// Render the HTML page with events baked into `EMBEDDED_DATA`.
///
/// Produces a fully self-contained file suitable for `file://` opening
/// (the D3 library is still loaded from a CDN at runtime).
#[must_use]
pub fn render_standalone(events: &[ReplayMolecule]) -> String {
    let json = serde_json::to_string(events).unwrap_or_else(|_| "[]".to_owned());
    REPLAY_HTML_TEMPLATE
        .replacen("/* __COSMON_REPLAY_DATA__ */ null", &json, 1)
        .replacen(
            "/* __COSMON_REPLAY_EVENTS_URL__ */ 'events.json'",
            "'events.json'",
            1,
        )
}

/// Render the HTML page that fetches events from `events_url` at runtime.
///
/// Used by the Horizon `/replay` view and the local `cs replay --port`
/// server: the JSON is served separately so live reloads are cheap.
#[must_use]
pub fn render_fetch(events_url: &str) -> String {
    let url_literal =
        serde_json::to_string(events_url).unwrap_or_else(|_| "\"events.json\"".to_owned());
    REPLAY_HTML_TEMPLATE.replacen(
        "/* __COSMON_REPLAY_EVENTS_URL__ */ 'events.json'",
        &url_literal,
        1,
    )
}

fn load_events_indexed(path: &Path) -> std::io::Result<BTreeMap<String, Vec<Value>>> {
    let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    if !path.exists() {
        return Ok(out);
    }
    let text = fs::read_to_string(path)?;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some(mid) = molecule_id_of(&v) {
            out.entry(mid).or_default().push(v);
        }
    }
    Ok(out)
}

fn molecule_id_of(envelope: &Value) -> Option<String> {
    let ev = envelope.get("event").unwrap_or(envelope);
    for key in ["molecule_id", "molecule"] {
        if let Some(s) = ev.get(key).and_then(Value::as_str) {
            return Some(s.to_owned());
        }
    }
    None
}

fn event_kind(envelope: &Value) -> Option<&str> {
    let ev = envelope.get("event").unwrap_or(envelope);
    ev.get("kind")
        .and_then(Value::as_str)
        .or_else(|| ev.get("type").and_then(Value::as_str))
}

fn event_timestamp(envelope: &Value) -> Option<DateTime<Utc>> {
    let ts = envelope
        .get("timestamp")
        .or_else(|| envelope.get("at"))
        .and_then(Value::as_str)?;
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn scan_molecules(state_dir: &Path) -> std::io::Result<Vec<Value>> {
    let mut out: Vec<Value> = Vec::new();

    // Fleet-scoped layout: fleets/<fleet>/molecules/<id>/state.json
    let fleets_dir = state_dir.join("fleets");
    if fleets_dir.is_dir() {
        for fleet in fs::read_dir(&fleets_dir)?.flatten() {
            let mols_dir = fleet.path().join("molecules");
            if mols_dir.is_dir() {
                collect_molecules(&mols_dir, &mut out)?;
            }
        }
    }

    // Legacy flat layout: molecules/<id>/state.json
    let flat = state_dir.join("molecules");
    if flat.is_dir() {
        collect_molecules(&flat, &mut out)?;
    }

    Ok(out)
}

fn collect_molecules(dir: &Path, out: &mut Vec<Value>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)?.flatten() {
        let state_file = entry.path().join("state.json");
        if state_file.is_file() {
            if let Ok(text) = fs::read_to_string(&state_file) {
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    out.push(v);
                }
            }
        }
    }
    Ok(())
}

fn project(state: &Value, mol_events: &BTreeMap<String, Vec<Value>>) -> ReplayMolecule {
    let id = state
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    let role = state
        .get("formula_id")
        .and_then(Value::as_str)
        .unwrap_or("task")
        .to_owned();

    let topic = state
        .pointer("/variables/topic")
        .and_then(Value::as_str)
        .map_or_else(
            || {
                state
                    .get("formula_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            },
            truncate_topic,
        );

    let nucleated_at = state
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));

    let mut spawned_at = None;
    let mut completed_at = None;
    let mut steps: Vec<DateTime<Utc>> = Vec::new();

    if let Some(events) = mol_events.get(&id) {
        for e in events {
            match event_kind(e) {
                Some("worker_spawned") => spawned_at = event_timestamp(e).or(spawned_at),
                Some("molecule_completed") => completed_at = event_timestamp(e).or(completed_at),
                Some("molecule_evolved") => {
                    if let Some(ts) = event_timestamp(e) {
                        steps.push(ts);
                    }
                }
                _ => {}
            }
        }
    }
    steps.sort();
    steps.dedup();

    let mut blocked_by: Vec<String> = Vec::new();
    if let Some(links) = state.get("typed_links").and_then(Value::as_array) {
        for link in links {
            if link.get("rel").and_then(Value::as_str) == Some("blocked_by") {
                if let Some(src) = link.get("source").and_then(Value::as_str) {
                    blocked_by.push(src.to_owned());
                }
            }
        }
    }

    let status = state
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();

    let total_steps = state
        .get("total_steps")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0);

    ReplayMolecule {
        id,
        role,
        topic,
        nucleated_at,
        spawned_at,
        completed_at,
        steps,
        total_steps,
        blocked_by,
        status,
    }
}

fn truncate_topic(s: &str) -> String {
    if s.chars().count() <= 140 {
        s.to_owned()
    } else {
        s.chars().take(140).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn render_standalone_embeds_data() {
        let events = vec![ReplayMolecule {
            id: "task-20260412-0001".into(),
            role: "task".into(),
            topic: "hello".into(),
            nucleated_at: None,
            spawned_at: None,
            completed_at: None,
            steps: vec![],
            total_steps: 2,
            blocked_by: vec![],
            status: "pending".into(),
        }];
        let html = render_standalone(&events);
        assert!(html.contains("task-20260412-0001"));
        assert!(!html.contains("/* __COSMON_REPLAY_DATA__ */ null"));
    }

    #[test]
    fn render_fetch_sets_url() {
        let html = render_fetch("/api/replay/events.json");
        assert!(html.contains("/api/replay/events.json"));
        assert!(html.contains("__COSMON_REPLAY_DATA__")); // data placeholder untouched
    }

    #[test]
    fn build_events_empty_state() {
        let tmp = TempDir::new().unwrap();
        let events = build_events(tmp.path()).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn build_events_reads_molecules_and_events() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path();
        let mol_dir = state.join("fleets/default/molecules/task-20260412-aaaa");
        fs::create_dir_all(&mol_dir).unwrap();
        fs::write(
            mol_dir.join("state.json"),
            serde_json::to_string(&serde_json::json!({
                "id": "task-20260412-aaaa",
                "formula_id": "task-work",
                "status": "completed",
                "created_at": "2026-04-12T10:00:00Z",
                "total_steps": 2,
                "variables": {"topic": "hello"},
                "typed_links": [{"rel": "blocked_by", "source": "task-20260412-bbbb"}]
            }))
            .unwrap(),
        )
        .unwrap();

        let events_path = state.join("events.jsonl");
        let lines = [
            serde_json::json!({"timestamp": "2026-04-12T10:01:00Z", "event": {"kind": "worker_spawned", "molecule_id": "task-20260412-aaaa"}}),
            serde_json::json!({"timestamp": "2026-04-12T10:02:00Z", "event": {"kind": "molecule_evolved", "molecule_id": "task-20260412-aaaa"}}),
            serde_json::json!({"timestamp": "2026-04-12T10:03:00Z", "event": {"kind": "molecule_completed", "molecule_id": "task-20260412-aaaa"}}),
        ];
        let text = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(events_path, text).unwrap();

        let out = build_events(state).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "task-20260412-aaaa");
        assert_eq!(out[0].role, "task-work");
        assert_eq!(out[0].topic, "hello");
        assert_eq!(out[0].blocked_by, vec!["task-20260412-bbbb"]);
        assert_eq!(out[0].steps.len(), 1);
        assert!(out[0].spawned_at.is_some());
        assert!(out[0].completed_at.is_some());
    }
}
