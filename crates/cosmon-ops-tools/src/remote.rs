// SPDX-License-Identifier: AGPL-3.0-only

//! The **remote** tool backend — the *same* [`cosmon_agent_harness::Tool`]
//! surface as the local backend ([`crate::observe`], [`crate::ensemble`]),
//! but each `execute` calls a `cosmon-rpp-adapter` HTTP endpoint over the
//! wire instead of touching `cosmon-state` in-process.
//!
//! ## Role in the architecture — one loop, two backends (ADR-115 §6)
//!
//! cs-pilot's whole point is that *the cognitive loop stays whole; only the
//! tool backend swaps* (delib `2026-05-31-cs-pilot-external-cognitive-pilot`
//! §6). Locally, a tool loads a [`cosmon_filestore::FileStore`] and calls
//! `cosmon_state::ops::*` directly. Remotely — a thin CLI installed *outside*
//! an avatar (e.g. tenant-demo) — the *identical* tool name + declaration is
//! backed by a call to [`cosmon_remote::Client`], which speaks the ADR-080
//! §8j HTTPS+JWT wire to the avatar's [`cosmon-rpp-adapter`]. The model runs
//! **client-side** (the operator's own Ollama / API on the tenant-demo box); the
//! avatar stays a pure orchestrator with no inbound LLM compute, honouring
//! the RPP one-way topology.
//!
//! Because the model sees the same tool *names* and *schemas* regardless of
//! backend, the REPL ([`cosmon-pilot`]) is byte-identical across local and
//! remote — it never learns which backend it drives. That is the clean
//! hexagonal port the ADR calls for.
//!
//! ## §8p strict subset — what is, and is NOT, on the wire (ADR-080 §4/§5)
//!
//! The network surface is a **strict subset** of the CLI surface (§8p). This
//! backend exposes exactly the four §8p molecule routes, in two tiers:
//!
//! | Tool | Route | Tier |
//! |------|-------|------|
//! | `observe`  | `GET  /v1/molecules/:id`         | read  |
//! | `ensemble` | `GET  /v1/molecules`             | read  |
//! | `nucleate` | `POST /v1/molecules`             | write |
//! | `tackle`   | `POST /v1/molecules/:id/tackle`  | write |
//!
//! - **`peek` is deliberately absent.** `cs peek` has *no* RPP route
//!   (`docs/guides/api-cli-coverage.md`: "NO (V2 TBD)") — it is a
//!   fleet-aggregate wheat-paste raster (ADR-066), not a single-tenant
//!   molecule read. Adding a `peek` route is an explicit §8p amendment with
//!   an ADR + freeze-test update, *not* something a client may invent. The
//!   remote read-only registry therefore ships `observe` + `ensemble` only.
//! - **`done` / `evolve` / `complete` are absent forever.** They are
//!   operator-only / worker-internal (ADR-080 §5 closed list; ADR-115 §5).
//!   A cognitive pilot never tears down or self-advances a molecule over the
//!   wire. There is no remote tool for them by construction — not a gated
//!   one, an *absent* one.
//!
//! ## Read-only first; write tools opt-in (ADR-115 §5)
//!
//! [`remote_read_only_registry`] is the honest walking skeleton: a pilot that
//! can *see* the remote fleet. [`remote_registry`] adds the write tools
//! (`nucleate` / `tackle`); the `cs pilot` front door keeps them behind an
//! explicit opt-in so the default remote session is read-only.
//!
//! ## Sync↔async bridge (no daemon, no cross-runtime hazard)
//!
//! [`cosmon_agent_harness::Tool::execute`] is **synchronous**;
//! [`cosmon_remote::Client`] is **async** (reqwest). The REPL itself runs on
//! a *current-thread* runtime, so neither `block_in_place` nor a nested
//! `Handle::block_on` is available. [`run_blocking`] therefore runs each
//! request on its own isolated current-thread runtime hosted on a
//! short-lived scoped OS thread, building the reqwest-backed client *inside*
//! that runtime — so there is never a cross-runtime reactor mismatch. The
//! per-call cost (one thread + one tiny runtime) is irrelevant on a
//! human-paced REPL where each tool call answers one operator question.
//!
//! [ADR-080]: ../../docs/adr/080-remote-pilot-port-https-oidc.md
//! [ADR-115]: ../../docs/adr/115-cs-pilot-cognitive-pilot.md

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use cosmon_agent_harness::{Tool, ToolDeclaration, ToolError, ToolRegistry};
use cosmon_remote::{Client, ListFilters, NucleateRequest, Profile};
use serde::Deserialize;

use crate::{io_err, parse_args};

/// Connection material shared by every remote tool: which avatar to call and
/// the JWT to present.
///
/// Cheap to clone (`Arc<Profile>` + an `Option<String>` token) so the
/// registry builders can hand one to each tool. The `cosmon_remote::Client`
/// is **not** stored — it is rebuilt per call inside [`run_blocking`]'s
/// isolated runtime to avoid binding a reqwest connection pool to a runtime
/// that is gone by the time the next synchronous `execute` fires.
#[derive(Debug, Clone)]
pub struct RemoteBackend {
    profile: Arc<Profile>,
    token: Option<String>,
}

impl RemoteBackend {
    /// Build a backend from a resolved [`Profile`] (host + auth identity) and
    /// an optional bearer JWT.
    ///
    /// The token is normally minted by the `cs pilot` front door before the
    /// registry is built (from `$COSMON_REMOTE_TOKEN` or an OIDC mint). A
    /// `None` token leaves requests unauthenticated — every §8p route then
    /// answers `401`, surfaced to the model as a [`ToolError::Io`].
    #[must_use]
    pub fn new(profile: Profile, token: Option<String>) -> Self {
        Self {
            profile: Arc::new(profile),
            token,
        }
    }

    /// Build a fresh [`cosmon_remote::Client`] for one request.
    ///
    /// Uses [`Client::new_unchecked`] (not `new`) deliberately: a pilot that
    /// already holds a minted token does not need `oidc_url` set, so the
    /// `check_ready` gate (which requires the full four-tuple) would reject a
    /// perfectly usable read-only profile. The token, not the profile
    /// completeness, is what authorises the call.
    fn client(&self) -> Result<Client, ToolError> {
        Client::new_unchecked(&self.profile, self.token.clone())
            .map_err(|e| io_err(format!("remote client init failed: {e}")))
    }
}

/// Run one async request to completion from a synchronous [`Tool::execute`].
///
/// The closure is invoked **inside** a freshly-built current-thread runtime
/// hosted on a scoped OS thread, so the reqwest client it builds lives and
/// dies entirely within that runtime — no cross-runtime reactor mismatch,
/// and it works whether the calling REPL is on a current-thread or
/// multi-thread runtime (it spawns neither `block_in_place` nor a nested
/// `block_on`). Honours the crate's no-`unwrap`/no-`expect` rule: a runtime
/// build failure and a worker-thread panic both map to [`ToolError::Io`].
fn run_blocking<C, F, T>(make_future: C) -> Result<T, ToolError>
where
    C: FnOnce() -> F + Send,
    F: Future<Output = Result<T, ToolError>>,
    T: Send,
{
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| io_err(format!("tokio runtime build failed: {e}")))?;
            rt.block_on(make_future())
        });
        match handle.join() {
            Ok(result) => result,
            Err(_) => Err(io_err("remote request worker thread panicked")),
        }
    })
}

/// Map a [`cosmon_remote::Error`] onto the harness [`ToolError`].
///
/// A `4xx` that signals the *model's* mistake (`400 Bad Request`,
/// `422 Unprocessable`) becomes [`ToolError::InvalidArguments`] so the model
/// is nudged to retry with corrected arguments. Everything else — transport
/// failure, `401`/`403`/`404`/`5xx`, decode error — collapses to
/// [`ToolError::Io`] carrying the original `Display` message, so the model
/// still sees *why* (e.g. the adapter's structured error body).
fn map_remote_err(tool: &'static str, err: cosmon_remote::Error) -> ToolError {
    if let cosmon_remote::Error::Api { status, .. } = &err {
        if *status == 400 || *status == 422 {
            return ToolError::InvalidArguments {
                tool: tool.to_owned(),
                message: err.to_string(),
            };
        }
    }
    io_err(err)
}

// ── observe ──────────────────────────────────────────────────────────────

/// Arguments for the remote `observe` tool — a single molecule id.
///
/// Identical shape to the local [`crate::observe::ObserveInput`] so the model
/// cannot tell the backends apart.
#[derive(Debug, Deserialize)]
pub struct RemoteObserveInput {
    /// The molecule id to inspect, e.g. `"task-20260531-ffed"`.
    pub molecule_id: String,
}

/// Remote `observe` — `GET /v1/molecules/:id` against the avatar's RPP.
#[derive(Debug, Clone)]
pub struct RemoteObserveTool(pub RemoteBackend);

impl Tool for RemoteObserveTool {
    fn name(&self) -> &'static str {
        "observe"
    }

    fn declaration(&self) -> ToolDeclaration {
        // Reuse the LOCAL declaration verbatim so the model sees the exact
        // same name + schema + description on either backend (the seam the
        // whole design rests on). Only the `execute` body differs.
        crate::observe::ObserveTool.declaration()
    }

    fn execute(&self, arguments_json: &str, _work_dir: &Path) -> Result<String, ToolError> {
        let input: RemoteObserveInput = parse_args("observe", arguments_json)?;
        let backend = self.0.clone();
        run_blocking(move || async move {
            let client = backend.client()?;
            let env = client
                .get_molecule(&input.molecule_id)
                .await
                .map_err(|e| map_remote_err("observe", e))?;
            serde_json::to_string(&env.molecule).map_err(io_err)
        })
    }
}

// ── ensemble ─────────────────────────────────────────────────────────────

/// Arguments for the remote `ensemble` tool — every filter optional.
///
/// Mirrors the local [`crate::ensemble::EnsembleInput`]. The RPP query
/// surface takes a single `tag` parameter, so a multi-glob `tags` array is
/// narrowed to its first entry on the wire (the remaining globs are not yet a
/// §8p capability — flagged in the field doc, not silently dropped).
#[derive(Debug, Default, Deserialize)]
pub struct RemoteEnsembleInput {
    /// Filter by lifecycle status (`running`, `pending`, …). Omit for all.
    #[serde(default)]
    pub status: Option<String>,
    /// Filter by molecule kind (`task`, `idea`, `decision`, …). Omit for all.
    #[serde(default)]
    pub kind: Option<String>,
    /// Tag glob filters. The RPP `GET /v1/molecules` route accepts one `tag`
    /// query param, so only the first glob reaches the wire in v0; a richer
    /// multi-tag remote filter is a future §8p capability.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Filter by fleet id. Omit to read the whole tenant store.
    #[serde(default)]
    pub fleet: Option<String>,
}

/// Remote `ensemble` — `GET /v1/molecules` against the avatar's RPP.
#[derive(Debug, Clone)]
pub struct RemoteEnsembleTool(pub RemoteBackend);

impl Tool for RemoteEnsembleTool {
    fn name(&self) -> &'static str {
        "ensemble"
    }

    fn declaration(&self) -> ToolDeclaration {
        crate::ensemble::EnsembleTool.declaration()
    }

    fn execute(&self, arguments_json: &str, _work_dir: &Path) -> Result<String, ToolError> {
        let input: RemoteEnsembleInput = parse_args("ensemble", arguments_json)?;
        let backend = self.0.clone();
        run_blocking(move || async move {
            let client = backend.client()?;
            let filters = ListFilters {
                status: input.status,
                kind: input.kind,
                tag: input.tags.into_iter().next(),
                fleet: input.fleet,
            };
            let env = client
                .list_molecules(&filters)
                .await
                .map_err(|e| map_remote_err("ensemble", e))?;
            // Return the `ensemble` projection (the {molecules, total, …}
            // shape) so the model reads the same body `cs ensemble --json`
            // prints — the envelope's `request_id` is transport bookkeeping.
            serde_json::to_string(&env.ensemble).map_err(io_err)
        })
    }
}

// ── nucleate (write) ─────────────────────────────────────────────────────

/// Arguments for the remote `nucleate` tool.
#[derive(Debug, Deserialize)]
pub struct RemoteNucleateInput {
    /// Formula to nucleate, e.g. `"task-work"` or `"deep-think"`.
    pub formula: String,
    /// Molecule kind (`task` / `idea` / `decision` / …). Optional — the
    /// formula's default kind applies when omitted.
    #[serde(default)]
    pub kind: Option<String>,
    /// Formula variables (`topic`, `question`, …). Each rendered into the
    /// new molecule's `prompt.md`.
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    /// Temperature / curation tags to stamp at birth, e.g. `["temp:warm"]`.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Remote `nucleate` — `POST /v1/molecules` against the avatar's RPP.
///
/// A **write** tool (ADR-115 §5, increment 2). Reuses the avatar's
/// already-admitted `nucleate` route; the cognitive loop adds nothing to the
/// trust model — it only emits more of the same JWT-authorised requests.
#[derive(Debug, Clone)]
pub struct RemoteNucleateTool(pub RemoteBackend);

impl Tool for RemoteNucleateTool {
    fn name(&self) -> &'static str {
        "nucleate"
    }

    fn declaration(&self) -> ToolDeclaration {
        ToolDeclaration::new(
            "nucleate",
            "Create a new cosmon molecule on the remote avatar from a formula \
             (e.g. 'task-work', 'deep-think') with optional kind, variables, and \
             tags. Returns the new molecule's projection (id, status, …), the \
             same shape `cs observe <id> --json` prints. This DISPATCHES no \
             worker — reach for `tackle <id>` to start work. Write tool: it \
             changes remote state, so use it only when the operator asked to \
             create work.",
            cosmon_agent_harness::ParametersSchema::from_json(serde_json::json!({
                "type": "object",
                "properties": {
                    "formula": {
                        "type": "string",
                        "description": "Formula to nucleate, e.g. 'task-work' or 'deep-think'."
                    },
                    "kind": {
                        "type": "string",
                        "description": "Molecule kind: 'task', 'idea', 'decision', 'issue', 'deliberation'. Optional."
                    },
                    "variables": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Formula variables, e.g. {\"topic\": \"…\"}."
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Tags to stamp at birth, e.g. ['temp:warm']."
                    }
                },
                "required": ["formula"],
            })),
        )
    }

    fn execute(&self, arguments_json: &str, _work_dir: &Path) -> Result<String, ToolError> {
        let input: RemoteNucleateInput = parse_args("nucleate", arguments_json)?;
        let backend = self.0.clone();
        run_blocking(move || async move {
            let client = backend.client()?;
            let body = NucleateRequest {
                formula: input.formula,
                kind: input.kind,
                variables: input.variables,
                tags: input.tags,
            };
            let env = client
                .nucleate(&body)
                .await
                .map_err(|e| map_remote_err("nucleate", e))?;
            serde_json::to_string(&env.molecule).map_err(io_err)
        })
    }
}

// ── tackle (write) ───────────────────────────────────────────────────────

/// Arguments for the remote `tackle` tool — a single molecule id.
#[derive(Debug, Deserialize)]
pub struct RemoteTackleInput {
    /// The molecule id to dispatch a worker on, e.g. `"task-20260531-ffed"`.
    pub molecule_id: String,
}

/// Remote `tackle` — `POST /v1/molecules/:id/tackle` against the avatar's RPP.
///
/// A **write** tool (ADR-115 §5, increment 2). Dispatches a worker
/// avatar-side. Its reverse gesture, `cs done` (teardown), stays
/// operator-only and is **never** a remote tool (ADR-080 §5).
#[derive(Debug, Clone)]
pub struct RemoteTackleTool(pub RemoteBackend);

impl Tool for RemoteTackleTool {
    fn name(&self) -> &'static str {
        "tackle"
    }

    fn declaration(&self) -> ToolDeclaration {
        ToolDeclaration::new(
            "tackle",
            "Dispatch a worker on a pending cosmon molecule on the remote avatar \
             by id (Inert → Propelled): spawns the worker that runs the formula's \
             steps. Returns the tackle envelope (molecule id, worker session). \
             Write tool. Teardown (`done`) stays operator-only and is never \
             available over the wire — never claim to be able to merge or close \
             a molecule remotely.",
            cosmon_agent_harness::ParametersSchema::from_json(serde_json::json!({
                "type": "object",
                "properties": {
                    "molecule_id": {
                        "type": "string",
                        "description": "Molecule id to tackle, e.g. 'task-20260531-ffed'."
                    }
                },
                "required": ["molecule_id"],
            })),
        )
    }

    fn execute(&self, arguments_json: &str, _work_dir: &Path) -> Result<String, ToolError> {
        let input: RemoteTackleInput = parse_args("tackle", arguments_json)?;
        let backend = self.0.clone();
        run_blocking(move || async move {
            let client = backend.client()?;
            let env = client
                .tackle(&input.molecule_id)
                .await
                .map_err(|e| map_remote_err("tackle", e))?;
            serde_json::to_string(&env.tackle).map_err(io_err)
        })
    }
}

// ── registry builders ────────────────────────────────────────────────────

/// Build a [`ToolRegistry`] holding the remote **read-only** tools —
/// `observe` + `ensemble`.
///
/// The honest remote walking skeleton (ADR-115 §5): a pilot that can *see*
/// the remote fleet without the authority to change it. `peek` is absent (no
/// RPP route — see the module docs); write tools are opted into via
/// [`remote_registry`].
#[must_use]
pub fn remote_read_only_registry(backend: RemoteBackend) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(RemoteObserveTool(backend.clone())));
    registry.register(Box::new(RemoteEnsembleTool(backend)));
    registry
}

/// Build a [`ToolRegistry`] holding the remote read tools **and** the write
/// tools — `observe` + `ensemble` + `nucleate` + `tackle`.
///
/// The full §8p molecule subset (ADR-080 §4). `done` / `evolve` / `complete`
/// are *not* here — not gated, *absent by construction* (ADR-080 §5; ADR-115
/// §5). The `cs pilot` front door keeps this behind an explicit `--write`
/// opt-in so a remote session is read-only unless the operator asks for more.
#[must_use]
pub fn remote_registry(backend: RemoteBackend) -> ToolRegistry {
    let mut registry = remote_read_only_registry(backend.clone());
    registry.register(Box::new(RemoteNucleateTool(backend.clone())));
    registry.register(Box::new(RemoteTackleTool(backend)));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_backend(host: String) -> RemoteBackend {
        RemoteBackend::new(Profile::from_host(host), Some("test-jwt".to_owned()))
    }

    #[test]
    fn read_only_registry_holds_exactly_observe_and_ensemble() {
        let registry = remote_read_only_registry(test_backend("http://localhost".into()));
        let names: Vec<&str> = registry.declarations().iter().map(|d| d.name).collect();
        // BTreeMap key order — alphabetical. No `peek` (not a §8p route).
        assert_eq!(names, vec!["ensemble", "observe"]);
    }

    #[test]
    fn write_registry_adds_nucleate_and_tackle_but_never_done() {
        let registry = remote_registry(test_backend("http://localhost".into()));
        let names: Vec<&str> = registry.declarations().iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["ensemble", "nucleate", "observe", "tackle"]);
        // `done` / `evolve` / `complete` are structurally absent — never a
        // remote tool (ADR-080 §5; ADR-115 §5).
        assert!(!names.contains(&"done"));
        assert!(!names.contains(&"evolve"));
        assert!(!names.contains(&"complete"));
    }

    #[test]
    fn remote_declarations_match_local_for_observe_and_ensemble() {
        // The model must not be able to tell the backends apart: the remote
        // observe/ensemble declarations are the local ones verbatim.
        let backend = test_backend("http://localhost".into());
        assert_eq!(
            RemoteObserveTool(backend.clone()).declaration().name,
            crate::observe::ObserveTool.declaration().name
        );
        assert_eq!(
            RemoteObserveTool(backend.clone()).declaration().description,
            crate::observe::ObserveTool.declaration().description
        );
        assert_eq!(
            RemoteEnsembleTool(backend).declaration().description,
            crate::ensemble::EnsembleTool.declaration().description
        );
    }

    #[test]
    fn invalid_json_is_invalid_arguments_without_touching_the_wire() {
        let backend = test_backend("http://127.0.0.1:1".into());
        let err = RemoteObserveTool(backend)
            .execute("not json", Path::new("."))
            .expect_err("must reject before any HTTP call");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[test]
    fn map_remote_err_routes_client_input_errors_to_invalid_arguments() {
        let bad_request = cosmon_remote::Error::Api {
            status: 400,
            body: serde_json::json!({"error": "bad"}),
        };
        assert!(matches!(
            map_remote_err("nucleate", bad_request),
            ToolError::InvalidArguments { .. }
        ));

        let not_found = cosmon_remote::Error::Api {
            status: 404,
            body: serde_json::json!({"error": "absent"}),
        };
        assert!(matches!(
            map_remote_err("observe", not_found),
            ToolError::Io(_)
        ));
    }
}
