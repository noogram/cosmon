// SPDX-License-Identifier: AGPL-3.0-only

//! `cs patrol --abandon` — the patrouille-abandon sweep.
//!
//! Folds traces the instance has **already emitted** (audit envelopes
//! under `whispers/inbox/api/`, phone-home reports under
//! `whispers/inbox/phone-home/`, PKCE auth sessions under
//! `state/auth-sessions/`, instance ledgers under `state/instances/`)
//! into **named abandonment motifs**, per tenant. No new channel is
//! created — the principle is inverted: read the signal that already
//! exists rather than emit a new one.
//!
//! The five motifs, one per persona pré-mortem:
//!
//! | motif | signature |
//! |---|---|
//! | `nucleate-sans-tackle` | nucleate envelope(s), no tackle after, then silence (Casey) |
//! | `pkce-start-sans-completed` | ≥2 auth sessions, zero `COMPLETED` (user-a) |
//! | `incarne-sans-login` | incarnated instance, zero login, ≥N 503 reports multi-sub (`Project_X`) |
//! | `rafale-4xx-puis-silence` | burst of write 4xx reports, then silence (user-b) |
//! | `decroissance-de-signalement` | a sub that used to signal stopped signalling (Dave) |
//!
//! The Dave motif carries a **higher gravity** than every other:
//! losing the one client who talks means losing the only human sensor
//! and falling back to the regime where abandonment is invisible by
//! default.
//!
//! The detector is a pure function over loaded traces + `now` so the
//! gate fixtures replay synthetic trace trees deterministically.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::Serialize;

/// Quiet window (hours): how long a sub must be silent after its last
/// trace before "puis silence" motifs fire. Default for the daily
/// patrol cadence.
pub(crate) const DEFAULT_QUIET_HOURS: u64 = 24;

/// Burst window (minutes) for the user-b motif — write-4xx reports
/// count as one rafale when they land within this span.
const BURST_WINDOW_MIN: i64 = 10;

/// Minimum 4xx reports for a rafale (user-b took three opaque 4xx
/// before reintroducing the docker-exec honteux).
const BURST_MIN_REPORTS: usize = 3;

/// Minimum 503 reports for the `Project_X` motif.
const PROJECT_X_MIN_503: usize = 3;

/// Minimum distinct subs behind the 503s for the `Project_X` motif —
/// the signature is *plural pilots*, one avatar, zero login.
const PROJECT_X_MIN_SUBS: usize = 2;

/// Minimum past signalements before the Dave decay rule can apply —
/// below this there is no trajectory to lose.
const DAVE_MIN_SIGNALS: usize = 3;

/// Decay factor: the Dave motif fires when the silence since the
/// last signalement exceeds this multiple of the historical mean
/// inter-signalement interval.
const DAVE_DECAY_FACTOR: i64 = 3;

/// Floor on the mean inter-signalement interval (seconds) for the
/// Dave motif. A rafale of reports minutes apart is a *burst* (the
/// user-b signature), not a regular signalling cadence — "signalait
/// tous les 2 jours", not "trois fois en deux minutes".
const DAVE_MIN_MEAN_GAP_SECS: i64 = 86_400;

// ---------------------------------------------------------------------------
// Trace model — what already exists on disk, nothing more.
// ---------------------------------------------------------------------------

/// One audit envelope (`whispers/inbox/api/<request_id>.json`), as
/// written by `cosmon-rpp-adapter::audit::materialize`. Parsed
/// loosely: traces are read-back artifacts, not a wire contract.
#[derive(Debug, Clone)]
pub(crate) struct EnvelopeTrace {
    pub received_at: DateTime<Utc>,
    pub verb: String,
    pub noyau: String,
    /// BLAKE3 hex of the JWT `sub` — the raw sub never lands on disk.
    pub sub_hash: String,
}

/// One phone-home report (`whispers/inbox/phone-home/<request_id>.json`),
/// as materialised by the adapter's ingest layer from the CLI's passive
/// opt-out remontée. Carries only `request_id + error code` by
/// construction.
#[derive(Debug, Clone)]
pub(crate) struct PhoneHomeTrace {
    pub reported_at: DateTime<Utc>,
    pub error_code: String,
    /// HTTP status class parsed from the error code prefix (e.g. `503`).
    pub status: Option<u16>,
    pub noyau: String,
    pub sub_hash: String,
}

/// One PKCE auth session (`state/auth-sessions/<session_id>.json`).
#[derive(Debug, Clone)]
pub(crate) struct AuthSessionTrace {
    pub state: String,
    #[allow(dead_code)]
    pub created_at: Option<DateTime<Utc>>,
}

/// Everything the sweep reads, loaded from one instance root.
#[derive(Debug, Default)]
pub(crate) struct AbandonTraces {
    pub envelopes: Vec<EnvelopeTrace>,
    pub phone_home: Vec<PhoneHomeTrace>,
    pub auth_sessions: Vec<AuthSessionTrace>,
    /// True when any instance ledger carries an `IncarnationAt` event.
    pub incarnated: bool,
}

// ---------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------

/// Stable motif labels — these are the names the operator greps for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum AbandonMotif {
    /// Casey — nucleate succeeded, nothing ever descended.
    NucleateSansTackle,
    /// user-a — PKCE `start` repeated, never `COMPLETED`.
    PkceStartSansCompleted,
    /// `Project_X` — incarnated avatar, zero login, plural subs in 503.
    IncarneSansLogin,
    /// user-b — burst of write 4xx, then silence.
    Rafale4xxPuisSilence,
    /// Dave — a client who signalled regularly went quiet.
    DecroissanceDeSignalement,
}

impl AbandonMotif {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NucleateSansTackle => "nucleate-sans-tackle",
            Self::PkceStartSansCompleted => "pkce-start-sans-completed",
            Self::IncarneSansLogin => "incarne-sans-login",
            Self::Rafale4xxPuisSilence => "rafale-4xx-puis-silence",
            Self::DecroissanceDeSignalement => "decroissance-de-signalement",
        }
    }
}

/// Gravity tiers. `High` is reserved for the Dave motif — losing
/// the dissident is graver than losing a client who never spoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AbandonGravity {
    Watch,
    High,
}

/// One detected abandonment motif, per tenant trajectory.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AbandonFinding {
    pub motif: AbandonMotif,
    pub gravity: AbandonGravity,
    /// Sub trajectory the motif belongs to (`None` for instance-wide
    /// motifs like the PKCE completion-rate).
    pub sub_hash: Option<String>,
    pub noyau: Option<String>,
    /// Human-readable evidence — names the trace, never re-asserts it.
    pub evidence: String,
}

/// Aggregate sweep report.
#[derive(Debug, Default, Serialize)]
pub(crate) struct AbandonReport {
    pub envelopes_read: usize,
    pub phone_home_read: usize,
    pub auth_sessions_read: usize,
    pub findings: Vec<AbandonFinding>,
}

// ---------------------------------------------------------------------------
// Loader — replays the trace tree under one instance root.
// ---------------------------------------------------------------------------

fn parse_ts(v: &serde_json::Value, key: &str) -> Option<DateTime<Utc>> {
    v.get(key)
        .and_then(|x| x.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

fn str_of(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_owned()
}

fn read_json_dir(dir: &Path) -> Vec<serde_json::Value> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                out.push(v);
            }
        }
    }
    out
}

/// Load every trace family from one instance root. The root is the
/// instance's `.cosmon/` directory: `whispers/inbox/{api,phone-home}/`
/// and `state/{auth-sessions,instances}/` live under it.
pub(crate) fn load_traces(root: &Path) -> AbandonTraces {
    let mut traces = AbandonTraces::default();

    for v in read_json_dir(&root.join("whispers").join("inbox").join("api")) {
        let Some(received_at) = parse_ts(&v, "received_at") else {
            continue;
        };
        traces.envelopes.push(EnvelopeTrace {
            received_at,
            verb: str_of(&v, "verb"),
            noyau: str_of(&v, "noyau"),
            sub_hash: v
                .get("claims")
                .map(|c| str_of(c, "sub_hash"))
                .unwrap_or_default(),
        });
    }

    for v in read_json_dir(&root.join("whispers").join("inbox").join("phone-home")) {
        let Some(reported_at) = parse_ts(&v, "reported_at") else {
            continue;
        };
        let error_code = str_of(&v, "error_code");
        let status = error_code
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|s| (100..=599).contains(s));
        traces.phone_home.push(PhoneHomeTrace {
            reported_at,
            error_code,
            status,
            noyau: str_of(&v, "noyau"),
            sub_hash: str_of(&v, "sub_hash"),
        });
    }

    for v in read_json_dir(&root.join("state").join("auth-sessions")) {
        traces.auth_sessions.push(AuthSessionTrace {
            state: str_of(&v, "state"),
            created_at: parse_ts(&v, "created_at"),
        });
    }

    let instances_dir = root.join("state").join("instances");
    if let Ok(entries) = std::fs::read_dir(&instances_dir) {
        for entry in entries.flatten() {
            let ledger = entry.path().join("events.jsonl");
            if let Ok(text) = std::fs::read_to_string(&ledger) {
                if text.lines().any(|l| l.contains("IncarnationAt")) {
                    traces.incarnated = true;
                }
            }
        }
    }

    traces
}

// ---------------------------------------------------------------------------
// Detector — pure over (traces, now, quiet window).
// ---------------------------------------------------------------------------

/// Fold the loaded traces into named abandonment findings.
#[allow(clippy::too_many_lines)]
pub(crate) fn detect(
    traces: &AbandonTraces,
    now: DateTime<Utc>,
    quiet_hours: u64,
) -> Vec<AbandonFinding> {
    let quiet = chrono::Duration::hours(i64::try_from(quiet_hours).unwrap_or(24));
    let mut findings = Vec::new();

    // Group envelopes per sub trajectory — the session is an object,
    // the trajectory is a person (janis pré-mortem c).
    let mut by_sub: BTreeMap<&str, Vec<&EnvelopeTrace>> = BTreeMap::new();
    for e in &traces.envelopes {
        if !e.sub_hash.is_empty() {
            by_sub.entry(&e.sub_hash).or_default().push(e);
        }
    }
    for v in by_sub.values_mut() {
        v.sort_by_key(|e| e.received_at);
    }

    // (1) nucleate-sans-tackle — Casey. A nucleate with no tackle at or
    // after it, and the whole trajectory silent past the quiet window:
    // the success that deceives (201, then nothing descends).
    for (sub, envs) in &by_sub {
        let Some(first_nucleate) = envs
            .iter()
            .find(|e| e.verb == "nucleate")
            .map(|e| e.received_at)
        else {
            continue;
        };
        let tackled_after = envs
            .iter()
            .any(|e| e.verb == "tackle" && e.received_at >= first_nucleate);
        let last_seen = envs.last().map_or(first_nucleate, |e| e.received_at);
        if !tackled_after && now - last_seen > quiet {
            let nucleates = envs.iter().filter(|e| e.verb == "nucleate").count();
            findings.push(AbandonFinding {
                motif: AbandonMotif::NucleateSansTackle,
                gravity: AbandonGravity::Watch,
                sub_hash: Some((*sub).to_owned()),
                noyau: envs.first().map(|e| e.noyau.clone()),
                evidence: format!(
                    "{nucleates} nucleate envelope(s), zero tackle after the first, \
                     last trace {}h ago",
                    (now - last_seen).num_hours()
                ),
            });
        }
    }

    // (2) pkce-start-sans-completed — user-a. The PKCE completion rate
    // is the most predictive early-abandon signal; the store only keeps
    // sessions, so the motif is instance-wide.
    let completed = traces
        .auth_sessions
        .iter()
        .filter(|s| s.state == "COMPLETED")
        .count();
    let attempts = traces.auth_sessions.len();
    if completed == 0 && attempts >= 2 {
        findings.push(AbandonFinding {
            motif: AbandonMotif::PkceStartSansCompleted,
            gravity: AbandonGravity::Watch,
            sub_hash: None,
            noyau: None,
            evidence: format!(
                "{attempts} PKCE session(s) started, zero COMPLETED — \
                 completion rate 0%"
            ),
        });
    }

    // (3) incarne-sans-login — Project_X. An incarnated avatar, zero
    // completed login, and 503s reported from plural subs: the exact
    // signature of a collective team abandon.
    if traces.incarnated && completed == 0 {
        let five03: Vec<&PhoneHomeTrace> = traces
            .phone_home
            .iter()
            .filter(|r| r.status == Some(503))
            .collect();
        let distinct_subs: std::collections::BTreeSet<&str> = five03
            .iter()
            .filter(|r| !r.sub_hash.is_empty())
            .map(|r| r.sub_hash.as_str())
            .collect();
        if five03.len() >= PROJECT_X_MIN_503 && distinct_subs.len() >= PROJECT_X_MIN_SUBS {
            findings.push(AbandonFinding {
                motif: AbandonMotif::IncarneSansLogin,
                gravity: AbandonGravity::Watch,
                sub_hash: None,
                noyau: five03.first().map(|r| r.noyau.clone()),
                evidence: format!(
                    "instance incarnated, zero COMPLETED login, {} × 503 \
                     reported from {} distinct sub(s)",
                    five03.len(),
                    distinct_subs.len()
                ),
            });
        }
    }

    // (4) rafale-4xx-puis-silence — user-b. ≥3 write-4xx reports inside
    // the burst window from one sub, and no envelope from that sub
    // since the burst for the quiet window.
    let mut fourxx_by_sub: BTreeMap<&str, Vec<&PhoneHomeTrace>> = BTreeMap::new();
    for r in &traces.phone_home {
        if r.status.is_some_and(|s| (400..500).contains(&s)) && !r.sub_hash.is_empty() {
            fourxx_by_sub.entry(&r.sub_hash).or_default().push(r);
        }
    }
    for (sub, mut reports) in fourxx_by_sub {
        reports.sort_by_key(|r| r.reported_at);
        let burst = reports
            .windows(BURST_MIN_REPORTS)
            .find(|w| {
                (w[BURST_MIN_REPORTS - 1].reported_at - w[0].reported_at)
                    <= chrono::Duration::minutes(BURST_WINDOW_MIN)
            })
            .map(|w| w[BURST_MIN_REPORTS - 1].reported_at);
        let Some(burst_end) = burst else { continue };
        let active_after = by_sub
            .get(sub)
            .is_some_and(|envs| envs.iter().any(|e| e.received_at > burst_end));
        if !active_after && now - burst_end > quiet {
            findings.push(AbandonFinding {
                motif: AbandonMotif::Rafale4xxPuisSilence,
                gravity: AbandonGravity::Watch,
                sub_hash: Some(sub.to_owned()),
                noyau: reports.first().map(|r| r.noyau.clone()),
                evidence: format!(
                    "{} write-4xx report(s) within {BURST_WINDOW_MIN} min \
                     ({}), then silence for {}h",
                    reports.len(),
                    reports
                        .iter()
                        .map(|r| r.error_code.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    (now - burst_end).num_hours()
                ),
            });
        }
    }

    // (5) decroissance-de-signalement — Dave. Named rule: a sub that
    // signalled regularly and stopped is GRAVER than a sub that never
    // signalled — losing the dissident means losing the only human
    // sensor. Signalements = phone-home reports ∪ `stuck` envelopes.
    let mut signals_by_sub: BTreeMap<&str, Vec<DateTime<Utc>>> = BTreeMap::new();
    for r in &traces.phone_home {
        if !r.sub_hash.is_empty() {
            signals_by_sub
                .entry(&r.sub_hash)
                .or_default()
                .push(r.reported_at);
        }
    }
    for e in &traces.envelopes {
        if e.verb == "stuck" && !e.sub_hash.is_empty() {
            signals_by_sub
                .entry(&e.sub_hash)
                .or_default()
                .push(e.received_at);
        }
    }
    for (sub, mut times) in signals_by_sub {
        if times.len() < DAVE_MIN_SIGNALS {
            continue;
        }
        times.sort();
        let mean_gap_secs = {
            let total: i64 = times.windows(2).map(|w| (w[1] - w[0]).num_seconds()).sum();
            total / i64::try_from(times.len() - 1).unwrap_or(1)
        };
        if mean_gap_secs < DAVE_MIN_MEAN_GAP_SECS {
            continue;
        }
        let silence_secs = (now - *times.last().expect("non-empty")).num_seconds();
        if silence_secs > DAVE_DECAY_FACTOR * mean_gap_secs {
            findings.push(AbandonFinding {
                motif: AbandonMotif::DecroissanceDeSignalement,
                gravity: AbandonGravity::High,
                sub_hash: Some(sub.to_owned()),
                noyau: None,
                evidence: format!(
                    "{} signalement(s), mean interval {}h, now silent for \
                     {}h (> {DAVE_DECAY_FACTOR}× the mean) — losing the \
                     dissident loses the only human sensor",
                    times.len(),
                    mean_gap_secs / 3600,
                    silence_secs / 3600
                ),
            });
        }
    }

    findings.sort_by_key(|x| std::cmp::Reverse(x.gravity));
    findings
}

/// Run the full sweep against one instance root.
pub(crate) fn abandon_sweep(root: &Path, now: DateTime<Utc>, quiet_hours: u64) -> AbandonReport {
    let traces = load_traces(root);
    let findings = detect(&traces, now, quiet_hours);
    AbandonReport {
        envelopes_read: traces.envelopes.len(),
        phone_home_read: traces.phone_home.len(),
        auth_sessions_read: traces.auth_sessions.len(),
        findings,
    }
}

/// Human-readable section of the patrol report.
pub(crate) fn print_abandon_report(report: &AbandonReport, root: &Path) {
    println!();
    let banner = "ABANDON".cyan().bold();
    println!(
        "  {banner} traces under {}: {} envelope(s), {} phone-home report(s), \
         {} auth session(s)",
        root.display(),
        report.envelopes_read,
        report.phone_home_read,
        report.auth_sessions_read,
    );
    if report.findings.is_empty() {
        println!("    no abandonment motif detected");
        return;
    }
    for f in &report.findings {
        let tag = match f.gravity {
            AbandonGravity::High => "HIGH ".red().bold(),
            AbandonGravity::Watch => "watch".yellow(),
        };
        let who = f.sub_hash.as_deref().map_or_else(
            || "instance-wide".to_owned(),
            |s| format!("sub {}", &s[..s.len().min(12)]),
        );
        println!("    [{tag}] {} ({who}) — {}", f.motif.label(), f.evidence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::path::PathBuf;

    fn ts(day: u32, hour: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, day, hour, min, 0).unwrap()
    }

    /// Fixture builder: writes a synthetic trace tree (the exact same
    /// layout the adapter materialises) under a temp root, then the
    /// tests replay it through `load_traces` + `detect` — the gate is
    /// "chaque motif détecté sur des traces synthétiques rejouées".
    struct FixtureRoot {
        td: tempfile::TempDir,
    }

    impl FixtureRoot {
        fn new() -> Self {
            Self {
                td: tempfile::TempDir::new().unwrap(),
            }
        }

        fn root(&self) -> PathBuf {
            self.td.path().to_path_buf()
        }

        fn envelope(&self, request_id: &str, at: DateTime<Utc>, verb: &str, sub_hash: &str) {
            let dir = self.root().join("whispers/inbox/api");
            std::fs::create_dir_all(&dir).unwrap();
            let body = serde_json::json!({
                "request_id": request_id,
                "received_at": at.to_rfc3339(),
                "nucleon_id": "nuc-test",
                "noyau": "tenant-test",
                "verb": verb,
                "molecule_id": null,
                "claims": {
                    "iss": "https://idp",
                    "sub_hash": sub_hash,
                    "aud": "cosmon-rpp-tenant",
                    "jti": "tok",
                    "lifetime_sec": 60
                }
            });
            std::fs::write(
                dir.join(format!("{request_id}.json")),
                serde_json::to_vec_pretty(&body).unwrap(),
            )
            .unwrap();
        }

        fn phone_home(&self, request_id: &str, at: DateTime<Utc>, code: &str, sub_hash: &str) {
            let dir = self.root().join("whispers/inbox/phone-home");
            std::fs::create_dir_all(&dir).unwrap();
            let body = serde_json::json!({
                "reported_request_id": request_id,
                "error_code": code,
                "noyau": "tenant-test",
                "sub_hash": sub_hash,
                "reported_at": at.to_rfc3339(),
            });
            std::fs::write(
                dir.join(format!("{request_id}.json")),
                serde_json::to_vec_pretty(&body).unwrap(),
            )
            .unwrap();
        }

        fn auth_session(&self, session_id: &str, state: &str, at: DateTime<Utc>) {
            let dir = self.root().join("state/auth-sessions");
            std::fs::create_dir_all(&dir).unwrap();
            let body = serde_json::json!({
                "session_id": session_id,
                "created_at": at.to_rfc3339(),
                "ttl_at": (at + chrono::Duration::minutes(30)).to_rfc3339(),
                "state": state,
            });
            std::fs::write(
                dir.join(format!("{session_id}.json")),
                serde_json::to_vec_pretty(&body).unwrap(),
            )
            .unwrap();
        }

        fn incarnate(&self, instance_id: &str, at: DateTime<Utc>) {
            let dir = self.root().join("state/instances").join(instance_id);
            std::fs::create_dir_all(&dir).unwrap();
            let line = serde_json::json!({
                "IncarnationAt": { "ts": at.to_rfc3339(), "instance_id": instance_id }
            });
            std::fs::write(dir.join("events.jsonl"), format!("{line}\n")).unwrap();
        }
    }

    fn motifs(findings: &[AbandonFinding]) -> Vec<&'static str> {
        findings.iter().map(|f| f.motif.label()).collect()
    }

    // Fixture 1 — Casey: one nucleate, one earlier tackle-free
    // trajectory, then 6 days of silence with pending orphans.
    #[test]
    fn fixture_jesse_nucleate_sans_tackle() {
        let fx = FixtureRoot::new();
        fx.envelope("req-j1", ts(1, 10, 0), "nucleate", "sub-casey");
        fx.envelope("req-j2", ts(1, 10, 5), "observe", "sub-casey");
        let report = abandon_sweep(&fx.root(), ts(7, 10, 0), DEFAULT_QUIET_HOURS);
        assert_eq!(motifs(&report.findings), vec!["nucleate-sans-tackle"]);
        let f = &report.findings[0];
        assert_eq!(f.sub_hash.as_deref(), Some("sub-casey"));
        assert_eq!(f.gravity, AbandonGravity::Watch);
    }

    // Counter-fixture: a tackle after the nucleate defuses the motif.
    #[test]
    fn jesse_motif_defused_by_tackle() {
        let fx = FixtureRoot::new();
        fx.envelope("req-j1", ts(1, 10, 0), "nucleate", "sub-casey");
        fx.envelope("req-j2", ts(1, 10, 5), "tackle", "sub-casey");
        let report = abandon_sweep(&fx.root(), ts(7, 10, 0), DEFAULT_QUIET_HOURS);
        assert!(report.findings.is_empty());
    }

    // Fixture 2 — user-a: three PKCE sessions die before COMPLETED
    // (the truncated paste-back → 502 → retry → close the terminal).
    #[test]
    fn fixture_user_a_pkce_start_sans_completed() {
        let fx = FixtureRoot::new();
        fx.auth_session("auth-20260601-aaaaaa", "FAILED", ts(1, 9, 0));
        fx.auth_session("auth-20260601-bbbbbb", "FAILED", ts(1, 9, 10));
        fx.auth_session("auth-20260601-cccccc", "EXPIRED", ts(1, 9, 20));
        let report = abandon_sweep(&fx.root(), ts(2, 9, 0), DEFAULT_QUIET_HOURS);
        assert_eq!(motifs(&report.findings), vec!["pkce-start-sans-completed"]);
    }

    // Counter-fixture: one COMPLETED session means the badge landed.
    #[test]
    fn user_a_motif_defused_by_completed() {
        let fx = FixtureRoot::new();
        fx.auth_session("auth-20260601-aaaaaa", "FAILED", ts(1, 9, 0));
        fx.auth_session("auth-20260601-bbbbbb", "COMPLETED", ts(1, 9, 10));
        let report = abandon_sweep(&fx.root(), ts(2, 9, 0), DEFAULT_QUIET_HOURS);
        assert!(report.findings.is_empty());
    }

    // Fixture 3 — Project_X: incarnated avatar, zero login, 503s
    // reported from three distinct subs (plural pilots, one avatar).
    #[test]
    fn fixture_project_x_incarne_sans_login() {
        let fx = FixtureRoot::new();
        fx.incarnate("avatar-project_x", ts(1, 8, 0));
        fx.phone_home(
            "req-r1",
            ts(1, 9, 0),
            "503 tackle_unavailable",
            "sub-r-tenant_auditor",
        );
        fx.phone_home(
            "req-r2",
            ts(1, 9, 30),
            "503 tackle_unavailable",
            "sub-r-bob",
        );
        fx.phone_home(
            "req-r3",
            ts(1, 11, 0),
            "503 tackle_unavailable",
            "sub-r-carol",
        );
        let report = abandon_sweep(&fx.root(), ts(3, 9, 0), DEFAULT_QUIET_HOURS);
        assert_eq!(motifs(&report.findings), vec!["incarne-sans-login"]);
    }

    // Counter-fixture: a single sub hammering 503 is not the team
    // signature (that is an individual blocked on auth, not bystander
    // diffusion across plural pilots).
    #[test]
    fn project_x_motif_needs_plural_subs() {
        let fx = FixtureRoot::new();
        fx.incarnate("avatar-project_x", ts(1, 8, 0));
        fx.phone_home("req-r1", ts(1, 9, 0), "503 tackle_unavailable", "sub-solo");
        fx.phone_home("req-r2", ts(1, 9, 30), "503 tackle_unavailable", "sub-solo");
        fx.phone_home("req-r3", ts(1, 11, 0), "503 tackle_unavailable", "sub-solo");
        let report = abandon_sweep(&fx.root(), ts(3, 9, 0), DEFAULT_QUIET_HOURS);
        assert!(!motifs(&report.findings).contains(&"incarne-sans-login"));
    }

    // Fixture 4 — user-b: three write-4xx within two minutes, then
    // silence (he asked someone to "le mettre dedans" — docker exec).
    #[test]
    fn fixture_user_b_rafale_4xx_puis_silence() {
        let fx = FixtureRoot::new();
        fx.envelope("req-u0", ts(1, 13, 50), "observe", "sub-user-b");
        fx.phone_home("req-u1", ts(1, 14, 0), "409 reserved_name", "sub-user-b");
        fx.phone_home("req-u2", ts(1, 14, 1), "400 invalid_name", "sub-user-b");
        fx.phone_home("req-u3", ts(1, 14, 2), "409 reserved_name", "sub-user-b");
        let report = abandon_sweep(&fx.root(), ts(4, 9, 0), DEFAULT_QUIET_HOURS);
        assert_eq!(motifs(&report.findings), vec!["rafale-4xx-puis-silence"]);
    }

    // Counter-fixture: activity after the burst defuses the silence.
    #[test]
    fn user_b_motif_defused_by_later_activity() {
        let fx = FixtureRoot::new();
        fx.phone_home("req-u1", ts(1, 14, 0), "409 reserved_name", "sub-user-b");
        fx.phone_home("req-u2", ts(1, 14, 1), "400 invalid_name", "sub-user-b");
        fx.phone_home("req-u3", ts(1, 14, 2), "409 reserved_name", "sub-user-b");
        fx.envelope("req-u4", ts(2, 9, 0), "tackle", "sub-user-b");
        let report = abandon_sweep(&fx.root(), ts(4, 9, 0), DEFAULT_QUIET_HOURS);
        assert!(!motifs(&report.findings).contains(&"rafale-4xx-puis-silence"));
    }

    // Fixture 5 — Dave: signalled every ~2 days three times, then
    // 9 days of silence. Gravity HIGH — graver than never-signalled.
    #[test]
    fn fixture_dave_decroissance_de_signalement() {
        let fx = FixtureRoot::new();
        fx.envelope("req-m1", ts(1, 10, 0), "stuck", "sub-dave");
        fx.envelope("req-m2", ts(3, 10, 0), "stuck", "sub-dave");
        fx.phone_home("req-m3", ts(5, 10, 0), "503 tackle_unavailable", "sub-dave");
        // Keep the trajectory otherwise active so casey/user-b stay out.
        fx.envelope("req-m4", ts(5, 10, 5), "tackle", "sub-dave");
        let report = abandon_sweep(&fx.root(), ts(14, 10, 0), DEFAULT_QUIET_HOURS);
        assert_eq!(
            motifs(&report.findings),
            vec!["decroissance-de-signalement"]
        );
        assert_eq!(report.findings[0].gravity, AbandonGravity::High);
    }

    // Counter-fixture: silence within 3× the mean interval is normal
    // cadence, not decay.
    #[test]
    fn dave_motif_needs_real_decay() {
        let fx = FixtureRoot::new();
        fx.envelope("req-m1", ts(1, 10, 0), "stuck", "sub-dave");
        fx.envelope("req-m2", ts(3, 10, 0), "stuck", "sub-dave");
        fx.envelope("req-m3", ts(5, 10, 0), "stuck", "sub-dave");
        fx.envelope("req-m4", ts(5, 10, 5), "tackle", "sub-dave");
        let report = abandon_sweep(&fx.root(), ts(9, 10, 0), DEFAULT_QUIET_HOURS);
        assert!(!motifs(&report.findings).contains(&"decroissance-de-signalement"));
    }

    // The Dave gravity strictly dominates every other motif's.
    #[test]
    fn dave_gravity_is_higher_and_sorted_first() {
        let fx = FixtureRoot::new();
        // Casey trajectory (watch).
        fx.envelope("req-j1", ts(1, 10, 0), "nucleate", "sub-casey");
        // Dave trajectory (high).
        fx.envelope("req-m1", ts(1, 10, 0), "stuck", "sub-dave");
        fx.envelope("req-m2", ts(3, 10, 0), "stuck", "sub-dave");
        fx.envelope("req-m3", ts(5, 10, 0), "stuck", "sub-dave");
        fx.envelope("req-m4", ts(5, 10, 5), "tackle", "sub-dave");
        let report = abandon_sweep(&fx.root(), ts(20, 10, 0), DEFAULT_QUIET_HOURS);
        assert!(report.findings.len() >= 2);
        assert_eq!(report.findings[0].gravity, AbandonGravity::High);
        assert_eq!(
            report.findings[0].motif.label(),
            "decroissance-de-signalement"
        );
        assert!(AbandonGravity::High > AbandonGravity::Watch);
    }

    // An empty root yields an empty report — the sweep never invents
    // signal where no trace exists.
    #[test]
    fn empty_root_is_silent() {
        let fx = FixtureRoot::new();
        let report = abandon_sweep(&fx.root(), ts(1, 0, 0), DEFAULT_QUIET_HOURS);
        assert_eq!(report.envelopes_read, 0);
        assert!(report.findings.is_empty());
    }
}
