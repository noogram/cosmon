// SPDX-License-Identifier: AGPL-3.0-only

//! Falsifier 6 of the session-thread brief: **nothing writes into the
//! session**.
//!
//! Writing into a worker session — answering its prompt, sending it a
//! message — was explicitly deferred, and "deferred" is a claim about the
//! code, not about one request. So it is checked over the code.
//!
//! # Why grep, and not a type-level guard
//!
//! A type-level guard is the stronger instrument and it was considered: give
//! the route a capability handle that only carries `capture-pane`, and a write
//! stops compiling. It buys nothing here that this test does not, and costs a
//! new abstraction on a surface whose whole point is that it has none. The
//! route spawns `tmux` directly, as its `/logs` sibling does; the thing to
//! forbid is a *string* reaching that argument list, and a string is exactly
//! what a grep decides.
//!
//! What this pins, therefore, is textual and deliberately blunt: neither the
//! adapter route nor the client verb may mention any input-side verb of the
//! transport. It fails loudly the day someone adds one, which is when the
//! decision to defer writing is actually being reversed — and reversing it
//! should require deleting this test, in a diff a reviewer reads.

use std::path::{Path, PathBuf};

/// Workspace root — `crates/cosmon-rpp-adapter/` up two levels.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root above crates/cosmon-rpp-adapter")
        .to_path_buf()
}

/// Every way this codebase has of putting text INTO a session. `send-keys` is
/// tmux's own; the rest are the transport port's input surface and the two
/// cosmon verbs that perturb a running worker.
const WRITE_TOKENS: &[&str] = &[
    "send-keys",
    "send_keys",
    "send_input",
    "load_buffer",
    "paste_buffer",
    "TmuxBuffer",
    "whisper",
    "propel",
];

/// The two surfaces the brief names: the route handler and the client verb.
fn read_only_surfaces() -> Vec<(&'static str, String)> {
    let root = workspace_root();
    ["crates/cosmon-rpp-adapter/src/routes/session.rs"]
        .into_iter()
        .map(|rel| {
            let text = std::fs::read_to_string(root.join(rel))
                .unwrap_or_else(|e| panic!("read {rel}: {e}"));
            (rel, text)
        })
        .collect()
}

#[test]
fn the_session_route_contains_no_write_path() {
    let mut hits = Vec::new();
    for (rel, text) in read_only_surfaces() {
        for (n, line) in text.lines().enumerate() {
            // Use, not mention. A doc comment that *names* `send-keys` in
            // order to say the route never spawns one is the documentation of
            // this very property; flagging it would make the invariant
            // unspeakable in the file that holds it.
            if line.trim_start().starts_with("//") {
                continue;
            }
            for token in WRITE_TOKENS {
                if line.contains(token) {
                    hits.push(format!("{rel}:{}: {token} — {}", n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "a write path reached the read-only session surface:\n  {}\n\
         Writing into a session is deferred by decision (issue #51 follow-up). \
         If that decision is being reversed, say so in the diff and delete this \
         test — do not widen it.",
        hits.join("\n  ")
    );
}

/// The client verb half of the same claim: `cosmon-remote`'s session command
/// dials one route and only reads it.
#[test]
fn the_client_session_verb_only_reads() {
    let root = workspace_root();
    let client = std::fs::read_to_string(root.join("crates/cosmon-remote/src/client.rs"))
        .expect("read cosmon-remote client");
    // The client's only session method is the GET. A `post`/`put` against the
    // session route would need this const, so the const's neighbourhood is what
    // is checked.
    let mentions: Vec<&str> = client
        .lines()
        .filter(|l| l.contains("GET_V1_MOLECULES_ID_SESSION"))
        .collect();
    assert_eq!(
        mentions.len(),
        1,
        "the client must dial the session route exactly once (a GET); found {mentions:?}",
    );
    assert!(
        mentions[0].contains("path_with"),
        "the single session call must be the path build of a GET, got {:?}",
        mentions[0]
    );
    assert!(
        !client.contains("POST_V1_MOLECULES_ID_SESSION")
            && !client.contains("PUT_V1_MOLECULES_ID_SESSION"),
        "a write route against the session surface appeared in the client",
    );
}

/// The only tmux subcommand the session route may spawn is `capture-pane`.
///
/// Complements the token denylist from the other direction: a denylist can
/// only forbid the write verbs someone thought of, while this allows exactly
/// one verb and refuses every other — including one tmux grows next year.
#[test]
fn the_only_tmux_verb_on_the_session_path_is_capture_pane() {
    let root = workspace_root();
    let text =
        std::fs::read_to_string(root.join("crates/cosmon-rpp-adapter/src/routes/session.rs"))
            .expect("read the session route");
    // tmux subcommands appear as quoted literals in the `args([...])` list.
    let tmux_verbs: Vec<&str> = [
        "capture-pane",
        "send-keys",
        "paste-buffer",
        "load-buffer",
        "set-buffer",
        "respawn-pane",
        "kill-session",
        "new-session",
        "run-shell",
        "display-message",
    ]
    .into_iter()
    .filter(|verb| text.contains(&format!("\"{verb}\"")))
    .collect();
    assert_eq!(
        tmux_verbs,
        vec!["capture-pane"],
        "the session route may spawn `tmux capture-pane` and nothing else",
    );
}
