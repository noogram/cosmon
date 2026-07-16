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
//!   which is a plain existence check on the credentials file the PKCE
//!   confirm handler writes).
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
    match me.claude_credentials_present {
        Some(true) => checks.push(Check {
            name: CHECK_WORKER_GLASSES,
            outcome: Outcome::Pass,
            detail: "the Claude worker is connected (credentials present)".to_owned(),
            fix: None,
        }),
        Some(false) => checks.push(Check {
            name: CHECK_WORKER_GLASSES,
            outcome: Outcome::Fail,
            detail: "the Claude worker is NOT connected — every `tackle` will fail with 503"
                .to_owned(),
            fix: Some(
                "cosmon-remote auth login --email you@example.com  (once; \
                 this is the second badge — distinct from your tenant token)"
                    .to_owned(),
            ),
        }),
        None => checks.push(Check {
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
        }),
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
}
