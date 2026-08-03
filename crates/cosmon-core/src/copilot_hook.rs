// SPDX-License-Identifier: AGPL-3.0-only

//! The co-pilotage hook wiring (mission co-pilotage M6) — the *pure* half.
//!
//! M5 gave an operator a cockpit to type into. M6 gives each pilot a hook that
//! runs without being typed: it pings presence so the other side can see this
//! session is alive, drains the mailbox at a turn boundary so a peer's message
//! arrives while the pilot is still listening, and publishes a staged
//! checkpoint at a natural transition.
//!
//! This module is everything about that wiring which is a *document
//! transformation* rather than an effect: given the settings file a provider
//! already has, what should it contain once the hook is installed, and what
//! should be left of it once the hook is removed. The file I/O, the presence
//! ping and the mailbox drain live in `cs sessions hook`; the rules below are
//! here because they are the part with the sharp edges, and a sharp edge that
//! needs a temporary directory to test is one that gets tested less.
//!
//! # Three rules, and each one is a refusal
//!
//! **A hook is identified by what it does, not by a badge.** Ownership is the
//! substring [`HOOK_MARKER`] — literally the command shape this crate emits.
//! There is no cosmetic `"installedBy": "cosmon"` key that a hand-edit could
//! strip while leaving the entry running, and no version token whose bump
//! orphans the entry it was supposed to identify.
//!
//! **Someone else's entry is never touched.** [`install_claude`] appends
//! beside a foreign hook and [`uninstall_claude`] removes only what matches
//! the marker; a Codex `notify` that is not ours is a **conflict**, reported
//! with what is actually there, not replaced. This is the doctrine
//! `scripts/install-hooks.sh` already applies to git hooks: an unattended step
//! that runs several times a day must not silently rewrite a developer's local
//! configuration.
//!
//! **Uninstall leaves no residue.** Removing the last hook of an event removes
//! the event; removing the last event removes the `hooks` key. A settings file
//! that had no hooks before an install has no `hooks` key after the uninstall,
//! so "clean deactivation" is checkable by comparing two documents rather than
//! by reading a diff and judging it harmless.

use serde_json::{Map, Value};

/// The substring that marks a hook command as this crate's.
///
/// It is the command shape itself rather than a badge: any command containing
/// it *is* a `cs sessions hook run` invocation, and any invocation of that verb
/// contains it. The two cannot drift apart, which is the property a separate
/// marker key would not have.
pub const HOOK_MARKER: &str = "sessions hook run --event";

/// The environment variable that switches the hook off without unwiring it.
///
/// Deactivation has two speeds on purpose. `cs sessions hook uninstall` is the
/// clean one and removes the entry. This one is for the middle of a session,
/// when an operator wants the co-pilot quiet *now* and does not want to edit a
/// settings file the running provider has already read: export it, and the
/// next invocation returns before it has read or written anything.
///
/// It lives here rather than in the CLI because a kill switch a test cannot
/// name is a kill switch nobody checks still works.
pub const HOOK_OFF_ENV: &str = "COSMON_COPILOT_HOOK_OFF";

/// Which pilot's configuration is being wired.
///
/// Deliberately not a list of every provider the mission may reach: an
/// adapter is added by teaching [`HookEvent::provider_event`] one more mapping
/// and nothing in `cs sessions` learns a new branch (mission falsifier 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookProvider {
    /// Claude Code — settings JSON with named hook events.
    Claude,
    /// Codex — a single `notify` program in `config.toml`.
    Codex,
}

impl HookProvider {
    /// The token an operator types after `--provider`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// Parse the operator's token.
    ///
    /// # Errors
    ///
    /// Returns the unknown token, so the caller can name it. An unrecognised
    /// provider is never defaulted: installing a hook into the wrong pilot's
    /// configuration is exactly the silent mistake this refuses.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            other => Err(format!(
                "unknown provider '{other}' — expected 'claude' or 'codex'"
            )),
        }
    }
}

/// The three moments the hook fires, named for what they are to the *mission*
/// rather than for what each provider calls them.
///
/// A provider's vocabulary is an implementation detail of its adapter: Claude
/// says `UserPromptSubmit` and Codex says nothing at all, because it has one
/// `notify` program and no event names. Naming the moments here is what lets
/// `cs sessions hook run --event turn-start` mean the same thing on both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookEvent {
    /// The pilot's session has just opened.
    SessionStart,
    /// The pilot is about to take a turn — the boundary where a peer's
    /// message can still reach it before it acts.
    TurnStart,
    /// The pilot has finished a turn — a natural transition to checkpoint at.
    TurnEnd,
}

impl HookEvent {
    /// Every moment, in the order a session meets them.
    pub const ALL: [Self; 3] = [Self::SessionStart, Self::TurnStart, Self::TurnEnd];

    /// The token an operator types after `--event`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session-start",
            Self::TurnStart => "turn-start",
            Self::TurnEnd => "turn-end",
        }
    }

    /// Parse the operator's token.
    ///
    /// # Errors
    ///
    /// Returns the unknown token. A hook invoked with an event this build does
    /// not know does nothing rather than guessing at the nearest one.
    pub fn parse(raw: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|e| e.as_str() == raw)
            .ok_or_else(|| {
                format!(
                    "unknown hook event '{raw}' — expected one of {}",
                    Self::ALL
                        .iter()
                        .map(|e| e.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    /// The provider's own name for this moment, when it has one.
    ///
    /// `None` means the provider does not distinguish this moment, and the
    /// installer simply does not wire it there — Codex has one `notify`
    /// program, so it gets [`HookEvent::TurnEnd`] and nothing else.
    #[must_use]
    pub const fn provider_event(self, provider: HookProvider) -> Option<&'static str> {
        match (provider, self) {
            (HookProvider::Claude, Self::SessionStart) => Some("SessionStart"),
            (HookProvider::Claude, Self::TurnStart) => Some("UserPromptSubmit"),
            (HookProvider::Claude, Self::TurnEnd) => Some("Stop"),
            (HookProvider::Codex, Self::TurnEnd) => Some("notify"),
            (HookProvider::Codex, _) => None,
        }
    }

    /// Whether this provider feeds the hook's stdout back to the model.
    ///
    /// This is the difference between *informing a pilot* and *writing into a
    /// log nobody reads*, and it is the reason the mailbox is drained at
    /// [`HookEvent::TurnStart`] rather than wherever is convenient: Claude
    /// injects `SessionStart` and `UserPromptSubmit` stdout as context, and
    /// discards `Stop`'s. Draining at a moment whose output is discarded would
    /// consume a peer's message and show it to no one — an at-least-once
    /// channel turned into a shredder (MESSAGE-TRACE).
    #[must_use]
    pub const fn stdout_reaches_pilot(self, provider: HookProvider) -> bool {
        match provider {
            HookProvider::Claude => matches!(self, Self::SessionStart | Self::TurnStart),
            HookProvider::Codex => false,
        }
    }
}

/// The command line a provider is wired to run.
///
/// Contains [`HOOK_MARKER`] by construction, which is what makes it
/// recognisable later.
#[must_use]
pub fn hook_command(cs_bin: &str, event: HookEvent) -> String {
    format!("{cs_bin} sessions hook run --event {}", event.as_str())
}

/// The outcome of an install or uninstall: the document that should be written
/// and whether it differs from what was read.
///
/// `changed: false` is what makes both verbs idempotent *observably* — the
/// caller writes nothing and says nothing landed, rather than rewriting an
/// identical file and reporting success that hides a no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEdit {
    /// The full document to write.
    pub document: String,
    /// Whether it differs from the input.
    pub changed: bool,
    /// The events actually wired or unwired, for the caller to report.
    pub events: Vec<HookEvent>,
}

/// Why a settings document could not be edited.
#[derive(Debug, thiserror::Error)]
pub enum HookWiringError {
    /// The document exists and is not the shape its provider defines.
    #[error("{path} is not valid {kind}: {detail}")]
    Unparsable {
        /// The file that was read.
        path: String,
        /// `JSON` or `TOML`.
        kind: &'static str,
        /// The parser's own complaint.
        detail: String,
    },
    /// The document holds something at the key this hook needs, and that
    /// something is not ours.
    #[error(
        "{path} already defines {key} as {found} — refusing to replace it. \
         Inspect it, then remove it by hand if you want cosmon's hook there."
    )]
    Occupied {
        /// The file that was read.
        path: String,
        /// The key that is taken.
        key: &'static str,
        /// What is there now, rendered.
        found: String,
    },
}

// ---------------------------------------------------------------------------
// Claude — settings JSON
// ---------------------------------------------------------------------------

/// Wire the hook into a Claude Code settings document.
///
/// `existing` is the file's current text, or `None` when there is no file. An
/// empty or whitespace-only file is treated as `{}` — a zero-byte
/// `settings.json` is a file somebody created and never filled, not a
/// corruption worth refusing over.
///
/// Foreign hooks on the same event are preserved and this one is appended
/// beside them. A previous cosmon hook on that event is *rewritten in place*,
/// so re-installing after moving the `cs` binary updates the path instead of
/// leaving two entries where one of them names a binary that is gone.
///
/// # Errors
///
/// [`HookWiringError::Unparsable`] when the document is not JSON, or is JSON
/// that is not an object — `hooks` has nowhere to live in an array.
pub fn install_claude(
    path: &str,
    existing: Option<&str>,
    cs_bin: &str,
) -> Result<HookEdit, HookWiringError> {
    let before = read_json(path, existing)?;
    let mut doc = before.clone();

    let mut wired = Vec::new();
    for event in HookEvent::ALL {
        let Some(name) = event.provider_event(HookProvider::Claude) else {
            continue;
        };
        let command = hook_command(cs_bin, event);
        upsert_claude_event(&mut doc, name, &command);
        wired.push(event);
    }

    Ok(render_json(&before, &doc, wired))
}

/// Remove this crate's hooks from a Claude Code settings document.
///
/// Only entries whose command carries [`HOOK_MARKER`] are removed. Containers
/// left empty by the removal are removed too, so a file that had no `hooks`
/// key before [`install_claude`] has none after this.
///
/// # Errors
///
/// [`HookWiringError::Unparsable`] on a document that is not a JSON object.
pub fn uninstall_claude(path: &str, existing: Option<&str>) -> Result<HookEdit, HookWiringError> {
    let before = read_json(path, existing)?;
    let mut doc = before.clone();

    let mut removed = Vec::new();
    for event in HookEvent::ALL {
        let Some(name) = event.provider_event(HookProvider::Claude) else {
            continue;
        };
        if remove_claude_event(&mut doc, name) {
            removed.push(event);
        }
    }
    prune_empty_hooks(&mut doc);

    Ok(render_json(&before, &doc, removed))
}

/// Which of this crate's hooks a Claude settings document currently carries.
///
/// # Errors
///
/// [`HookWiringError::Unparsable`] on a document that is not a JSON object.
pub fn installed_claude(
    path: &str,
    existing: Option<&str>,
) -> Result<Vec<HookEvent>, HookWiringError> {
    let doc = read_json(path, existing)?;
    Ok(HookEvent::ALL
        .into_iter()
        .filter(|event| {
            event
                .provider_event(HookProvider::Claude)
                .and_then(|name| doc.get("hooks")?.get(name))
                .and_then(Value::as_array)
                .is_some_and(|entries| entries.iter().any(entry_is_ours))
        })
        .collect())
}

fn read_json(path: &str, existing: Option<&str>) -> Result<Value, HookWiringError> {
    let raw = existing.unwrap_or("").trim();
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let parsed: Value = serde_json::from_str(raw).map_err(|e| HookWiringError::Unparsable {
        path: path.to_owned(),
        kind: "JSON",
        detail: e.to_string(),
    })?;
    if !parsed.is_object() {
        return Err(HookWiringError::Unparsable {
            path: path.to_owned(),
            kind: "JSON",
            detail: "expected an object at the top level".to_owned(),
        });
    }
    Ok(parsed)
}

fn render_json(before: &Value, after: &Value, events: Vec<HookEvent>) -> HookEdit {
    let document = serde_json::to_string_pretty(after).unwrap_or_else(|_| "{}".to_owned()) + "\n";
    HookEdit {
        changed: before != after,
        document,
        events,
    }
}

/// Is this one hook object — `{"type":"command","command":"…"}` — ours?
fn hook_is_ours(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(HOOK_MARKER))
}

/// Is this matcher entry — `{"hooks":[…]}` — one of ours?
fn entry_is_ours(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| hooks.iter().any(hook_is_ours))
}

fn upsert_claude_event(doc: &mut Value, name: &str, command: &str) {
    let entries = doc
        .as_object_mut()
        .expect("read_json guarantees an object")
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !entries.is_object() {
        *entries = Value::Object(Map::new());
    }
    let list = entries
        .as_object_mut()
        .expect("just forced to an object")
        .entry(name)
        .or_insert_with(|| Value::Array(Vec::new()));
    if !list.is_array() {
        *list = Value::Array(Vec::new());
    }
    let list = list.as_array_mut().expect("just forced to an array");

    // Rewrite ours in place if it is already there; a re-install after the
    // binary moved must update the path, not add a second entry pointing at a
    // `cs` that no longer exists.
    for entry in list.iter_mut() {
        if !entry_is_ours(entry) {
            continue;
        }
        if let Some(hooks) = entry.get_mut("hooks").and_then(Value::as_array_mut) {
            for hook in hooks.iter_mut() {
                if hook_is_ours(hook) {
                    if let Some(obj) = hook.as_object_mut() {
                        obj.insert("command".to_owned(), Value::from(command));
                    }
                }
            }
        }
        return;
    }

    list.push(serde_json::json!({
        "hooks": [{
            "type": "command",
            "command": command,
            "timeout": HOOK_TIMEOUT_SECONDS,
        }]
    }));
}

/// How long a provider waits for the hook before giving up on it.
///
/// Ten seconds is generous for a directory scan and a few line appends, and
/// short enough that a hung filesystem stalls one turn rather than the
/// session. The hook itself never blocks a turn: it exits 0 on every path.
pub const HOOK_TIMEOUT_SECONDS: u64 = 10;

fn remove_claude_event(doc: &mut Value, name: &str) -> bool {
    let Some(hooks) = doc.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let Some(list) = hooks.get_mut(name).and_then(Value::as_array_mut) else {
        return false;
    };
    let mut removed = false;
    for entry in list.iter_mut() {
        if let Some(inner) = entry.get_mut("hooks").and_then(Value::as_array_mut) {
            let before = inner.len();
            inner.retain(|h| !hook_is_ours(h));
            removed |= inner.len() != before;
        }
    }
    // An entry whose only hook was ours is now an empty shell; a matcher that
    // matches nothing is residue, so it goes too.
    list.retain(|entry| {
        entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_none_or(|inner| !inner.is_empty())
    });
    if list.is_empty() {
        hooks.remove(name);
    }
    removed
}

fn prune_empty_hooks(doc: &mut Value) {
    let empty = doc
        .get("hooks")
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty);
    if empty {
        if let Some(obj) = doc.as_object_mut() {
            obj.remove("hooks");
        }
    }
}

// ---------------------------------------------------------------------------
// Codex — config.toml `notify`
// ---------------------------------------------------------------------------

/// The argv Codex is wired to run as its `notify` program.
///
/// Codex appends its own JSON payload as a final argument, which is why
/// `cs sessions hook run` takes an optional trailing positional payload as
/// well as reading stdin.
#[must_use]
pub fn codex_notify_argv(cs_bin: &str) -> Vec<String> {
    vec![
        cs_bin.to_owned(),
        "sessions".to_owned(),
        "hook".to_owned(),
        "run".to_owned(),
        "--event".to_owned(),
        HookEvent::TurnEnd.as_str().to_owned(),
    ]
}

/// Is this `notify` value one this crate wrote?
fn codex_notify_is_ours(item: &toml_edit::Item) -> bool {
    let Some(arr) = item.as_array() else {
        return false;
    };
    let joined = arr
        .iter()
        .filter_map(toml_edit::Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    joined.contains(HOOK_MARKER)
}

/// Wire the hook into a Codex `config.toml`.
///
/// Codex has exactly one program slot, so unlike Claude there is no room to
/// append beside a foreign entry. A `notify` that is not ours is therefore a
/// [`HookWiringError::Occupied`] naming what is there — never a replacement.
/// The edit is made with `toml_edit`, so comments, ordering and formatting the
/// operator wrote survive it.
///
/// # Errors
///
/// [`HookWiringError::Unparsable`] on a document that is not TOML;
/// [`HookWiringError::Occupied`] when `notify` holds someone else's program.
pub fn install_codex(
    path: &str,
    existing: Option<&str>,
    cs_bin: &str,
) -> Result<HookEdit, HookWiringError> {
    let mut doc = read_toml(path, existing)?;
    let before = doc.to_string();

    if let Some(item) = doc.get("notify") {
        if !codex_notify_is_ours(item) {
            return Err(HookWiringError::Occupied {
                path: path.to_owned(),
                key: "notify",
                found: item.to_string().trim().to_owned(),
            });
        }
    }

    let mut arr = toml_edit::Array::new();
    for token in codex_notify_argv(cs_bin) {
        arr.push(token);
    }
    doc["notify"] = toml_edit::value(arr);

    let document = doc.to_string();
    Ok(HookEdit {
        changed: document != before,
        document,
        events: vec![HookEvent::TurnEnd],
    })
}

/// Remove this crate's `notify` program from a Codex `config.toml`.
///
/// A foreign `notify` is left exactly as it is — uninstalling cosmon's hook is
/// not licence to clear a slot cosmon does not own.
///
/// # Errors
///
/// [`HookWiringError::Unparsable`] on a document that is not TOML.
pub fn uninstall_codex(path: &str, existing: Option<&str>) -> Result<HookEdit, HookWiringError> {
    let mut doc = read_toml(path, existing)?;
    let before = doc.to_string();

    let ours = doc.get("notify").is_some_and(codex_notify_is_ours);
    if ours {
        doc.remove("notify");
    }

    let document = doc.to_string();
    Ok(HookEdit {
        changed: document != before,
        document,
        events: if ours {
            vec![HookEvent::TurnEnd]
        } else {
            Vec::new()
        },
    })
}

/// Which of this crate's hooks a Codex config currently carries.
///
/// # Errors
///
/// [`HookWiringError::Unparsable`] on a document that is not TOML.
pub fn installed_codex(
    path: &str,
    existing: Option<&str>,
) -> Result<Vec<HookEvent>, HookWiringError> {
    let doc = read_toml(path, existing)?;
    Ok(if doc.get("notify").is_some_and(codex_notify_is_ours) {
        vec![HookEvent::TurnEnd]
    } else {
        Vec::new()
    })
}

fn read_toml(
    path: &str,
    existing: Option<&str>,
) -> Result<toml_edit::DocumentMut, HookWiringError> {
    existing
        .unwrap_or("")
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| HookWiringError::Unparsable {
            path: path.to_owned(),
            kind: "TOML",
            detail: e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CS: &str = "/usr/local/bin/cs";

    #[test]
    fn install_into_nothing_wires_the_three_claude_moments() {
        let edit = install_claude("settings.json", None, CS).expect("fresh document");
        assert!(edit.changed);
        assert_eq!(edit.events, HookEvent::ALL.to_vec());

        let doc: Value = serde_json::from_str(&edit.document).expect("valid JSON out");
        for name in ["SessionStart", "UserPromptSubmit", "Stop"] {
            let entries = doc["hooks"][name].as_array().expect("event wired");
            assert_eq!(entries.len(), 1, "{name} has exactly one entry");
            assert!(entry_is_ours(&entries[0]));
        }
    }

    #[test]
    fn a_second_install_changes_nothing() {
        let first = install_claude("settings.json", None, CS).expect("first");
        let second = install_claude("settings.json", Some(&first.document), CS).expect("second");
        assert!(!second.changed, "installing twice is installing once");
        assert_eq!(second.document, first.document);
    }

    #[test]
    fn reinstalling_after_the_binary_moved_rewrites_rather_than_duplicates() {
        let first = install_claude("settings.json", None, "/old/cs").expect("first");
        let moved =
            install_claude("settings.json", Some(&first.document), "/new/cs").expect("move");
        assert!(moved.changed);

        let doc: Value = serde_json::from_str(&moved.document).expect("valid JSON");
        let entries = doc["hooks"]["Stop"].as_array().expect("wired");
        assert_eq!(entries.len(), 1, "one entry, not two");
        let command = entries[0]["hooks"][0]["command"].as_str().expect("command");
        assert!(command.starts_with("/new/cs"), "{command}");
    }

    #[test]
    fn a_foreign_hook_on_the_same_event_survives_install_and_uninstall() {
        let theirs = r#"{
          "model": "opus",
          "hooks": {
            "Stop": [{"hooks": [{"type": "command", "command": "notify-send done"}]}]
          }
        }"#;
        let installed = install_claude("settings.json", Some(theirs), CS).expect("install");
        let doc: Value = serde_json::from_str(&installed.document).expect("valid JSON");
        assert_eq!(doc["hooks"]["Stop"].as_array().expect("both").len(), 2);

        let removed = uninstall_claude("settings.json", Some(&installed.document)).expect("remove");
        let after: Value = serde_json::from_str(&removed.document).expect("valid JSON");
        let theirs_parsed: Value = serde_json::from_str(theirs).expect("valid fixture");
        assert_eq!(
            after, theirs_parsed,
            "uninstall restores the document byte-for-byte in value terms"
        );
    }

    #[test]
    fn uninstall_leaves_no_empty_containers() {
        let plain = r#"{"model": "opus"}"#;
        let installed = install_claude("settings.json", Some(plain), CS).expect("install");
        let removed = uninstall_claude("settings.json", Some(&installed.document)).expect("remove");
        let after: Value = serde_json::from_str(&removed.document).expect("valid JSON");
        assert_eq!(after, serde_json::json!({"model": "opus"}));
        assert!(after.get("hooks").is_none(), "no empty hooks key");
    }

    #[test]
    fn uninstalling_what_was_never_installed_changes_nothing() {
        let plain = r#"{"model":"opus"}"#;
        let removed = uninstall_claude("settings.json", Some(plain)).expect("remove");
        assert!(!removed.changed);
        assert!(removed.events.is_empty());
    }

    #[test]
    fn installed_claude_reports_what_is_actually_wired() {
        assert!(installed_claude("s.json", None).expect("empty").is_empty());
        let edit = install_claude("s.json", None, CS).expect("install");
        assert_eq!(
            installed_claude("s.json", Some(&edit.document)).expect("read"),
            HookEvent::ALL.to_vec()
        );
    }

    #[test]
    fn a_non_object_settings_document_is_refused_not_overwritten() {
        let err = install_claude("s.json", Some("[1, 2, 3]"), CS).expect_err("array refused");
        assert!(matches!(err, HookWiringError::Unparsable { .. }), "{err}");
    }

    #[test]
    fn codex_install_preserves_comments_and_is_idempotent() {
        let theirs = "# my config\nmodel = \"gpt-5\"\n";
        let first = install_codex("config.toml", Some(theirs), CS).expect("install");
        assert!(first.changed);
        assert!(first.document.contains("# my config"), "{}", first.document);
        assert!(first.document.contains("sessions"), "{}", first.document);

        let second = install_codex("config.toml", Some(&first.document), CS).expect("again");
        assert!(!second.changed);
    }

    #[test]
    fn a_foreign_codex_notify_is_refused_and_left_alone() {
        let theirs = "notify = [\"/usr/bin/say\", \"done\"]\n";
        let err = install_codex("config.toml", Some(theirs), CS).expect_err("occupied");
        assert!(matches!(err, HookWiringError::Occupied { .. }), "{err}");

        let removed = uninstall_codex("config.toml", Some(theirs)).expect("uninstall");
        assert!(
            !removed.changed,
            "someone else's notify is not ours to clear"
        );
        assert_eq!(removed.document, theirs);
    }

    #[test]
    fn codex_uninstall_restores_the_original_document() {
        let theirs = "# my config\nmodel = \"gpt-5\"\n";
        let installed = install_codex("config.toml", Some(theirs), CS).expect("install");
        let removed = uninstall_codex("config.toml", Some(&installed.document)).expect("remove");
        assert_eq!(removed.document, theirs);
        assert_eq!(removed.events, vec![HookEvent::TurnEnd]);
    }

    #[test]
    fn the_marker_is_the_command_shape_itself() {
        for event in HookEvent::ALL {
            assert!(hook_command(CS, event).contains(HOOK_MARKER));
        }
        assert!(codex_notify_argv(CS).join(" ").contains(HOOK_MARKER));
    }

    #[test]
    fn the_mailbox_is_drained_where_the_pilot_can_read_it() {
        // The one property that makes M6 an exchange rather than a shredder.
        assert!(HookEvent::TurnStart.stdout_reaches_pilot(HookProvider::Claude));
        assert!(HookEvent::SessionStart.stdout_reaches_pilot(HookProvider::Claude));
        assert!(!HookEvent::TurnEnd.stdout_reaches_pilot(HookProvider::Claude));
    }

    #[test]
    fn codex_wires_only_the_moment_it_has() {
        assert_eq!(
            HookEvent::TurnEnd.provider_event(HookProvider::Codex),
            Some("notify")
        );
        assert!(HookEvent::TurnStart
            .provider_event(HookProvider::Codex)
            .is_none());
    }

    #[test]
    fn unknown_tokens_are_errors_not_defaults() {
        assert!(HookProvider::parse("gemini").is_err());
        assert!(HookEvent::parse("turn-middle").is_err());
        assert_eq!(HookProvider::parse("codex"), Ok(HookProvider::Codex));
        assert_eq!(HookEvent::parse("turn-end"), Ok(HookEvent::TurnEnd));
    }
}
