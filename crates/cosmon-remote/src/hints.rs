// SPDX-License-Identifier: AGPL-3.0-only

//! Actionable rendering of wire errors — « erreurs actionnables ».
//!
//! The server's error bodies are deliberately terse: a stable label +
//! `request_id`, no per-instance detail (anti-oracle). The
//! place where the *rule* and the *repair command* belong is the CLI a
//! human is actually holding. This module maps `(status, label)` to a
//! static hint line printed under the raw error:
//!
//! - the hint names the **probable cause** and **the** repair command —
//!   one gesture, not a menu;
//! - it states the **general rule** (e.g. which artifact names are
//!   reserved) without echoing what the server refused — the hint is
//!   the same whatever the input, so it adds no oracle surface;
//! - unknown labels get no hint — silence is better than a wrong map.

/// Static hint for a wire error, looked up by `(status, label)`.
/// `None` when this binary has nothing trustworthy to say.
#[must_use]
pub fn for_api_error(status: u16, label: &str) -> Option<&'static str> {
    match (status, label) {
        // The janis marche n°1: the worker badge. The probable cause is
        // named, the repair command is singular, doctor closes the loop.
        (503, "tackle_unavailable") => Some(
            "probable cause: the container's Claude worker is not connected (the second \
             badge, distinct from your tenant token).\n  fix:    cosmon-remote auth login \
             --email you@example.com\n  verify: cosmon-remote doctor",
        ),
        (503, "tenant_unavailable") => Some(
            "your space is not yet provisioned on this instance — that is an operator \
             gesture. Check the binding with `cosmon-remote auth me` (noyau field).",
        ),
        // The Jordan wall: artifact push 4xx. State the rule, never
        // which byte offended.
        (400, "invalid_path_segment") => Some(
            "rule: an artifact name is a flat filename — no `/`, `\\` or `..`. Paths \
             `responses/…` are reserved for cosmon. Choose a flat name \
             (e.g. `my-scene.json`) and re-push.",
        ),
        (409, "reserved_name") => Some(
            "rule: names reserved for cosmon (prefix `responses/`, protocol files) are \
             not writable client-side. Choose another name.",
        ),
        (412, "if_match_failed") => Some(
            "the artifact changed since you read it (different ETag), or does not exist \
             yet. `artifact list` to pick up the current ETag — or push without \
             `--if-match` for a first write.",
        ),
        (400, "digest_mismatch") => Some(
            "the received content does not match the announced Digest — did the file \
             change during upload? Re-run the push.",
        ),
        // The Bob wall: PKCE paste-back. The `#` matters.
        (502, "token_exchange_failed") => Some(
            "probable cause: incomplete authorization code — you must paste the ENTIRE \
             `code#state` string, including the part after the `#`. Re-run \
             `cosmon-remote auth login` and paste the full code again.",
        ),
        (409, "already_active") => Some(
            "a worker is already attached to this molecule — observe it with \
             `cosmon-remote molecule get <id>` or wait for it to finish.",
        ),
        (429, _) => {
            Some("quota reached — `cosmon-remote quota` shows the bucket and the reset time.")
        }
        (401, _) => Some(
            "the server refuses the token. `cosmon-remote config show` (sub, aud, oidc-url) \
             then `cosmon-remote doctor` to locate the broken step.",
        ),
        _ => None,
    }
}

/// Extract the stable label from a wire error body (`{"error": "..."}`)
/// so [`for_api_error`] can be keyed off [`crate::error::Error::Api`].
#[must_use]
pub fn label_of(body: &serde_json::Value) -> Option<&str> {
    body.get("error").and_then(|v| v.as_str())
}

/// Actionable line for a `result` call that returned **no deliverable**.
/// Keyed by the **molecule's
/// derived status** (`result_status`), NOT by a
/// server error label: by the time we get here the server answered 200,
/// the client already holds the verdict, and the only thing missing is
/// *the next gesture*. A failure message does not narrate the past — it
/// poses the next move.
///
/// `name` is the invoked binary name ([`crate::invoked_name`]); never
/// hardcode `cosmon-remote` in an actionable string (C6). `age_secs` is
/// the client-computed elapsed time (since tackle / last sign of life),
/// rendered only when known.
///
/// Returns `None` for `ready` (the body is printed instead) and for any
/// status this binary does not recognise — silence beats a wrong map,
/// the same discipline as [`for_api_error`].
#[must_use]
pub fn for_result_status(
    result_status: &str,
    name: &str,
    id: &str,
    age_secs: Option<u64>,
) -> Option<String> {
    let since = age_secs.map_or_else(String::new, |s| format!(" {s}s ago"));
    let stalled_for = age_secs.map_or_else(String::new, |s| format!(" for {s}s"));
    match result_status {
        // Queued, the worker has not been dispatched yet — but it will;
        // nothing is wrong. Wait and retry the same gesture.
        "pending" => Some(format!(
            "not ready yet — the molecule is queued and no worker has started. \
             Retry: `{name} molecule result {id}`"
        )),
        // The worker is alive and producing. Poll the same gesture.
        "running" => Some(format!(
            "not ready yet — the worker is running (started{since}). \
             Retry: `{name} molecule result {id}`"
        )),
        // Alive in name only: tackled but no sign of life. Relaunch.
        "stalled" => Some(format!(
            "the worker has stalled{stalled_for} (no sign of life). \
             Relaunch: `{name} molecule tackle {id}`"
        )),
        // The run stopped without producing a deliverable. Relaunch, or
        // read the lifecycle for the cause.
        "failed" => Some(format!(
            "the worker stopped without a deliverable. \
             Relaunch: `{name} molecule tackle {id}` — or `{name} molecule get {id}` \
             for the cause"
        )),
        // Completed, but this formula yields no single canonical
        // deliverable — point at the artifact listing.
        "done-no-deliverable" => Some(format!(
            "finished, but this formula produces no single deliverable. \
             List what it did write: `{name} artifact list {id}`"
        )),
        // `ready` prints the body; unknown statuses stay silent.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 503 hint must name the probable cause AND the single repair
    /// command, and point back at doctor.
    #[test]
    fn tackle_503_names_cause_and_repair() {
        let hint = for_api_error(503, "tackle_unavailable").unwrap();
        assert!(hint.contains("auth login"));
        assert!(hint.contains("doctor"));
        assert!(hint.contains("probable cause"));
    }

    /// The artifact 4xx hints state the rule (reserved `responses/…`
    /// prefix) without referencing the rejected input — same hint for
    /// every input, zero oracle surface.
    #[test]
    fn artifact_4xx_states_the_rule_not_the_input() {
        for (status, label) in [(400, "invalid_path_segment"), (409, "reserved_name")] {
            let hint = for_api_error(status, label).unwrap();
            assert!(hint.contains("responses/"), "{label} must state the rule");
            assert!(hint.contains("rule"));
        }
    }

    #[test]
    fn pkce_502_explains_the_full_paste() {
        let hint = for_api_error(502, "token_exchange_failed").unwrap();
        assert!(hint.contains('#'));
        assert!(hint.contains("auth login"));
    }

    #[test]
    fn unknown_labels_stay_silent() {
        assert!(for_api_error(500, "weird_new_label").is_none());
        assert!(for_api_error(404, "not_found").is_none());
    }

    #[test]
    fn label_extraction_reads_the_error_field() {
        let body = serde_json::json!({"error": "tackle_unavailable", "request_id": "r-1"});
        assert_eq!(label_of(&body), Some("tackle_unavailable"));
        assert_eq!(label_of(&serde_json::json!({})), None);
    }

    // ---- C4: `result` actionable status hints ----------------------

    /// Each non-ready `result_status` names the EXACT next command, under
    /// the invoked name (not a frozen `cosmon-remote`), with the id.
    /// This is the gate: a failure message poses the next gesture.
    #[test]
    fn result_status_hints_carry_the_exact_next_command() {
        let id = "task-20260614-2f9a";
        let name = "cosmon"; // an alias — the message must follow it

        // pending/running → wait, poll the same gesture.
        for status in ["pending", "running"] {
            let h = for_result_status(status, name, id, Some(42)).unwrap();
            assert!(
                h.contains(&format!("{name} molecule result {id}")),
                "{status} must point back at `result`, got: {h}"
            );
        }

        // stalled → relaunch via tackle.
        let h = for_result_status("stalled", name, id, Some(900)).unwrap();
        assert!(
            h.contains(&format!("{name} molecule tackle {id}")),
            "stalled must point at `tackle`, got: {h}"
        );

        // failed → relaunch via tackle, with `get` for the cause.
        let h = for_result_status("failed", name, id, None).unwrap();
        assert!(h.contains(&format!("{name} molecule tackle {id}")));
        assert!(h.contains(&format!("{name} molecule get {id}")));

        // done-no-deliverable → list the artifacts (the REAL command is
        // `artifact list`, not `molecule artifacts`).
        let h = for_result_status("done-no-deliverable", name, id, None).unwrap();
        assert!(
            h.contains(&format!("{name} artifact list {id}")),
            "done-no-deliverable must point at `artifact list`, got: {h}"
        );
    }

    /// `ready` prints the body, so it carries no hint; unknown statuses
    /// stay silent rather than guessing.
    #[test]
    fn ready_and_unknown_statuses_stay_silent() {
        assert!(for_result_status("ready", "cosmon", "task-1", Some(1)).is_none());
        assert!(for_result_status("weird-future-state", "cosmon", "task-1", None).is_none());
    }

    /// The elapsed age renders only when known — never a dangling "ago"
    /// with no number.
    #[test]
    fn age_is_rendered_only_when_known() {
        let with = for_result_status("running", "cosmon", "task-1", Some(7)).unwrap();
        assert!(with.contains("7s ago"), "got: {with}");
        let without = for_result_status("running", "cosmon", "task-1", None).unwrap();
        assert!(
            !without.contains("ago"),
            "no number → no dangling 'ago': {without}"
        );
    }
}
