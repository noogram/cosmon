// SPDX-License-Identifier: AGPL-3.0-only

//! Dispatch-site stability for the Worker-Spawn Port (ADR-099 / TS-0).
//!
//! # The bug TS-0 makes uncompilable
//!
//! A smoke test showed that `cs tackle --adapter aider`
//! could emit `adapter_selected: aider` to `events.jsonl` and then route the
//! actual `spawn_*` call through Claude. Pre-TS-0, the spawn seam took an
//! unconstrained `adapter_name: &str` — any caller could pass a literal, an
//! env var, or a stale variable; the validation that happened ~120 lines
//! upstream in `cmd::tackle` had no compile-time link to the value the
//! spawn site received.
//!
//! An earlier fix closed the empirical gap with a runtime `match
//! adapter_name { ... other => Err("build-time bug") }`. TS-0 closes the
//! structural gap: the spawn seam refuses to compile when fed a `&str`,
//! so no future addition to the tackle chain can bypass adapter validation
//! without breaking the type system.
//!
//! # The contract
//!
//! - [`ValidatedAdapterName`] has no public constructor. The only way to
//!   obtain one is [`validate_adapter_name`], which checks the raw string
//!   against a caller-supplied registry and returns the per-Adapter triple
//!   `(ValidatedAdapterName, SupervisionMode, LoopOwnership)`.
//! - The Worker-Spawn Port dispatch site (today
//!   `cosmon_cli::cmd::tackle::spawn_and_prompt`) accepts
//!   `&ValidatedAdapterName` rather than `&str`. The runtime `match` arms
//!   inside that dispatcher remain — they guard a different invariant
//!   (registry completeness vs. validation), and the catch-all becomes
//!   genuinely unreachable from in-tree call sites.
//! - `WorkerSpawned.adapter_name` (see [`crate::event_v2`]) is emitted via
//!   [`ValidatedAdapterName::as_str`], so the cat-test
//!   `adapter_selected.adapter_name == worker_spawned.adapter_name` reads
//!   the exact byte sequence that traversed the validation gate.
//!
//! # Per-Adapter typed identity — the four axes
//!
//! [`SupervisionMode`] names *how cosmon learns the worker died* (tmux
//! pane-died hook vs. in-process loop return). [`LoopOwnership`] names
//! *who runs the agent loop* (an external binary cosmon spawns vs. an
//! in-process loop inside `cosmon-agent-harness`). [`RuntimeOwnership`]
//! names *who runs the model server an Adapter forwards completions
//! to — the* who can pull the plug *axis* (an operator-supervised
//! sidecar / `pub(crate)` library vs. a third-party vendor endpoint
//! cosmon merely consumes). All three are orthogonal questions
//! answered jointly per Adapter at validation time and threaded
//! through the spawn pipeline so none has to be re-derived from a
//! string allowlist downstream.
//!
//! ADR-103 records the binding lineage for the binary [`LoopOwnership`]
//! axis; ADR-104 records the two-axis refinement (the *two-axis split* +
//! the *who can pull the plug* question) that produced the
//! [`RuntimeOwnership`] axis.
//!
//! # The 2×2 grid (`LoopOwnership × RuntimeOwnership`)
//!
//! ```text
//!                       │ RuntimeOwnership::Operated │ RuntimeOwnership::Vendor
//! ──────────────────────┼────────────────────────────┼─────────────────────────
//! LoopOwnership::Cosmon │ Path B vllm-mlx sidecar    │ Anthropic API
//!                       │ Path A v0 cosmon-llama     │ OpenAI API
//! ──────────────────────┼────────────────────────────┼─────────────────────────
//! LoopOwnership::Extern │ reserved (self-hosted CLI) │ claude / aider / codex
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};

/// An adapter name proven to belong to a declared dispatch registry.
///
/// Constructed exclusively through [`validate_adapter_name`]; the inner
/// `String` is private so no other module can forge a value, even within
/// the crate. This is the load-bearing primitive of TS-0: the Worker-Spawn
/// Port dispatch site takes `&ValidatedAdapterName`, which makes
/// "spawn called without validation" a compile error rather than a
/// silent runtime regression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ValidatedAdapterName(String);

impl ValidatedAdapterName {
    /// Borrow the validated name as a `&str`.
    ///
    /// Used for in-dispatch match arms and for the
    /// `WorkerSpawned.adapter_name` event field. The returned slice IS
    /// the value that passed [`validate_adapter_name`] — there is no
    /// other source.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ValidatedAdapterName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How an Adapter's worker is supervised post-spawn.
///
/// Names *how cosmon learns the worker died*. Orthogonal to
/// [`LoopOwnership`] (which names *who runs the agent loop*). Both
/// axes are answered jointly at validation time by
/// [`validate_adapter_name`] and travel together through the spawn
/// pipeline.
///
/// `#[non_exhaustive]` so future supervision channels (e.g. a sidecar
/// runner with its own liveness signal) can land without breaking
/// exhaustive matches downstream. Wire format is the lowercased
/// variant name (`tmux_pane`, `in_process`); old `events.jsonl` lines
/// that pre-date the field round-trip through serde defaults at the
/// event-variant level.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisionMode {
    /// Worker runs in a tmux pane; cosmon's pane-died hook is the
    /// supervisor. `claude` / `aider` / `codex` today.
    TmuxPane,
    /// Worker runs in-process inside `cs tackle`; the agent-loop return
    /// is the supervisor. `openai` / `anthropic` today. No tmux pane to
    /// probe — absence of a pane is the nominal state, not death.
    InProcess,
}

/// Who runs the agent loop for a given Adapter.
///
/// Orthogonal to [`SupervisionMode`] (which names *how cosmon learns
/// the worker died*). One bit per Adapter at validation time; the
/// post-spawn pipeline and event log carry it without re-deriving from
/// string allowlists (the seam ADR-099/101 closed for two other axes).
///
/// Reserved space for a `Composite` variant is intentionally absent —
/// `Composite` is a fleet topology (set-level), not an Adapter
/// property (atom-level). (The speculative `fleet_topology` module the
/// original doc-string referenced was deleted as zero-caller scaffolding;
/// see an internal chronicle.)
///
/// `#[non_exhaustive]` so a future cosmon-lab variant can land
/// without breaking exhaustive matches downstream; the `_ =>` arm
/// every consumer must write is the stable widening hook.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopOwnership {
    /// Loop runs in an external binary cosmon spawns
    /// (`claude`, `aider`, `codex`). Cosmon owns spawn, supervision,
    /// pane-signature, liveness — nothing of the loop itself.
    External,
    /// Loop runs in-process inside cosmon
    /// (`cosmon-agent-harness` backing the `openai` / `anthropic`
    /// Direct-API providers). Cosmon owns the FSM, tool dispatch,
    /// message log, briefing seal.
    Cosmon,
}

/// Who runs the model server an Adapter forwards completions to.
///
/// Orthogonal to [`LoopOwnership`] (who runs the agent loop) and
/// [`SupervisionMode`] (how cosmon learns the worker died). The
/// canonical reading is feynman's *« qui peut tirer la prise »* — the
/// operator can pull a self-hosted sidecar (`localhost:8000`) by
/// killing the process; the operator cannot pull Anthropic.
///
/// This axis is the ADR-104 successor refinement to ADR-103's
/// `LoopOwnership`. ADR-103 alone conflated *who runs the loop* with
/// *who runs the model server* because every shipped Adapter at the
/// time sat on the diagonal — `External` loops talked to vendor
/// servers, `Cosmon` loops talked to vendor servers. Path B
/// (a vllm-mlx HTTP sidecar) is the first case
/// where the diagonal breaks: `LoopOwnership::Cosmon ×
/// RuntimeOwnership::Operated`. Naming the second axis lets the
/// dispatch site decide on a typed value rather than re-parsing
/// `base_url` strings.
///
/// `#[non_exhaustive]` reserves the widening hook for future
/// cosmon-lab variants (a hypothetical `Embedded` that
/// distinguishes a `pub(crate)` Rust library in cosmon's address
/// space from a sidecar process — Path A v0 territory). Until a
/// second cosmon-side example forces the cut, the axis stays binary.
/// Wire format is the lowercased variant name (`operated`,
/// `vendor`).
///
/// # Examples — the 2×2 grid
///
/// ```rust,no_run
/// use cosmon_core::spawn_seam::{LoopOwnership, RuntimeOwnership};
///
/// fn dispatch_summary(loop_o: LoopOwnership, runtime_o: RuntimeOwnership) -> &'static str {
///     match (loop_o, runtime_o) {
///         (LoopOwnership::Cosmon, RuntimeOwnership::Operated) =>
///             "cosmon runs the loop and the operator can pull the model server's plug \
///              (vllm-mlx sidecar; cosmon-llama in-process)",
///         (LoopOwnership::Cosmon, RuntimeOwnership::Vendor) =>
///             "cosmon runs the loop; the model server is a vendor cosmon merely consumes \
///              (Anthropic API, OpenAI API)",
///         (LoopOwnership::External, RuntimeOwnership::Operated) =>
///             "cosmon spawns an external CLI talking to an operator-run runtime \
///              (reserved cell — e.g. self-hosted Codex driven by cosmon)",
///         (LoopOwnership::External, RuntimeOwnership::Vendor) =>
///             "cosmon spawns an external CLI talking to its vendor cloud \
///              (claude / aider / codex today)",
///         _ => "widening hook — both axes are #[non_exhaustive]",
///     }
/// }
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeOwnership {
    /// Cosmon (or the operator) installs, version-pins, restarts, and
    /// reads logs from the model server. Path B (`vllm-mlx` sidecar
    /// on `localhost:8000`) and Path A (a `pub(crate)` Rust library
    /// backing the same `LlmProvider` trait) both resolve here. The
    /// distinguishing *capability* is *the operator can pull the
    /// plug*.
    Operated,
    /// A third-party vendor endpoint cosmon merely consumes
    /// (`api.openai.com`, `api.anthropic.com`, `api.x.ai`, etc.).
    /// Cosmon can configure the request but cannot restart the
    /// server, version-pin its release, or read its logs. The
    /// distinguishing *inability* is *the operator cannot pull the
    /// plug*.
    Vendor,
}

/// Built-in Adapter axes — one row per Adapter cosmon ships in-tree.
///
/// Each row groups every per-Adapter axis (supervision, loop ownership,
/// runtime ownership) into a single tuple. Two parallel tables
/// (`BUILT_IN_AXES` + `BUILT_IN_RUNTIMES`) were folded into one because
/// they could drift apart; one table makes desynchronization impossible
/// by construction.
///
/// Used by [`validate_adapter_name`] (loop + supervision) and
/// [`runtime_for_built_in`] (runtime). Hand-authored TOML rows that
/// introduce a new Adapter name fall back to
/// `(TmuxPane, External)` (legacy pre-ADR-100 contract) and
/// `Vendor` for runtime — see the adapter-config helpers in
/// [`crate::config`].
///
/// Adding a name to [`built_in_adapter_names`] without a matching row
/// here trips `built_in_axes_cover_every_built_in_name`.
const BUILT_IN_AXES: &[(&str, SupervisionMode, LoopOwnership, RuntimeOwnership)] = &[
    (
        "claude",
        SupervisionMode::TmuxPane,
        LoopOwnership::External,
        RuntimeOwnership::Vendor,
    ),
    (
        "aider",
        SupervisionMode::TmuxPane,
        LoopOwnership::External,
        RuntimeOwnership::Vendor,
    ),
    (
        "codex",
        SupervisionMode::TmuxPane,
        LoopOwnership::External,
        RuntimeOwnership::Vendor,
    ),
    // `task-20260615-556a` (parent `delib-20260615-73f9`, ADR-125): opencode
    // (sst/opencode) is the external-CLI sibling of codex — same Valence
    // `(TmuxPane, External, Vendor)`. It is a binary on PATH whose pane dies
    // when the run completes, supervised through the standard `pane-died`
    // hook; cosmon does not own its model loop. Clones the codex arm.
    (
        "opencode",
        SupervisionMode::TmuxPane,
        LoopOwnership::External,
        RuntimeOwnership::Vendor,
    ),
    (
        "openai",
        SupervisionMode::InProcess,
        LoopOwnership::Cosmon,
        RuntimeOwnership::Vendor,
    ),
    (
        "anthropic",
        SupervisionMode::InProcess,
        LoopOwnership::Cosmon,
        RuntimeOwnership::Vendor,
    ),
    // C3 (`task-20260519-a226`, parent `delib-20260519-a20b`): the
    // in-process llama.cpp adapter is `(InProcess, Cosmon, Operated)`
    // — same loop/supervision shape as `openai`/`anthropic`, but the
    // operator runs the FFI library on their own hardware (Path A v0
    // cosmon-llama). The canonical CLI name is `llama-cpp` per
    // tolnay's name-stability table (delib synthesis §B.2 D4):
    // bare `llama` collides with the Meta model family. The bare
    // `llama` row below preserves operator vocabulary while the
    // rename-bait kebab form ages out — the canonical row is
    // `llama-cpp`; `llama` is the legacy alias.
    (
        "llama-cpp",
        SupervisionMode::InProcess,
        LoopOwnership::Cosmon,
        RuntimeOwnership::Operated,
    ),
    (
        "llama",
        SupervisionMode::InProcess,
        LoopOwnership::Cosmon,
        RuntimeOwnership::Operated,
    ),
    // `task-20260530-821f` (parent `delib-20260530-0877`): the
    // walking-skeleton local-default adapter. `local` drives the
    // proven `cosmon-agent-harness` spine through the existing
    // `OpenAIProvider` pointed at Ollama's OpenAI-compat endpoint
    // (`http://localhost:11434/v1/chat/completions`). Same axes as
    // `llama` — `(InProcess, Cosmon, Operated)`: the loop runs inside
    // cosmon's address space (no tmux, no subprocess, no Claude Code),
    // and the operator runs the Ollama model server on their own
    // hardware. This is the IFBDD "first bit that runs" route: a bare
    // `cs tackle` (no `--adapter`) routes here by default, proving
    // `provider(LOCAL) → harness(cosmon's OWN) → molecule executed`
    // with ZERO Claude Code in the default path.
    (
        "local",
        SupervisionMode::InProcess,
        LoopOwnership::Cosmon,
        RuntimeOwnership::Operated,
    ),
    // `task-20260707-7d27` (academy banc Mode C, hole #1 — the naming
    // trap): `ollama` is a canonical **alias** of the `local` floor.
    // Before this row, `[adapters.ollama]` in `.cosmon/config.toml`
    // passed `validate_adapter_name` (the TOML name entered the registry)
    // but died on the `spawn_and_prompt` catch-all — "validated but not
    // wired, build-time bug" — because the only dispatch arm was the
    // `local` floor. That breached the invariant "a listed adapter
    // dispatches or fails cleanly". Wiring `ollama` to the same
    // `(InProcess, Cosmon, Operated)` axes as `local` (they drive the
    // identical `OpenAIProvider`-against-Ollama spawn path) closes the
    // trap: `--adapter ollama` is now a first-class name that routes to
    // `spawn_local_session`, and its telemetry stamps `ollama` so the
    // ADR-099 cat-test (`adapter_selected == worker_spawned`) still holds.
    (
        "ollama",
        SupervisionMode::InProcess,
        LoopOwnership::Cosmon,
        RuntimeOwnership::Operated,
    ),
];

/// The set of built-in Adapter names cosmon ships in-tree.
///
/// Exposed as a `&'static [&'static str]` so callers (notably
/// `cs tackle`) can compose their dispatch registry without
/// duplicating the list.
#[must_use]
pub fn built_in_adapter_names() -> &'static [&'static str] {
    &[
        "claude",
        "aider",
        "codex",
        "opencode",
        "openai",
        "anthropic",
        "llama-cpp",
        "llama",
        "local",
        // Canonical alias of `local` (task-20260707-7d27, hole #1).
        "ollama",
    ]
}

/// Promote a raw adapter name to a [`ValidatedAdapterName`], paired
/// with the per-Adapter [`SupervisionMode`] and [`LoopOwnership`] axes.
///
/// `declared` is the dispatch registry the name must belong to — the
/// caller composes it (typically: built-in Adapter names ∪ TOML
/// `.cosmon/config.toml::[adapters]` extras). An empty `declared`
/// rejects every input, which is the intended degenerate behaviour:
/// no validation, no spawn.
///
/// Returned tuple:
/// - `ValidatedAdapterName` — the proof-of-validation newtype.
/// - `SupervisionMode` — how cosmon learns the worker died.
/// - `LoopOwnership` — who runs the agent loop.
///
/// For built-in names the axes are resolved against the in-code
/// [`BUILT_IN_AXES`] table. For caller-supplied names (TOML adapters
/// not shipped in-tree) the axes default to
/// `(SupervisionMode::TmuxPane, LoopOwnership::External)` — the
/// legacy pre-ADR-100 contract preserved so hand-authored
/// `[adapters.<name>]` rows that pre-date this ADR keep their
/// observable behaviour. TOML may override the loop axis explicitly
/// by declaring `ownership = "cosmon"` on the row (see
/// `cosmon-core::config::AdapterEntry::ownership`).
///
/// # Errors
///
/// Returns [`UnknownAdapter`] when `raw` is not a member of `declared`.
pub fn validate_adapter_name(
    raw: &str,
    declared: &[String],
) -> Result<(ValidatedAdapterName, SupervisionMode, LoopOwnership), UnknownAdapter> {
    if declared.iter().any(|d| d == raw) {
        let (supervision, ownership) =
            axes_for_built_in(raw).unwrap_or((SupervisionMode::TmuxPane, LoopOwnership::External));
        Ok((ValidatedAdapterName(raw.to_owned()), supervision, ownership))
    } else {
        Err(UnknownAdapter {
            name: raw.to_owned(),
            available: declared.to_vec(),
        })
    }
}

/// Look up the per-Adapter axes for a built-in name.
///
/// Returns `None` for names not shipped in-tree — caller-supplied TOML
/// rows fall back to the legacy `(TmuxPane, External)` contract or
/// override explicitly via `[adapters.<name>] ownership = "cosmon"`.
#[must_use]
pub fn axes_for_built_in(name: &str) -> Option<(SupervisionMode, LoopOwnership)> {
    BUILT_IN_AXES
        .iter()
        .find(|(n, _, _, _)| *n == name)
        .map(|(_, s, o, _)| (*s, *o))
}

/// Look up the per-Adapter built-in [`RuntimeOwnership`] default for a
/// built-in name (ADR-104 fourth axis).
///
/// Returns `None` for names not shipped in-tree. Caller-supplied TOML
/// rows fall back to `RuntimeOwnership::Vendor` — the honest default,
/// because operators running an operator-supervised runtime (sidecar,
/// in-process library) are the ones who opt in by declaring
/// `[adapters.<name>] runtime = "operated"`. See
/// [`crate::config::AdapterEntry::runtime`].
///
/// The built-in vendor-cloud rows all land in `Vendor`: `claude` /
/// `aider` / `codex` talk to their vendor clouds via the external
/// CLI; `openai` / `anthropic` talk to their vendor clouds via the
/// in-process loop. The `llama-cpp` row (with `llama` legacy alias)
/// lands in `Operated` — the FFI library runs inside cosmon's address
/// space on the operator's hardware. Path B (vllm-mlx sidecar) ships
/// as a per-installation TOML override on `openai` / `anthropic`, not
/// as a new row here — the axis is per-instance, not
/// per-adapter-name.
#[must_use]
pub fn runtime_for_built_in(name: &str) -> Option<RuntimeOwnership> {
    BUILT_IN_AXES
        .iter()
        .find(|(n, _, _, _)| *n == name)
        .map(|(_, _, _, r)| *r)
}

/// Validation failure for [`validate_adapter_name`].
///
/// Carries the raw name and the registry snapshot at the validation
/// point, so the operator-facing diagnostic can list what was actually
/// available without a second lookup.
#[derive(Debug, thiserror::Error)]
#[error("adapter '{name}' not declared; available: {available:?}")]
pub struct UnknownAdapter {
    /// The raw name that failed validation.
    pub name: String,
    /// The declared dispatch table at validation time.
    pub available: Vec<String>,
}

/// Compile-fail proofs that [`ValidatedAdapterName`] cannot be forged.
///
/// These doctests are the type-system witness for TS-0: they fail to
/// compile if a future refactor (a) exposes the tuple field, (b) adds an
/// `impl From<String>` / `From<&str>`, or (c) introduces a `pub fn new`.
/// Any of those would re-open the dispatch-site stability hole that
/// ADR-099 closes.
///
/// ```compile_fail
/// use cosmon_core::spawn_seam::ValidatedAdapterName;
/// // Tuple field must stay private — no public construction.
/// let _bad = ValidatedAdapterName("claude".to_owned());
/// ```
///
/// ```compile_fail
/// use cosmon_core::spawn_seam::ValidatedAdapterName;
/// // No `impl From<String>` — string coercion must not compile.
/// let _bad: ValidatedAdapterName = "claude".to_owned().into();
/// ```
#[doc(hidden)]
#[allow(dead_code)]
pub fn _ts0_compile_fail_witness() {}

/// Compile-fail proofs that the per-Adapter [`LoopOwnership`] axis
/// stays load-bearing (ADR-103).
///
/// These doctests are the type-system witness that the loop-ownership
/// axis cannot be forged or short-circuited. They fail to compile if a
/// future refactor exposes a `LoopOwnership::Cosmon` constructor that
/// bypasses [`validate_adapter_name`], or if the validator's return
/// shape silently drops the axis.
///
/// ```compile_fail
/// use cosmon_core::spawn_seam::validate_adapter_name;
/// // The validator returns a TRIPLE — destructuring as a pair must
/// // not compile. If this starts compiling, the axis was dropped.
/// let (_name, _supervision) =
///     validate_adapter_name("claude", &["claude".to_owned()]).unwrap();
/// ```
///
/// ```compile_fail
/// use cosmon_core::spawn_seam::LoopOwnership;
/// // LoopOwnership is `#[non_exhaustive]`, so an exhaustive match
/// // without a `_ =>` arm must not compile from outside the crate.
/// // This pins the widening hook against an accidental loss in a
/// // future refactor.
/// fn classify(o: LoopOwnership) -> &'static str {
///     match o {
///         LoopOwnership::External => "external",
///         LoopOwnership::Cosmon => "cosmon",
///     }
/// }
/// ```
#[doc(hidden)]
#[allow(dead_code)]
pub fn _loop_ownership_compile_fail_witness() {}

/// Compile-fail proofs that the per-Adapter [`RuntimeOwnership`]
/// axis stays load-bearing (ADR-104).
///
/// These doctests are the type-system witness that the
/// runtime-ownership axis cannot be silently collapsed by a future
/// refactor. They fail to compile if `RuntimeOwnership` loses its
/// `#[non_exhaustive]` attribute (which would let downstream crates
/// write exhaustive matches and silently miss a future widening).
///
/// ```compile_fail
/// use cosmon_core::spawn_seam::RuntimeOwnership;
/// // RuntimeOwnership is `#[non_exhaustive]`, so an exhaustive
/// // match without a `_ =>` arm must not compile from outside the
/// // crate. This pins the widening hook against an accidental loss
/// // in a future refactor (e.g. someone adding an `Embedded`
/// // variant and silently breaking every downstream match).
/// fn classify(r: RuntimeOwnership) -> &'static str {
///     match r {
///         RuntimeOwnership::Operated => "operated",
///         RuntimeOwnership::Vendor => "vendor",
///     }
/// }
/// ```
#[doc(hidden)]
#[allow(dead_code)]
pub fn _runtime_ownership_compile_fail_witness() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_declared_name() {
        let registry = vec!["claude".to_owned(), "aider".to_owned()];
        let (v, sup, own) = validate_adapter_name("aider", &registry).expect("aider is declared");
        assert_eq!(v.as_str(), "aider");
        assert_eq!(sup, SupervisionMode::TmuxPane);
        assert_eq!(own, LoopOwnership::External);
    }

    #[test]
    fn validate_rejects_undeclared_name() {
        let registry = vec!["claude".to_owned()];
        let err = validate_adapter_name("ghost", &registry).expect_err("ghost is not declared");
        assert_eq!(err.name, "ghost");
        assert_eq!(err.available, registry);
        let msg = err.to_string();
        assert!(msg.contains("ghost"), "msg: {msg}");
        assert!(msg.contains("claude"), "msg: {msg}");
    }

    #[test]
    fn validate_rejects_every_name_against_empty_registry() {
        let err = validate_adapter_name("claude", &[]).expect_err("empty registry rejects all");
        assert!(err.available.is_empty());
    }

    #[test]
    fn display_renders_raw_name() {
        let registry = vec!["claude".to_owned()];
        let (v, _, _) = validate_adapter_name("claude", &registry).unwrap();
        assert_eq!(v.to_string(), "claude");
        assert_eq!(format!("{v}"), "claude");
    }

    #[test]
    fn validated_is_clone_and_eq() {
        let registry = vec!["aider".to_owned()];
        let (a, _, _) = validate_adapter_name("aider", &registry).unwrap();
        let b = a.clone();
        assert_eq!(a, b);
    }

    /// ADR-103: the built-in axis table must cover every name returned
    /// by [`built_in_adapter_names`]. A new built-in without a matching
    /// row would silently dispatch with the legacy
    /// `(TmuxPane, External)` default and erase the axis on the
    /// event log.
    #[test]
    fn built_in_axes_cover_every_built_in_name() {
        for name in built_in_adapter_names() {
            assert!(
                axes_for_built_in(name).is_some(),
                "built-in adapter '{name}' has no row in BUILT_IN_AXES — \
                 add (supervision, loop ownership, runtime ownership) before shipping"
            );
        }
    }

    /// ADR-103: the canonical mapping. Pin the table so a silent
    /// edit (swapping Cosmon ↔ External for an in-process adapter) is
    /// caught here rather than at runtime, where it would mis-route
    /// supervision.
    #[test]
    fn built_in_axes_pin_canonical_mapping() {
        assert_eq!(
            axes_for_built_in("claude"),
            Some((SupervisionMode::TmuxPane, LoopOwnership::External))
        );
        assert_eq!(
            axes_for_built_in("aider"),
            Some((SupervisionMode::TmuxPane, LoopOwnership::External))
        );
        assert_eq!(
            axes_for_built_in("codex"),
            Some((SupervisionMode::TmuxPane, LoopOwnership::External))
        );
        assert_eq!(
            axes_for_built_in("opencode"),
            Some((SupervisionMode::TmuxPane, LoopOwnership::External))
        );
        assert_eq!(
            axes_for_built_in("openai"),
            Some((SupervisionMode::InProcess, LoopOwnership::Cosmon))
        );
        assert_eq!(
            axes_for_built_in("anthropic"),
            Some((SupervisionMode::InProcess, LoopOwnership::Cosmon))
        );
        // C3 (`delib-20260519-a20b`): `llama-cpp` is the canonical
        // CLI name for the in-process llama.cpp adapter; `llama` is
        // the legacy alias preserved for operator vocabulary. Both
        // resolve to the same axes because the underlying spawn-site
        // dispatch is identical — only the public name differs.
        assert_eq!(
            axes_for_built_in("llama-cpp"),
            Some((SupervisionMode::InProcess, LoopOwnership::Cosmon))
        );
        assert_eq!(
            axes_for_built_in("llama"),
            Some((SupervisionMode::InProcess, LoopOwnership::Cosmon))
        );
        // task-20260707-7d27 (hole #1): `local` and its `ollama` alias
        // share the in-process floor axes. Pinning both here catches a
        // silent drift that would send `--adapter ollama` down the
        // tmux/external legacy fallback.
        assert_eq!(
            axes_for_built_in("local"),
            Some((SupervisionMode::InProcess, LoopOwnership::Cosmon))
        );
        assert_eq!(
            axes_for_built_in("ollama"),
            Some((SupervisionMode::InProcess, LoopOwnership::Cosmon))
        );
    }

    /// ADR-103: TOML-introduced adapters (names not in
    /// [`BUILT_IN_AXES`]) fall back to the legacy contract
    /// `(TmuxPane, External)`. This is the pre-ADR-100 default and
    /// keeps hand-authored `[adapters.<name>]` rows observable as
    /// they were before the seam closed.
    #[test]
    fn unknown_built_in_axes_fall_back_to_legacy_contract() {
        let registry = vec!["custom".to_owned()];
        let (_name, sup, own) = validate_adapter_name("custom", &registry).unwrap();
        assert_eq!(sup, SupervisionMode::TmuxPane);
        assert_eq!(own, LoopOwnership::External);
    }

    /// `LoopOwnership` wire format is the lowercase variant name —
    /// `external` / `cosmon`. Pin the strings; a future serde
    /// rename would silently break `events.jsonl` replay.
    #[test]
    fn loop_ownership_wire_format_is_snake_case() {
        let s = serde_json::to_string(&LoopOwnership::External).unwrap();
        assert_eq!(s, r#""external""#);
        let s = serde_json::to_string(&LoopOwnership::Cosmon).unwrap();
        assert_eq!(s, r#""cosmon""#);
    }

    /// `SupervisionMode` wire format mirrors the
    /// `cosmon_transport::registry::SupervisionMode` shape it migrated
    /// from. Pin the strings; the helper
    /// `cosmon_transport::registry::supervision_mode_for` re-exports
    /// from here and downstream readers compare against these tags.
    #[test]
    fn supervision_mode_wire_format_is_snake_case() {
        let s = serde_json::to_string(&SupervisionMode::TmuxPane).unwrap();
        assert_eq!(s, r#""tmux_pane""#);
        let s = serde_json::to_string(&SupervisionMode::InProcess).unwrap();
        assert_eq!(s, r#""in_process""#);
    }

    /// ADR-104: the canonical built-in `RuntimeOwnership` defaults at
    /// acceptance. Every shipped Adapter defaults to `Vendor` — Path B
    /// (sidecar) and Path A (in-process library) opt into `Operated`
    /// per-installation via TOML, not as a new row here.
    #[test]
    fn built_in_runtimes_pin_canonical_defaults() {
        assert_eq!(
            runtime_for_built_in("claude"),
            Some(RuntimeOwnership::Vendor)
        );
        assert_eq!(
            runtime_for_built_in("aider"),
            Some(RuntimeOwnership::Vendor)
        );
        assert_eq!(
            runtime_for_built_in("codex"),
            Some(RuntimeOwnership::Vendor)
        );
        assert_eq!(
            runtime_for_built_in("openai"),
            Some(RuntimeOwnership::Vendor)
        );
        assert_eq!(
            runtime_for_built_in("anthropic"),
            Some(RuntimeOwnership::Vendor)
        );
        // C3 (`delib-20260519-a20b`): `llama-cpp` is the first
        // built-in row to default to `Operated` — the operator runs
        // the model server (in-process FFI library), not a vendor
        // cloud. `llama` (legacy alias) tracks the same default.
        assert_eq!(
            runtime_for_built_in("llama-cpp"),
            Some(RuntimeOwnership::Operated)
        );
        assert_eq!(
            runtime_for_built_in("llama"),
            Some(RuntimeOwnership::Operated)
        );
    }

    /// ADR-104: non-built-in names return `None` from
    /// [`runtime_for_built_in`]. Caller-supplied TOML rows fall back
    /// to `Vendor` (the honest default) at the call site, parallel to
    /// the way `axes_for_built_in` returns `None` for non-built-in
    /// names and the call site applies the `(TmuxPane, External)`
    /// legacy default.
    #[test]
    fn runtime_for_built_in_returns_none_for_unknown_name() {
        assert!(runtime_for_built_in("ghost").is_none());
        assert!(runtime_for_built_in("custom-adapter").is_none());
    }

    /// ADR-104: `RuntimeOwnership` wire format is the lowercase variant
    /// name — `operated` / `vendor`. Pin the strings; a future serde
    /// rename would silently break `events.jsonl` replay once the
    /// impl PR lands the event-log field.
    #[test]
    fn runtime_ownership_wire_format_is_snake_case() {
        let s = serde_json::to_string(&RuntimeOwnership::Operated).unwrap();
        assert_eq!(s, r#""operated""#);
        let s = serde_json::to_string(&RuntimeOwnership::Vendor).unwrap();
        assert_eq!(s, r#""vendor""#);
    }

    /// ADR-104: the 2×2 product cell pattern-match used in the
    /// `spawn_seam` module's worked example must compile and cover
    /// every observable cell. This is the runtime witness paired with
    /// the `_runtime_ownership_compile_fail_witness` doctest above.
    #[test]
    fn two_axis_grid_cells_pattern_match() {
        // The `_ => "widen"` arm is intentional: both enums are
        // `#[non_exhaustive]`, so a future variant on either axis
        // must land somewhere — this arm is the stable widening
        // hook. The `unreachable_patterns` allow is load-bearing:
        // intra-crate the four cells are exhaustive today (both
        // enums have exactly two variants), and the warning would
        // erase the widening hook witness from the test.
        #[allow(unreachable_patterns)]
        fn cell(l: LoopOwnership, r: RuntimeOwnership) -> &'static str {
            match (l, r) {
                (LoopOwnership::Cosmon, RuntimeOwnership::Operated) => "C×O",
                (LoopOwnership::Cosmon, RuntimeOwnership::Vendor) => "C×V",
                (LoopOwnership::External, RuntimeOwnership::Operated) => "E×O",
                (LoopOwnership::External, RuntimeOwnership::Vendor) => "E×V",
                _ => "widen",
            }
        }
        assert_eq!(
            cell(LoopOwnership::Cosmon, RuntimeOwnership::Operated),
            "C×O"
        );
        assert_eq!(cell(LoopOwnership::Cosmon, RuntimeOwnership::Vendor), "C×V");
        assert_eq!(
            cell(LoopOwnership::External, RuntimeOwnership::Operated),
            "E×O"
        );
        assert_eq!(
            cell(LoopOwnership::External, RuntimeOwnership::Vendor),
            "E×V"
        );
    }
}
