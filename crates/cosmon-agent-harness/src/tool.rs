// SPDX-License-Identifier: AGPL-3.0-only

//! Tool primitives — the [`Tool`] trait, the [`ToolRegistry`]
//! `BTreeMap` backing, the spine-internal [`ToolCall`] representation,
//! and the [`ToolError`] failure surface.
//!
//! ADR-102 §1 names *Tool* as one of the four words. The registry
//! today exposes seven tools, in two cohorts:
//!
//! - **The v0 trio**: [`ReadFile`],
//!   [`crate::tools::edit_file::EditFile`],
//!   [`crate::tools::exec_command::ExecCommand`]. Read + edit + exec
//!   is the historically-validated agent stance.
//! - **The local-research extension**:
//!   [`crate::tools::list_dir::ListDir`],
//!   [`crate::tools::grep::Grep`],
//!   [`crate::tools::find_file::FindFile`],
//!   [`crate::tools::write_file::WriteFile`]. Together they let the
//!   model navigate, search, and produce files inside `work_dir`
//!   without spending an `exec_command` turn on `ls` / `rg` /
//!   `find` / `cat > file`.
//!
//! ## `BTreeMap` from day one (S5)
//!
//! [`ToolRegistry`] is backed by [`std::collections::BTreeMap`], not
//! [`std::collections::HashMap`]. The reason is stable iteration
//! order — once the harness starts emitting tool schemas to providers
//! that honour prompt-cache prefixes (Anthropic's `cache_control`,
//! OpenAI's implicit prefix freezing), tool-declaration order must
//! be deterministic across runs or the cache invalidates on every
//! call. The choice is free insurance, not a commitment to ship
//! prompt caching in v0 (ADR-102 §9 — prompt cache is out of scope).
//!
//! ## `read_file` still lives in this module
//!
//! `read_file` is the only concrete
//! [`Tool`] still defined inside `tool.rs` — historical, predated the
//! `tools/` submodule. Every other tool lives under
//! [`crate::tools`]; new tools should go there.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Spine-internal representation of one model-emitted tool invocation.
///
/// Per-provider `one_turn` impls translate their native envelope
/// (OpenAI's `tool_calls[].function.{name,arguments}`, Anthropic's
/// `tool_use` content blocks) into this shape before handing them to
/// the spine. **This is not a wire envelope** — ADR-102 §D-3
/// explicitly refuses to extract a normalised wire `ToolCall` /
/// `ToolResult` until a third schema lands. The spine's `ToolCall`
/// is the *internal* dispatch shape, owned by this crate.
///
/// `#[non_exhaustive]` — additive
/// fields (e.g. invocation provenance, tool-call timestamps) must not
/// require a major bump.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Opaque tool-call identifier, used to pair the response with
    /// the originating call inside the provider's [`crate::message_log::MessageLog`].
    pub id: String,
    /// The tool name the model asked the registry to dispatch.
    pub name: String,
    /// JSON-serialised arguments, parsed inside [`Tool::execute`].
    pub arguments_json: String,
}

impl ToolCall {
    /// Construct a [`ToolCall`] from the three identifier fields.
    ///
    /// Required path for downstream crates now that the struct is
    /// `#[non_exhaustive]` — the struct literal `ToolCall { … }` no
    /// longer compiles outside `cosmon-agent-harness`.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments_json: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments_json: arguments_json.into(),
        }
    }
}

/// JSON-Schema fragment carried by [`ToolDeclaration::parameters`].
///
/// Newtype wrapper over [`serde_json::Value`].
/// Hides the `serde_json` dependency from the
/// crate's public surface — a future migration to `schemars` (or any
/// other schema crate) is a non-breaking internal change instead of a
/// federation-wide major bump.
///
/// The two helpers below are intentionally minimal: callers either
/// build a JSON value with `serde_json::json!` and lift it into a
/// [`ParametersSchema`] via [`Self::from_json`], or read it back via
/// [`Self::as_json`] to feed a provider-specific serializer.
#[derive(Debug, Clone)]
pub struct ParametersSchema(serde_json::Value);

impl ParametersSchema {
    /// Lift a `serde_json::Value` into a [`ParametersSchema`].
    #[must_use]
    pub fn from_json(value: serde_json::Value) -> Self {
        Self(value)
    }

    /// Borrow the wrapped JSON-Schema fragment for serialization.
    #[must_use]
    pub fn as_json(&self) -> &serde_json::Value {
        &self.0
    }
}

impl From<serde_json::Value> for ParametersSchema {
    fn from(value: serde_json::Value) -> Self {
        Self(value)
    }
}

/// Provider-facing tool schema declaration, emitted by
/// [`crate::spine::Provider::tool_schema`] so the wire envelope can
/// advertise the registered tools to the model.
///
/// The `parameters` value is a JSON Schema fragment — both OpenAI's
/// `function.parameters` and Anthropic's `input_schema` consume the
/// same JSON-Schema shape, so v0 carries one [`ParametersSchema`]
/// (a newtype over [`serde_json::Value`]) across both providers. A
/// third schema with a divergent declaration syntax would force a
/// per-provider translation method; the per-provider impl is the
/// natural home for that fix.
///
/// `#[non_exhaustive]` — keeps
/// future fields (provenance, schema-version tags, capability flags)
/// non-breaking. Use [`Self::new`] to construct from outside the
/// crate.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ToolDeclaration {
    /// Tool name — must match the registry key.
    pub name: &'static str,
    /// Human-readable description, surfaced to the model verbatim.
    pub description: &'static str,
    /// JSON Schema for the tool's argument object, behind the
    /// [`ParametersSchema`] newtype (tolnay F1 — no `serde_json`
    /// surface leak).
    pub parameters: ParametersSchema,
}

impl ToolDeclaration {
    /// Construct a [`ToolDeclaration`] for a tool.
    ///
    /// Required path for downstream crates now that the struct is
    /// `#[non_exhaustive]` — the struct literal `ToolDeclaration { … }`
    /// no longer compiles outside `cosmon-agent-harness`.
    #[must_use]
    pub fn new(
        name: &'static str,
        description: &'static str,
        parameters: impl Into<ParametersSchema>,
    ) -> Self {
        Self {
            name,
            description,
            parameters: parameters.into(),
        }
    }
}

/// Tool-dispatch failures returned by [`Tool::execute`] and surfaced
/// via [`crate::error::HarnessError::Tool`].
///
/// The variants are deliberately narrow — `NotWhitelisted` and
/// `PathEscape` are loud-by-construction structural refusals, while
/// `Io` and `InvalidArguments` carry the underlying error message
/// as a string to keep the trait object-safe. SF-6 (`StaleBasePatch`,
/// hunk-based `edit_file`) is named in ADR-102 §7 but not implemented
/// in v0; the variant will land with the `edit_file` tool itself.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// The model asked the registry to dispatch a tool name that is
    /// not registered. Structural refusal — never a runtime guess.
    #[error("tool not whitelisted: {0}")]
    NotWhitelisted(String),

    /// The tool's IO step failed (`std::fs::write`, `create_dir_all`,
    /// etc).
    #[error("tool io: {0}")]
    Io(String),

    /// JSON deserialisation of the tool's arguments failed.
    #[error("invalid arguments for {tool}: {message}")]
    InvalidArguments {
        /// Tool name whose arguments could not be parsed.
        tool: String,
        /// Underlying parser error message.
        message: String,
    },

    /// The tool's path argument escaped the worker's `work_dir`
    /// (absolute path or `..` segment). Forgemaster §3.3 loud-failure
    /// requirement — silent path escapes would let a worker scribble
    /// outside its worktree.
    #[error("path escapes work_dir: {0}")]
    PathEscape(String),
}

/// A whitelisted capability the model may invoke during a turn.
///
/// Per-tool implementors live in this crate's `tool` module today.
/// Sibling beads (c4d8 `exec_command`, f9c7 `edit_file`, 6c4a
/// `read_file`) will each add one new struct in this module and one
/// `registry.register(...)` line in [`default_registry`].
pub trait Tool: Send + Sync {
    /// Stable tool name — used as the registry key and matched
    /// against [`ToolCall::name`] at dispatch.
    fn name(&self) -> &'static str;

    /// Declaration emitted to the provider via
    /// [`crate::spine::Provider::tool_schema`].
    fn declaration(&self) -> ToolDeclaration;

    /// Execute the tool against `work_dir`. The `arguments_json`
    /// string is the model's serialised argument object; the impl
    /// is responsible for deserialising and validating it.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError`] when arguments are invalid, the path
    /// escapes `work_dir`, the IO step fails, or the tool refuses
    /// for a tool-specific reason.
    fn execute(&self, arguments_json: &str, work_dir: &Path) -> Result<String, ToolError>;
}

/// `BTreeMap`-backed registry of whitelisted [`Tool`] implementations.
///
/// Iteration order is stable (S5) — the prerequisite for future
/// prompt-cache prefix-stability work without being a commitment to
/// ship prompt caching in v0. See the module docs.
pub struct ToolRegistry {
    tools: BTreeMap<&'static str, Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
        }
    }

    /// Register one [`Tool`] implementation. Overwrites any prior
    /// entry under the same name — registration order is the
    /// caller's responsibility.
    pub fn register(&mut self, tool: Box<dyn Tool>) -> &mut Self {
        let name = tool.name();
        self.tools.insert(name, tool);
        self
    }

    /// Dispatch one [`ToolCall`] against the registered tools.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::NotWhitelisted`] when the call's `name`
    /// has no registered match; propagates the dispatched tool's
    /// own [`ToolError`] on failure.
    pub fn execute(&self, call: &ToolCall, work_dir: &Path) -> Result<String, ToolError> {
        let tool = self
            .tools
            .get(call.name.as_str())
            .ok_or_else(|| ToolError::NotWhitelisted(call.name.clone()))?;
        tool.execute(&call.arguments_json, work_dir)
    }

    /// Snapshot of all registered tool declarations, in stable
    /// iteration order (BTreeMap key order).
    #[must_use]
    pub fn declarations(&self) -> Vec<ToolDeclaration> {
        self.tools.values().map(|t| t.declaration()).collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Build the default registry — seven tools:
///
/// - [`ReadFile`] — one syscall, one
///   message-log entry; spares the model an `exec_command cat` turn.
/// - [`crate::tools::edit_file::EditFile`] —
///   Aider-style exact-match search-and-replace.
/// - [`crate::tools::exec_command::ExecCommand`] —
///   shell execution with the 32 KiB output cap.
/// - [`crate::tools::list_dir::ListDir`] —
///   gitignore-aware directory listing.
/// - [`crate::tools::grep::Grep`] — in-process
///   regex search over text files.
/// - [`crate::tools::find_file::FindFile`] —
///   gitignore-style glob over file names.
/// - [`crate::tools::write_file::WriteFile`] —
///   create-only file writer; refuses to overwrite. This is enforced
///   by construction — the model can only use `write_file` for fresh
///   files and must reach for `edit_file` to modify — which preserves
///   the rule that re-emitting unchanged lines wholesale is
///   hallucination-prone and should be avoided.
#[must_use]
pub fn default_registry() -> ToolRegistry {
    default_registry_with_operator_block(None)
}

/// Build the capability set for an untrusted local worker.
///
/// A process shell cannot be confined by its current directory: absolute
/// paths and host-wide walkers such as `find /` remain available to its uid.
/// Local workers therefore receive only path-checked file tools rooted at
/// `work_dir`; `exec_command` is intentionally absent.
#[must_use]
pub fn local_sandbox_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadFile));
    registry.register(Box::new(crate::tools::edit_file::EditFile));
    registry.register(Box::new(crate::tools::list_dir::ListDir));
    registry.register(Box::new(crate::tools::grep::Grep));
    registry.register(Box::new(crate::tools::find_file::FindFile));
    registry.register(Box::new(crate::tools::write_file::WriteFile));
    registry
}

/// Build the default registry, additionally gating the
/// [`crate::tools::await_operator::AwaitOperator`] tool on the molecule's
/// operator-block capability (ADR-123).
///
/// - `capability` **`None`** ⇒ the seven base tools, **no** blocking
///   primitive. A worker on a non-capability molecule literally cannot
///   block through the harness — its only paths are finish
///   ([`crate::spine::Turn::Stop`]), keep working, or
///   surface-and-continue. This is byte-identical to today's
///   [`default_registry`], so existing callers are unaffected.
/// - `capability` **`Some(..)`** ⇒ the seven base tools **plus**
///   `await_operator`, the single sanctioned blocking primitive. It emits
///   the typed block signal before yielding (CV-2: *emit, do not infer*).
///
/// **No modal / `ask_user_question` tool is ever registered, in either
/// branch.** The off-cosmon modal that caused the incident is unavailable
/// by construction — *make blocking-without-emitting structurally hard,
/// not merely documented* (kahneman).
#[must_use]
pub fn default_registry_with_operator_block(
    capability: Option<&cosmon_core::operator_block::OperatorBlockCapability>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadFile));
    registry.register(Box::new(crate::tools::edit_file::EditFile));
    registry.register(Box::new(crate::tools::exec_command::ExecCommand::default()));
    registry.register(Box::new(crate::tools::list_dir::ListDir));
    registry.register(Box::new(crate::tools::grep::Grep));
    registry.register(Box::new(crate::tools::find_file::FindFile));
    registry.register(Box::new(crate::tools::write_file::WriteFile));
    if capability.is_some() {
        registry.register(Box::new(
            crate::tools::await_operator::AwaitOperator::default(),
        ));
    }
    registry
}

/// Arguments for the `read_file` tool — a single `path` relative to
/// the worker's `work_dir` (validated by [`sanitize_join`]).
///
/// The single-field shape mirrors the wire contract;
/// no offsets, no `max_bytes`, no
/// metadata flags in v0 — YAGNI until the second use case proves
/// otherwise.
#[derive(Debug, Deserialize)]
pub struct ReadParams {
    /// Path relative to `work_dir`. Absolute paths and `..`
    /// segments are refused by [`sanitize_join`].
    pub path: String,
}

/// Result payload returned by the `read_file` tool — the file's
/// UTF-8 contents as a single string, capped at [`READ_FILE_CAP_BYTES`]
/// with a loud truncation marker matching `exec_command`'s
/// `OUTPUT_CAP_BYTES` discipline.
///
/// The earlier
/// "v0 returns the full body without truncation" rationale assumed
/// disk slowdown was the natural backpressure signal. On M4 Max with
/// NVMe a 50 MiB read completes in ~40 ms and saturates the model's
/// context window silently — undermining the context-budget
/// enforcement, hence the cap.
#[derive(Debug, Serialize)]
pub struct ReadResult {
    /// File contents, decoded as UTF-8 and capped to
    /// [`READ_FILE_CAP_BYTES`]. Non-UTF-8 bytes surface as
    /// [`ToolError::Io`].
    pub content: String,
}

/// Maximum bytes returned in [`ReadResult::content`]. Mirrors
/// `cosmon_agent_harness::tools::exec_command::OUTPUT_CAP_BYTES` so the
/// two read-shaped tools share a single truncation discipline.
///
/// An uncapped
/// `read_file` on a 50 MiB seeded file silently saturates the model's
/// context window before the context-budget guard can fire — the cap
/// is necessary for the in-loop monotonicity check on
/// `MessageLog::estimate_tokens` to actually bind.
pub const READ_FILE_CAP_BYTES: usize = 32 * 1024;

/// `read_file` — reads a UTF-8 text file inside the worker's
/// `work_dir` and returns its content (capped at
/// [`READ_FILE_CAP_BYTES`]) as a [`ReadResult`].
///
/// Three lines of Rust on top of [`std::fs::read_to_string`] and
/// [`sanitize_join`] (briefing). The point is to spare the model
/// from spending an `exec_command` turn on `cat` — one tool call,
/// one syscall, one message-log entry.
///
/// ## Why UTF-8 only in v0
///
/// [`std::fs::read_to_string`] errors with `InvalidData` when the
/// file is not valid UTF-8; the error is surfaced verbatim as
/// [`ToolError::Io`]. Binary-file ingestion would require a wire
/// envelope choice (base64? raw bytes?) and a model that knows what
/// to do with the result — both YAGNI per the briefing.
///
/// ## Why a 32 KiB cap
///
/// The earlier "no size cap; OS slowdown is the natural signal"
/// rationale was wrong on M4 Max with NVMe — a 50 MiB read completes
/// in ~40 ms and saturates the model's context window silently. With
/// the cap and a loud `... read_file truncated; original size = N
/// bytes ...` marker matching `exec_command`'s pattern, the model
/// knows to narrow the next read via `tail` / `rg` / a second
/// `read_file` with a smaller target.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReadFile;

impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn declaration(&self) -> ToolDeclaration {
        ToolDeclaration {
            name: "read_file",
            description: "Read a UTF-8 text file inside the worker's work_dir and \
                 return its contents. Prefer this over `exec_command cat` — \
                 it is one syscall and one message-log entry.",
            parameters: ParametersSchema::from_json(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to work_dir."},
                },
                "required": ["path"],
            })),
        }
    }

    fn execute(&self, arguments_json: &str, work_dir: &Path) -> Result<String, ToolError> {
        let params: ReadParams =
            serde_json::from_str(arguments_json).map_err(|e| ToolError::InvalidArguments {
                tool: "read_file".to_owned(),
                message: e.to_string(),
            })?;
        let target = sanitize_join(work_dir, &params.path)?;
        let raw = std::fs::read_to_string(&target).map_err(|e| ToolError::Io(e.to_string()))?;
        let content = cap_read_output(raw);
        let result = ReadResult { content };
        serde_json::to_string(&result).map_err(|e| ToolError::Io(e.to_string()))
    }
}

/// Cap the read body at [`READ_FILE_CAP_BYTES`] with a loud truncation
/// marker matching `exec_command`'s pattern. Char-boundary safe so the
/// returned `String` remains valid UTF-8 even when the cap falls in
/// the middle of a multi-byte codepoint.
fn cap_read_output(mut s: String) -> String {
    use std::fmt::Write as _;
    if s.len() <= READ_FILE_CAP_BYTES {
        return s;
    }
    let original = s.len();
    let mut cut = READ_FILE_CAP_BYTES;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    let _ = write!(
        s,
        "\n... read_file truncated; original size = {original} bytes ...\n"
    );
    s
}

/// Reject absolute paths and `..` segments before joining onto
/// `work_dir`. Forgemaster §3.3 loud-failure requirement — a silent
/// path escape lets a worker scribble outside its worktree.
///
/// # Errors
///
/// Returns [`ToolError::PathEscape`] if `raw` is absolute or contains
/// a parent-directory segment.
pub fn sanitize_join(work_dir: &Path, raw: &str) -> Result<PathBuf, ToolError> {
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        return Err(ToolError::PathEscape(format!(
            "absolute path refused: {raw}"
        )));
    }
    for component in candidate.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(ToolError::PathEscape(format!("parent-dir escape: {raw}")));
        }
    }
    Ok(work_dir.join(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sanitize_join_refuses_absolute_path() {
        let dir = tempdir().unwrap();
        let err = sanitize_join(dir.path(), "/etc/passwd").expect_err("must refuse");
        assert!(err.to_string().contains("absolute path refused"));
    }

    #[test]
    fn sanitize_join_refuses_parent_escape() {
        let dir = tempdir().unwrap();
        let err = sanitize_join(dir.path(), "../escape.txt").expect_err("must refuse");
        assert!(err.to_string().contains("parent-dir escape"));
    }

    #[test]
    fn sanitize_join_accepts_nested_relative() {
        let dir = tempdir().unwrap();
        let joined = sanitize_join(dir.path(), "out/haiku.md").expect("ok");
        assert!(joined.starts_with(dir.path()));
        assert!(joined.ends_with("out/haiku.md"));
    }

    #[test]
    fn default_registry_contains_seven_tools() {
        // The v0 trio + the local-research extension (task-20260521-a095):
        // read_file + edit_file + exec_command + list_dir + grep +
        // find_file + write_file. The 2026-05-22 re-introduction of
        // write_file is **create-only** — see
        // `tools/write_file.rs` for the panel-verdict honouring.
        let registry = default_registry();
        let names: Vec<&str> = registry.declarations().iter().map(|d| d.name).collect();
        // BTreeMap iteration order — alphabetical by key.
        assert_eq!(
            names,
            vec![
                "edit_file",
                "exec_command",
                "find_file",
                "grep",
                "list_dir",
                "read_file",
                "write_file",
            ]
        );
    }

    #[test]
    fn local_sandbox_registry_omits_shell_capability() {
        let names: Vec<_> = local_sandbox_registry()
            .declarations()
            .into_iter()
            .map(|declaration| declaration.name)
            .collect();
        assert!(!names.contains(&"exec_command"));
        assert_eq!(names.len(), 6);
    }

    /// ADR-123 (c) — `await_operator` is registered ONLY when the
    /// molecule carries the operator-block capability. A non-capability
    /// molecule gets exactly the seven base tools and **no** blocking
    /// primitive: it cannot block through the harness at all.
    #[test]
    fn await_operator_is_gated_on_capability() {
        use cosmon_core::operator_block::{IrreversibleBoundary, OperatorBlockCapability};

        // Without capability: identical to the seven-tool default.
        let none = default_registry_with_operator_block(None);
        let names: Vec<&str> = none.declarations().iter().map(|d| d.name).collect();
        assert_eq!(
            names,
            default_registry()
                .declarations()
                .iter()
                .map(|d| d.name)
                .collect::<Vec<_>>()
        );
        assert!(!names.contains(&"await_operator"));

        // With capability: the base tools PLUS the blocking primitive.
        let cap = OperatorBlockCapability::new(IrreversibleBoundary::Signature);
        let gated = default_registry_with_operator_block(Some(&cap));
        let gated_names: Vec<&str> = gated.declarations().iter().map(|d| d.name).collect();
        assert!(gated_names.contains(&"await_operator"), "{gated_names:?}");
        assert_eq!(gated_names.len(), names.len() + 1);
    }

    /// ADR-123 (c) — the forbidden `AskUserQuestion`-style modal is
    /// **never** registered, in either capability branch. The off-cosmon
    /// modal that caused the incident is unavailable by construction; the
    /// only sanctioned block is the signal-emitting `await_operator`.
    #[test]
    fn no_modal_tool_is_ever_registered() {
        use cosmon_core::operator_block::{IrreversibleBoundary, OperatorBlockCapability};

        let cap = OperatorBlockCapability::new(IrreversibleBoundary::Signature);
        for registry in [
            default_registry(),
            default_registry_with_operator_block(None),
            default_registry_with_operator_block(Some(&cap)),
        ] {
            for d in registry.declarations() {
                let n = d.name.to_ascii_lowercase();
                assert!(
                    !n.contains("askuserquestion") && !n.contains("ask_user_question"),
                    "a modal blocking tool must never be registered, found `{}`",
                    d.name
                );
            }
        }
    }

    /// `write_file` is registered, but the no-wholesale-rewrite rule is
    /// preserved by construction: the tool refuses to overwrite
    /// an existing file, so the *re-emit unchanged lines* failure
    /// mode cannot occur via this surface.
    #[test]
    fn registry_dispatch_routes_write_file_to_create_only_tool() {
        let dir = tempdir().unwrap();
        let registry = default_registry();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "write_file".to_owned(),
            arguments_json: serde_json::json!({
                "path": "new.txt",
                "content": "hello"
            })
            .to_string(),
        };
        let raw = registry
            .execute(&call, dir.path())
            .expect("write_file must dispatch");
        assert!(raw.contains("bytes_written"));
        // Second call on the same path must refuse — create-only.
        let err = registry
            .execute(&call, dir.path())
            .expect_err("must refuse second write");
        assert!(matches!(err, ToolError::Io(_)));
    }

    #[test]
    fn registry_dispatch_refuses_unknown_tool() {
        let dir = tempdir().unwrap();
        let registry = default_registry();
        let call = ToolCall {
            id: "call-2".to_owned(),
            name: "nonexistent_tool".to_owned(),
            arguments_json: "{}".to_owned(),
        };
        let err = registry
            .execute(&call, dir.path())
            .expect_err("must refuse");
        assert!(matches!(err, ToolError::NotWhitelisted(_)));
    }

    #[test]
    fn read_file_returns_utf8_contents() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("haiku.md"),
            "old pond\na frog jumps\nsplash\n",
        )
        .expect("seed file");
        let tool = ReadFile;
        let args = serde_json::json!({ "path": "haiku.md" });
        let raw = tool
            .execute(&args.to_string(), dir.path())
            .expect("read must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        assert_eq!(
            parsed.get("content").and_then(|v| v.as_str()),
            Some("old pond\na frog jumps\nsplash\n")
        );
    }

    #[test]
    fn read_file_io_error_when_path_missing() {
        let dir = tempdir().unwrap();
        let tool = ReadFile;
        let args = serde_json::json!({ "path": "nope.txt" });
        let err = tool
            .execute(&args.to_string(), dir.path())
            .expect_err("must fail");
        assert!(matches!(err, ToolError::Io(_)));
    }

    #[test]
    fn read_file_io_error_on_non_utf8_bytes() {
        let dir = tempdir().unwrap();
        // 0xFF, 0xFE are not a valid UTF-8 lead sequence.
        std::fs::write(dir.path().join("blob.bin"), [0xFFu8, 0xFE, 0xFD]).expect("seed");
        let tool = ReadFile;
        let args = serde_json::json!({ "path": "blob.bin" });
        let err = tool
            .execute(&args.to_string(), dir.path())
            .expect_err("must fail");
        assert!(matches!(err, ToolError::Io(_)));
    }

    #[test]
    fn read_file_refuses_parent_escape() {
        let dir = tempdir().unwrap();
        let tool = ReadFile;
        let args = serde_json::json!({ "path": "../escape.txt" });
        let err = tool
            .execute(&args.to_string(), dir.path())
            .expect_err("must refuse");
        assert!(matches!(err, ToolError::PathEscape(_)));
    }

    #[test]
    fn read_file_refuses_absolute_path() {
        let dir = tempdir().unwrap();
        let tool = ReadFile;
        let args = serde_json::json!({ "path": "/etc/passwd" });
        let err = tool
            .execute(&args.to_string(), dir.path())
            .expect_err("must refuse");
        assert!(matches!(err, ToolError::PathEscape(_)));
    }

    /// A 1 MiB body
    /// must come back truncated at [`READ_FILE_CAP_BYTES`] with a
    /// loud marker that names the original size.
    #[test]
    fn read_file_caps_large_payload_with_loud_marker() {
        let dir = tempdir().unwrap();
        let original = "x".repeat(1024 * 1024); // 1 MiB
        std::fs::write(dir.path().join("big.txt"), &original).expect("seed");
        let tool = ReadFile;
        let args = serde_json::json!({ "path": "big.txt" });
        let raw = tool
            .execute(&args.to_string(), dir.path())
            .expect("read must succeed");
        let parsed: ReadResult = serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|v| {
                v.get("content")
                    .and_then(|c| c.as_str())
                    .map(|s| ReadResult {
                        content: s.to_owned(),
                    })
            })
            .expect("ReadResult shape");
        assert!(parsed.content.len() > READ_FILE_CAP_BYTES);
        assert!(parsed.content.len() <= READ_FILE_CAP_BYTES + 256);
        assert!(parsed.content.contains("read_file truncated"));
        assert!(parsed
            .content
            .contains(&format!("original size = {} bytes", original.len())));
    }

    #[test]
    fn read_file_under_cap_passes_through_unchanged() {
        let dir = tempdir().unwrap();
        let body = "small file\nstill small\n";
        std::fs::write(dir.path().join("ok.txt"), body).expect("seed");
        let tool = ReadFile;
        let args = serde_json::json!({ "path": "ok.txt" });
        let raw = tool
            .execute(&args.to_string(), dir.path())
            .expect("read must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        assert_eq!(parsed.get("content").and_then(|v| v.as_str()), Some(body));
    }

    #[test]
    fn registry_dispatch_routes_to_read_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("note.txt"), "hello").expect("seed");
        let registry = default_registry();
        let call = ToolCall {
            id: "call-3".to_owned(),
            name: "read_file".to_owned(),
            arguments_json: serde_json::json!({ "path": "note.txt" }).to_string(),
        };
        let raw = registry.execute(&call, dir.path()).expect("dispatch ok");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        assert_eq!(
            parsed.get("content").and_then(|v| v.as_str()),
            Some("hello")
        );
    }
}
