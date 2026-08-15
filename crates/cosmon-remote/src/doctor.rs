// SPDX-License-Identifier: AGPL-3.0-only

//! `cosmon-remote doctor` — named green/red onboarding checks.
//!
//! « Onboarding sans marche cassée » : three of the five client personas die on invisible
//! prerequisites *before* the first useful command — the network wall,
//! the oidc-url wall (Dave n°2), and the two-badges trap (the tenant
//! JWT vs the worker's Claude login). `doctor` makes each prerequisite
//! a **named check** that is green or red on its own, with the exact
//! repair command on the red line.
//!
//! Design rules (anti-cascade):
//!
//! - **Each check is independently falsifiable** — one cause, one red
//!   line. Break the oidc-url and only `oidc-mint` goes red.
//! - **A check whose prerequisite failed is `Skipped`, not red** — a
//!   cascade of reds hides the single real cause.
//! - **No check fabricates its verdict** — every probe reads a signal
//!   that exists independently of this binary (`/healthz` body, the
//!   issuer's HTTP status, `/v1/auth/me`'s `claude_credentials_present`
//!   which the server derives by reading the credentials file the PKCE
//!   confirm handler writes and asking whether a worker could start
//!   with it).
//!
//! The module is UI-free: [`run`] returns a [`DoctorReport`] the binary
//! renders (text or `--json`). Tests drive [`run`] against a wiremock
//! server and provoke each red state independently.

use serde::Serialize;

use crate::client::Client;
use crate::config::Profile;

/// Stable check names — these are the vocabulary of the onboarding
/// conversation (install.sh prints them, the 503 hint references
/// `doctor`), so they are constants rather than ad-hoc strings.
pub const CHECK_PROFILE: &str = "profile";
pub const CHECK_HOST: &str = "host-reachable";
pub const CHECK_OIDC: &str = "oidc-mint";
pub const CHECK_TENANT_BADGE: &str = "badge-tenant";
pub const CHECK_WORKER_GLASSES: &str = "badge-worker-claude";

/// Outcome of one named check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Green — the probed signal is the expected one.
    Pass,
    /// Red — the probe ran and the signal contradicts the expectation.
    Fail,
    /// Not probed — a prerequisite check failed, so probing this one
    /// would only duplicate the same root cause as a second red line.
    Skipped,
    /// Probed, but the server does not publish the signal (older
    /// adapter) — honest "cannot know", never coerced to green or red.
    Unknown,
}

/// One named check with its outcome, a human detail line, and — on
/// red — the repair command.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// Stable name (one of the `CHECK_*` constants).
    pub name: &'static str,
    pub outcome: Outcome,
    /// What was probed and what came back, one line.
    pub detail: String,
    /// The repair gesture, present iff the outcome calls for one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

/// The full doctor report — ordered checks, plus the aggregate the
/// caller turns into an exit code.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub checks: Vec<Check>,
}

impl DoctorReport {
    /// True iff no check is red. `Skipped`/`Unknown` do not fail the
    /// report on their own — the red line they depend on already does.
    #[must_use]
    pub fn healthy(&self) -> bool {
        self.checks.iter().all(|c| c.outcome != Outcome::Fail)
    }
}

/// Run the five onboarding checks against `profile`. Network probes
/// reuse the same [`Client`] paths the real verbs use — doctor tests
/// the road the tenant will actually drive, not a parallel one.
pub async fn run(profile: &Profile) -> DoctorReport {
    let mut checks = Vec::with_capacity(5);
    let profile_ok = check_profile(profile, &mut checks);
    let host_ok = check_host(profile, &mut checks).await;
    let minted = check_oidc_mint(profile, profile_ok, &mut checks).await;
    match minted {
        Some(client) if host_ok => check_badges(&client, &mut checks).await,
        _ => push_skipped_badges(&mut checks),
    }
    DoctorReport { checks }
}

/// ── 1. profile — local completeness, no network.
fn check_profile(profile: &Profile, checks: &mut Vec<Check>) -> bool {
    match profile.check_ready() {
        Ok(()) => {
            checks.push(Check {
                name: CHECK_PROFILE,
                outcome: Outcome::Pass,
                detail: "required fields present (host, sub, aud, oidc_url)".to_owned(),
                fix: None,
            });
            true
        }
        Err(e) => {
            checks.push(Check {
                name: CHECK_PROFILE,
                outcome: Outcome::Fail,
                detail: e.to_string(),
                fix: Some(
                    "cosmon-remote config set <key> <value> — or re-run install.sh \
                     from the host to re-template the profile"
                        .to_owned(),
                ),
            });
            false
        }
    }
}

/// ── 2. host-reachable — GET /healthz, unauthenticated. Only needs
/// `host`, so it runs even when the profile is incomplete: a missing
/// `sub` must not mask a network wall (one cause, one red line — and
/// vice versa).
async fn check_host(profile: &Profile, checks: &mut Vec<Check>) -> bool {
    if profile.host.is_empty() {
        checks.push(Check {
            name: CHECK_HOST,
            outcome: Outcome::Skipped,
            detail: "not tested — `host` missing from the profile".to_owned(),
            fix: None,
        });
        return false;
    }
    let probe = match Client::new_unchecked(profile, None) {
        Ok(client) => client.healthz().await.map(|_| ()),
        Err(e) => Err(e),
    };
    match probe {
        Ok(()) => {
            checks.push(Check {
                name: CHECK_HOST,
                outcome: Outcome::Pass,
                detail: format!("{} responds (healthz ok)", profile.host),
                fix: None,
            });
            true
        }
        Err(e) => {
            checks.push(Check {
                name: CHECK_HOST,
                outcome: Outcome::Fail,
                detail: format!("{} unreachable: {e}", profile.host),
                fix: Some(
                    "check the sovereign network (Tailscale connected? ACL in place?) \
                     then `cosmon-remote config show` for the exact host"
                        .to_owned(),
                ),
            });
            false
        }
    }
}

/// ── 3. oidc-mint — mint a JWT via the profile's issuer (the Dave
/// wall n°2: an oidc_url templated for another host). Least-privilege
/// scope: read-only, no spawn. Returns the authenticated client the
/// badge checks reuse.
async fn check_oidc_mint(
    profile: &Profile,
    profile_ok: bool,
    checks: &mut Vec<Check>,
) -> Option<Client> {
    if !profile_ok {
        checks.push(Check {
            name: CHECK_OIDC,
            outcome: Outcome::Skipped,
            detail: "not tested — fix `profile` first".to_owned(),
            fix: None,
        });
        return None;
    }
    let probe = match Client::new_unchecked(profile, None) {
        Ok(client) => client
            .mint_jwt(&["cosmon:molecule:read".to_owned()])
            .await
            .map(|minted| client.with_token(minted.access_token)),
        Err(e) => Err(e),
    };
    match probe {
        Ok(client) => {
            checks.push(Check {
                name: CHECK_OIDC,
                outcome: Outcome::Pass,
                detail: format!("token minted via {}", profile.oidc_url),
                fix: None,
            });
            Some(client)
        }
        Err(e) => {
            checks.push(Check {
                name: CHECK_OIDC,
                outcome: Outcome::Fail,
                detail: format!("mint failed via {}: {e}", profile.oidc_url),
                fix: Some(
                    "cosmon-remote config show — the `oidc-url` must point to YOUR \
                     deployment's issuer (re-run install.sh from the host if it was \
                     templated for another machine)"
                        .to_owned(),
                ),
            });
            None
        }
    }
}

/// ── 4 + 5. badge-tenant / badge-worker-claude — one authenticated
/// `GET /v1/auth/me` answers both: « le serveur accepte-t-il mon
/// badge ? » and « le worker a-t-il ses lunettes ? ».
async fn check_badges(client: &Client, checks: &mut Vec<Check>) {
    let me = match client.auth_me().await {
        Ok(me) => me,
        Err(e) => {
            checks.push(Check {
                name: CHECK_TENANT_BADGE,
                outcome: Outcome::Fail,
                detail: format!("/v1/auth/me rejects the token: {e}"),
                fix: Some(
                    "check `sub` and `aud` (cosmon-remote config show) — they must \
                     match the binding posed by the operator"
                        .to_owned(),
                ),
            });
            checks.push(Check {
                name: CHECK_WORKER_GLASSES,
                outcome: Outcome::Skipped,
                detail: "not tested — fix `badge-tenant` first".to_owned(),
                fix: None,
            });
            return;
        }
    };
    match me.noyau.as_deref() {
        Some(noyau) => checks.push(Check {
            name: CHECK_TENANT_BADGE,
            outcome: Outcome::Pass,
            detail: format!("badge accepted — sub={}, noyau={noyau}", me.sub),
            fix: None,
        }),
        None => checks.push(Check {
            name: CHECK_TENANT_BADGE,
            outcome: Outcome::Fail,
            detail: format!(
                "the server accepts the token (sub={}) but no noyau is \
                 bound to this principal",
                me.sub
            ),
            fix: Some(
                "the binding (sub ↔ noyau) is an operator gesture — raise it \
                 with your instance's operator"
                    .to_owned(),
            ),
        }),
    }
    checks.push(worker_glasses_check(&me));
}

/// The badge-worker-claude line, derived from `/v1/auth/me`'s
/// worker-glasses pair.
///
/// Split out of [`check_badges`] because the six statuses issue #48
/// introduced each carry their own sentence — and because the mapping
/// « status → what the tenant should do » is the part worth reading on
/// its own.
fn worker_glasses_check(me: &crate::client::AuthMeResponse) -> Check {
    match me.claude_credentials_present {
        Some(true) => Check {
            name: CHECK_WORKER_GLASSES,
            outcome: Outcome::Pass,
            detail: match me.claude_credentials_status.as_deref() {
                Some("refreshable") => "the Claude worker is connected (access token \
                     expired, renewed from the stored refresh token on first use)"
                    .to_owned(),
                _ => "the Claude worker is connected (credentials usable)".to_owned(),
            },
            fix: None,
        },
        Some(false) => Check {
            name: CHECK_WORKER_GLASSES,
            outcome: Outcome::Fail,
            // The status names *which* precondition failed, so the
            // line points at the cause instead of printing one
            // undifferentiated « not connected » for every shape of
            // it. Issue #48: an unreadable file is an operator-side
            // filesystem problem and no amount of `auth login` fixes
            // it — saying so is the whole point of the field.
            detail: match me.claude_credentials_status.as_deref() {
                Some("absent") => "the Claude worker is NOT connected — no credentials \
                     file; every `tackle` will fail with 503"
                    .to_owned(),
                Some("expired") => "the Claude worker is NOT connected — the stored token \
                     expired and carries no refresh token; every `tackle` will fail \
                     with 503"
                    .to_owned(),
                Some("malformed") => "the Claude worker is NOT connected — the credentials \
                     file exists but holds no usable token; every `tackle` will fail \
                     with 503"
                    .to_owned(),
                Some("unreadable") => "the Claude worker is NOT connected — the credentials \
                     path exists but cannot be read (permissions, or a directory in its \
                     place)"
                    .to_owned(),
                _ => "the Claude worker is NOT connected — every `tackle` will fail with 503"
                    .to_owned(),
            },
            fix: Some(match me.claude_credentials_status.as_deref() {
                Some("unreadable") => "the file is there but unreadable — this is an \
                     operator-side fix on the container (ownership/mode of \
                     ~/.claude/.credentials.json); `auth login` will not repair it"
                    .to_owned(),
                _ => "cosmon-remote auth login --email you@example.com  (once; \
                     this is the second badge — distinct from your tenant token)"
                    .to_owned(),
            }),
        },
        None => Check {
            name: CHECK_WORKER_GLASSES,
            outcome: Outcome::Unknown,
            detail: "the server does not publish this signal (older adapter, or \
                     auth-claude surface not configured)"
                .to_owned(),
            fix: Some(
                "if your first `molecule tackle` returns 503: \
                 cosmon-remote auth login --email you@example.com"
                    .to_owned(),
            ),
        },
    }
}

/// The two badge checks depend on a minted token AND a reachable host;
/// when either prerequisite is red they are skipped with a pointer to
/// the real cause, never turned into duplicate red lines.
fn push_skipped_badges(checks: &mut Vec<Check>) {
    for name in [CHECK_TENANT_BADGE, CHECK_WORKER_GLASSES] {
        checks.push(Check {
            name,
            outcome: Outcome::Skipped,
            detail: "not tested — fix the red checks above first".to_owned(),
            fix: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_iff_no_fail() {
        let mut report = DoctorReport {
            checks: vec![Check {
                name: CHECK_PROFILE,
                outcome: Outcome::Pass,
                detail: String::new(),
                fix: None,
            }],
        };
        assert!(report.healthy());
        report.checks.push(Check {
            name: CHECK_WORKER_GLASSES,
            outcome: Outcome::Unknown,
            detail: String::new(),
            fix: None,
        });
        assert!(report.healthy(), "Unknown must not fail the report");
        report.checks.push(Check {
            name: CHECK_HOST,
            outcome: Outcome::Fail,
            detail: String::new(),
            fix: None,
        });
        assert!(!report.healthy());
    }

    /// An `/v1/auth/me` payload reduced to the two fields the worker-glasses
    /// line reads. Everything else is filler: the check is a pure function of
    /// `claude_credentials_present` and `claude_credentials_status`, which is
    /// exactly why it is falsifiable here rather than behind a mock server.
    fn auth_me(present: Option<bool>, status: Option<&str>) -> crate::client::AuthMeResponse {
        crate::client::AuthMeResponse {
            sub: "tenant-demo-operator".to_owned(),
            aud: vec!["cosmon-rpp-tenant".to_owned()],
            scopes: vec!["cosmon:molecule:read".to_owned()],
            noyau: Some("tenant-demo-sandbox".to_owned()),
            expires_at: "2026-06-10T12:00:00Z".to_owned(),
            issuer: "https://mock-issuer".to_owned(),
            claude_credentials_present: present,
            claude_credentials_status: status.map(str::to_owned),
            extra: Default::default(),
        }
    }

    /// Issue #48, client half: when the server says *why* the worker cannot
    /// start, doctor must repeat the cause instead of one undifferentiated
    /// « not connected ». Each status shapes the detail line it is paired with.
    #[test]
    fn worker_glasses_status_shapes_the_detail() {
        for (status, detail_needle) in [
            ("absent", "no credentials file"),
            ("expired", "expired"),
            ("malformed", "holds no usable token"),
        ] {
            let check = worker_glasses_check(&auth_me(Some(false), Some(status)));
            assert_eq!(check.outcome, Outcome::Fail, "status {status}");
            assert!(
                check.detail.contains(detail_needle),
                "status {status}: detail {:?} must name the cause",
                check.detail
            );
            assert!(
                check.fix.unwrap_or_default().contains("auth login"),
                "status {status}: a re-login is the fix for this cause"
            );
        }
    }

    /// `unreadable` is the exception: an operator-side filesystem problem that
    /// `auth login` cannot repair, so proposing it would be the same kind of
    /// confident-wrong advice the issue was about.
    #[test]
    fn unreadable_glasses_do_not_propose_a_re_login() {
        let check = worker_glasses_check(&auth_me(Some(false), Some("unreadable")));
        assert_eq!(check.outcome, Outcome::Fail);
        let fix = check.fix.unwrap_or_default();
        assert!(
            !fix.contains("auth login --email"),
            "an unreadable file is not fixed by logging in again, got {fix:?}"
        );
        assert!(fix.contains("operator-side"));
    }

    /// A server that answers `true` with no status (an adapter older than issue
    /// #48) must still read as green — the client degrades to the bare boolean
    /// rather than demanding the new field.
    #[test]
    fn worker_glasses_without_status_still_passes() {
        let check = worker_glasses_check(&auth_me(Some(true), None));
        assert_eq!(check.outcome, Outcome::Pass);
        assert!(check.fix.is_none());
    }

    /// An unrecognised status is "some other cause", never an error: the
    /// boolean still decides, and the generic detail stands in.
    #[test]
    fn unrecognised_status_falls_back_to_the_boolean() {
        let check = worker_glasses_check(&auth_me(Some(false), Some("some-future-cause")));
        assert_eq!(check.outcome, Outcome::Fail);
        assert!(check.detail.contains("NOT connected"));
        assert!(check.fix.unwrap_or_default().contains("auth login"));
    }
}
