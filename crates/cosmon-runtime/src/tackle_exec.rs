// SPDX-License-Identifier: AGPL-3.0-only

//! The **effect half of a tackle as a library** — plan → execute with no
//! `cs` subprocess (issue #54 / U5).
//!
//! # Role in the architecture
//!
//! U4 extracted the *decision* half of `cs tackle` into
//! [`cosmon_core::tackle_plan`]: pure functions that turn typed inputs into
//! a [`TacklePlan`]. This module is the sibling *effect* half: it consumes
//! that plan and performs the dispatch — worktree + branch creation, the
//! ledger record **before** the spawn ([`crate::dispatch_ledger`]), the
//! spawn itself through the injectable
//! [`cosmon_core::transport::TransportBackend`] port, and the rollback
//! symmetry when the spawn fails. Together they give a library consumer
//! (the resident runtime, the rpp-adapter after U6, a test) the sequence
//! `cs tackle` performs, without shelling the `cs` binary — the runtime
//! dependency the ADR-080 §3.5 clause (e) envelope encodes and issue #54
//! retires.
//!
//! # What this executor covers, and what it deliberately does not (yet)
//!
//! [`LibraryExecutor`] handles the **worker-spawn** dispatch: a molecule
//! whose current step wants an agent worker. It resolves the adapter/model
//! chains ([`cosmon_core::tackle_plan::resolve_selection`]) honouring a
//! [`DispatchPin`], emits the `AdapterSelected` / `ModelSelected`
//! attribution events, builds the plan, creates the worktree, records the
//! dispatch, and spawns through the injected backend.
//!
//! It does **not** yet cover the CLI-only execution kinds and gates that
//! `cmd::tackle::run` routes before its spawn phase — gate / native /
//! query / llm steps, the capability and briefless guards, the model-budget
//! ceiling (an event-log fold), the adapter preflight probes, and the
//! per-adapter spawn arms (claude readiness pipeline, detached local
//! worker, in-process Direct-API loops). A step of an unsupported kind is
//! refused with a **typed** [`TackleExecError::UnsupportedStep`] rather
//! than half-executed, so a caller can fall back to the full CLI path. The
//! remaining parity is the U6 cut-over's work; until it lands,
//! [`crate::SubprocessExecutor`] stays the default executor for
//! [`crate::Runtime`] and the resident loop, and this executor is the
//! opt-in library door.
//!
//! # Ordering contract
//!
//! The executor preserves `cs tackle`'s effect ordering exactly where it
//! matters:
//!
//! 1. selection + attribution events (before any filesystem side effect);
//! 2. worktree + branch creation;
//! 3. **ledger record before spawn** ([`dispatch_ledger::commit_dispatch`]
//!    — the executor's one spawn seam, `spawn_recorded`, takes the returned
//!    token by reference, so spawn-then-record does not compile on this
//!    path);
//! 4. spawn + prompt injection through the transport port;
//! 5. on spawn failure: ledger rollback, `WorkerSpawnRolledBack` emission,
//!    worktree/branch cleanup — the same symmetry `cs tackle` guarantees.

use std::path::{Path, PathBuf};

use cosmon_core::config::AdaptersConfig;
use cosmon_core::error::CosmonError;
use cosmon_core::harness_settings::UnsupportedHarnessCarrier;
use cosmon_core::id::{AgentId, MoleculeId, WorkerId};
use cosmon_core::injection::{InjectionOrigin, InjectionProvenance};
use cosmon_core::spawn_seam::UnknownAdapter;
use cosmon_core::tackle::TackledBy;
use cosmon_core::tackle_plan::{
    resolve_selection, MoleculeBrief, PromptRequest, SelectionRequest, TacklePlan,
};
use cosmon_core::transport::{AgentDefinition, RuntimeConfig, TransportBackend};
use cosmon_filestore::FileStore;
use cosmon_state::events::worker_spawn::{
    emit_adapter_selected, emit_model_selected, emit_worker_spawn_rolled_back,
};
use cosmon_state::{MoleculeData, StateStore};

use crate::dispatch_ledger::{self, DispatchLedgerError, DispatchRecord};
use crate::{DispatchPin, Executor, RuntimeError};

/// Failure modes of the library tackle executor.
///
/// Typed so callers (the runtime's [`Executor`] adaptation, the rpp-adapter
/// after U6, tests) can distinguish "fall back to the CLI path"
/// ([`Self::UnsupportedStep`]) from a real dispatch failure.
#[derive(Debug, thiserror::Error)]
pub enum TackleExecError {
    /// A state-store read, write, or identifier construction failed.
    #[error("state store error: {0}")]
    State(#[from] CosmonError),

    /// A derived identifier (worker / agent id from the session name) was
    /// not well-formed.
    #[error("identifier error: {0}")]
    Id(#[from] cosmon_core::id::IdError),

    /// The dispatch ledger refused to record the dispatch.
    #[error("dispatch ledger error: {0}")]
    Ledger(#[from] DispatchLedgerError),

    /// The resolved adapter name is not in the dispatch registry.
    #[error(transparent)]
    UnknownAdapter(#[from] UnknownAdapter),

    /// The executing step pinned `[steps.harness]` settings and the resolved
    /// adapter has no channel to carry them (ADR-177 / issue #65).
    ///
    /// Refused rather than dropped: a spore whose harness pin looks honoured on
    /// every adapter and is honoured on two is worse than one that refuses
    /// loudly on the adapters it cannot serve.
    #[error(transparent)]
    UnsupportedHarnessCarrier(#[from] UnsupportedHarnessCarrier),

    /// The molecule is in a terminal state and cannot be tackled.
    #[error("molecule {id} is {status} — cannot tackle a terminal molecule")]
    NotTackleable {
        /// The refused molecule.
        id: Box<MoleculeId>,
        /// The terminal status it reads.
        status: String,
    },

    /// The current formula step is an execution kind this executor does not
    /// cover (gate / native / query / llm). The caller should route the
    /// molecule through the full CLI path instead.
    #[error(
        "molecule {id}: step '{step_id}' is a {kind} step — the library \
         executor covers worker-spawn steps only (U5); dispatch it through \
         `cs tackle` until the U6 cut-over lands"
    )]
    UnsupportedStep {
        /// The refused molecule.
        id: Box<MoleculeId>,
        /// The formula step id.
        step_id: String,
        /// The step's execution kind (`gate` / `native` / `query` / `llm`).
        kind: &'static str,
    },

    /// A `git` invocation failed (worktree / branch creation, repo probe).
    #[error("git error: {0}")]
    Git(String),

    /// The transport backend could not spawn the worker or deliver its
    /// prompt. The ledger entry has already been rolled back when this
    /// surfaces.
    #[error("spawn failed for molecule {id}: {reason}")]
    Spawn {
        /// The molecule whose worker could not be spawned.
        id: Box<MoleculeId>,
        /// The transport-level failure.
        reason: String,
    },

    /// The session **spawned**, its briefing could not be delivered, and
    /// the teardown of that session could not be confirmed either.
    ///
    /// This is the one dispatch failure that deliberately leaves state
    /// behind. A tmux worker is committed to the operating system the
    /// instant the spawn returns (§8ab); rolling the ledger back and
    /// removing the worktree under a process that may still be running
    /// produces exactly the shape §8ab forbids — *an effect without a
    /// record is visible to nothing*. So the dispatch record and the
    /// worktree are RETAINED, and this error names both so an operator (or
    /// a sweep) can finish the teardown.
    #[error(
        "molecule {id}: worker session '{session_name}' spawned but its \
         briefing could not be delivered ({reason}), and terminating that \
         session could not be confirmed ({termination}); the dispatch \
         record and the worktree at {} are RETAINED so the possibly-live \
         worker stays discoverable — verify and tear it down by hand",
        worktree.display()
    )]
    OrphanRetained {
        /// The molecule whose worker may still be running.
        id: Box<MoleculeId>,
        /// The transport session that was created and may survive.
        session_name: String,
        /// Why the briefing could not be delivered.
        reason: String,
        /// Why the teardown could not be confirmed.
        termination: String,
        /// The retained worktree.
        worktree: PathBuf,
    },

    /// A dispatch failed and the rollback deliberately kept resources it
    /// had not created.
    ///
    /// [`create_worktree`] is idempotent: it reuses an existing worktree
    /// and tolerates an existing branch. Rollback therefore removes only
    /// what *this* attempt allocated ([`WorktreeOwnership`]) — retrying a
    /// crashed worker whose worktree was preserved must never destroy that
    /// worker's uncommitted work. The wrapper exists so the outcome is
    /// visible on the wire and in the log rather than being a silent
    /// asymmetry.
    #[error("{source} — rollback preserved pre-existing {preserved}")]
    RolledBackPreserving {
        /// The dispatch failure that triggered the rollback.
        source: Box<TackleExecError>,
        /// What was deliberately left in place.
        preserved: String,
    },
}

/// Which of a dispatch's filesystem resources **this attempt actually
/// created** — the receipt [`create_worktree`] hands back so rollback can
/// tell its own allocations from someone else's.
///
/// # Why this exists
///
/// `create_worktree` is idempotent by design: an existing worktree is
/// reused and an existing branch is tolerated, so a retry after a crashed
/// worker lands on the work the crash left behind. Rollback used to be
/// unconditional (`git worktree remove --force` then `git branch -D`), so
/// a retry that then failed for an unrelated reason — a refused ledger
/// commit, a backend with no seats — destroyed that prior work instead of
/// undoing its own allocations. Ownership is the missing half of
/// idempotence: what you did not create, you do not get to delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorktreeOwnership {
    /// `true` when this call created the worktree directory; `false` when
    /// it reused one that already existed.
    pub worktree_created: bool,
    /// `true` when this call created the feature branch; `false` when the
    /// branch already existed.
    pub branch_created: bool,
}

impl WorktreeOwnership {
    /// Everything was created by this attempt — rollback may remove both.
    const OWNS_ALL: Self = Self {
        worktree_created: true,
        branch_created: true,
    };

    /// Nothing was created by this attempt — rollback must remove neither.
    const OWNS_NOTHING: Self = Self {
        worktree_created: false,
        branch_created: false,
    };

    /// Name what a rollback under this receipt would deliberately keep, or
    /// `None` when the attempt owns everything it touched.
    ///
    /// The wording lands in [`TackleExecError::RolledBackPreserving`], so
    /// an operator reading a failed dispatch sees which resources survived
    /// on purpose rather than wondering whether cleanup half-ran.
    #[must_use]
    fn preserved_note(self, worktree_path: &Path, branch: &str) -> Option<String> {
        let mut kept = Vec::new();
        if !self.worktree_created {
            kept.push(format!("worktree {}", worktree_path.display()));
        }
        if !self.branch_created {
            kept.push(format!("branch {branch}"));
        }
        if kept.is_empty() {
            None
        } else {
            Some(kept.join(" and "))
        }
    }
}

/// What a successful library dispatch leaves behind — the receipt the
/// caller can hand to observers.
#[derive(Debug, Clone)]
pub struct TackleReceipt {
    /// The dispatched molecule.
    pub molecule_id: MoleculeId,
    /// The worker registered on the fleet ledger.
    pub worker: WorkerId,
    /// The transport session name the worker owns.
    pub session_name: String,
    /// The branch the worker commits to (`feat/<mol-id>`).
    pub branch_name: String,
    /// The worktree the worker writes in.
    pub worktree_path: PathBuf,
}

/// How a spawn attempt failed, in the only dimension the caller must act
/// on: whether anything the transport created can still be running.
///
/// The dispatch ledger's rollback is safe exactly when nothing survives the
/// failure. When a session was spawned and could not be confirmed torn
/// down, rolling the record back would erase the only trace of a live,
/// paid process (§8ab) — so that case is typed apart rather than folded
/// into a string.
#[derive(Debug)]
enum SpawnAttemptFailure {
    /// Nothing survives: either the spawn itself failed, or the briefing
    /// failed and the session was confirmed terminated. The caller rolls
    /// the ledger back as usual.
    Rolled(String),
    /// A session was spawned, its briefing failed, and terminating it did
    /// not succeed. The caller retains the dispatch record and worktree.
    Unterminated {
        /// Why the briefing could not be delivered.
        reason: String,
        /// Why the teardown could not be confirmed.
        termination: String,
    },
}

/// Where one dispatch reads its state, formulas, and project config.
///
/// # Why this exists
///
/// The executor used to resolve all three through the `cosmon_filestore`
/// `*_from` helpers, which consult `COSMON_STATE_DIR`,
/// `COSMON_FORMULAS_DIR` and `COSMON_CONFIG` **before** the caller-supplied
/// directory. On a single-tenant CLI that ordering is the feature: an
/// operator's explicit override outranks walk-up. On the multi-tenant RPP
/// path it is a confusion of authority — the route authorises and observes
/// the molecule in the admitted tenant's deterministic store
/// (`<tenant_root>/.cosmon/state`), and the executor would then load and
/// mutate whatever store the adapter process happened to inherit. With a
/// same-named molecule there, the wrong record is dispatched into the
/// admitted tenant's worktree; without one, an authorised molecule fails as
/// if it did not exist. The worker envelope pins `COSMON_STATE_DIR` for the
/// *child*, which cannot repair a read the parent already performed.
///
/// So the paths become an explicit value: [`Self::rooted_at`] for a caller
/// that knows its tenant (the RPP routes, the drain), [`Self::ambient`] for
/// a caller that genuinely wants operator overrides to win (the CLI, the
/// resident runtime on a developer machine).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantPaths {
    /// The state directory (`<root>/.cosmon/state`) molecules are read
    /// from and written to.
    pub state_dir: PathBuf,
    /// The directory formula TOML files are resolved against.
    pub formulas_dir: PathBuf,
    /// The project config file (`<root>/.cosmon/config.toml`).
    pub config_path: PathBuf,
}

impl TenantPaths {
    /// The deterministic layout under a project root: no environment
    /// variable can move any of the three.
    ///
    /// This is the spelling the RPP adapter's authorisation and observe
    /// paths already use, so a dispatch cannot read a different store from
    /// the one the request was admitted against.
    #[must_use]
    pub fn rooted_at(root: &Path) -> Self {
        let cosmon = root.join(cosmon_filestore::resolve::COSMON_DIR_NAME);
        Self {
            state_dir: cosmon.join("state"),
            formulas_dir: cosmon.join("formulas"),
            config_path: cosmon.join("config.toml"),
        }
    }

    /// The historical resolution: `COSMON_STATE_DIR` /
    /// `COSMON_FORMULAS_DIR` / `COSMON_CONFIG` first, then walk-up from
    /// `cwd`, then the `$HOME` fallbacks.
    ///
    /// Kept for callers that *want* ambient overrides — a `cs` invocation
    /// whose operator exported one is asking for exactly this.
    #[must_use]
    pub fn ambient(cwd: &Path) -> Self {
        Self {
            state_dir: cosmon_filestore::resolve_state_dir_from(cwd),
            formulas_dir: cosmon_filestore::resolve_formulas_dir_from(cwd),
            config_path: cosmon_filestore::resolve_config_path_from(cwd),
        }
    }
}

/// The library implementation of the runtime's [`Executor`] seam: plan →
/// execute in-process, spawning through an injectable transport backend
/// instead of shelling `cs tackle`.
///
/// This is the U5 deliverable of issue #54: with it, a
/// [`crate::Runtime`] can dispatch worker-spawn molecules with **no `cs`
/// binary on `PATH` at all** — the decision half runs via
/// [`cosmon_core::tackle_plan`], the ledger via
/// [`crate::dispatch_ledger`], and the process creation via whatever
/// [`TransportBackend`] the embedder injects (`cosmon_transport`'s
/// `TmuxBackend` in production, its `MockBackend` in tests).
///
/// # Where it reads from
///
/// [`Self::new`] resolves state / formulas / config the way `cs` does —
/// `COSMON_STATE_DIR` & co. first, then walk-up from `cwd`. A multi-tenant
/// embedder must not inherit that: it calls [`Self::with_paths`] with
/// [`TenantPaths::rooted_at`] so the dispatch reads the very store the
/// request was authorised against.
///
/// # Not yet the default
///
/// [`crate::SubprocessExecutor`] remains the default executor for one more
/// release, because `cs tackle` still owns the execution kinds and guards
/// this executor refuses (see the module docs). Once the U6 cut-over gives
/// the library path step-kind parity, the defaults flip and the subprocess
/// executor stays available behind its explicit constructor only.
#[derive(Debug, Clone)]
pub struct LibraryExecutor<B> {
    /// Project root containing `.cosmon/` — the dispatch's git root and
    /// the origin of [`TenantPaths::ambient`] walk-up.
    cwd: PathBuf,
    /// Where this dispatch reads state, formulas, and project config.
    ///
    /// Ambient by default (the CLI-shaped caller); [`Self::with_paths`]
    /// replaces it with the tenant's deterministic layout.
    paths: TenantPaths,
    /// The transport port workers are spawned through.
    backend: B,
    /// The actor class stamped on the anti-preemption lease.
    by: TackledBy,
}

impl<B: TransportBackend> LibraryExecutor<B> {
    /// Build a library executor rooted at `cwd`, spawning through `backend`.
    ///
    /// The dispatch claim is stamped `runtime:<pid>` — the same actor class
    /// [`crate::SubprocessExecutor`] passes via `--by`, so the walker's
    /// "manual always wins" lease semantics are identical on both paths.
    #[must_use]
    pub fn new(cwd: impl Into<PathBuf>, backend: B) -> Self {
        let cwd = cwd.into();
        let paths = TenantPaths::ambient(&cwd);
        Self {
            cwd,
            backend,
            by: TackledBy::Runtime {
                pid: std::process::id(),
            },
            paths,
        }
    }

    /// Pin the state / formulas / config this executor reads, instead of
    /// resolving them from the ambient environment.
    ///
    /// A multi-tenant embedder (the RPP adapter's tackle and run routes)
    /// MUST call this with [`TenantPaths::rooted_at`] of the admitted
    /// tenant root: it is what makes the route's "tenant-deterministic"
    /// claim true for the dispatch itself and not only for the worker it
    /// spawns.
    #[must_use]
    pub fn with_paths(mut self, paths: TenantPaths) -> Self {
        self.paths = paths;
        self
    }

    /// Override the actor class recorded on the dispatch claim.
    ///
    /// A library embedder acting on a direct operator gesture (the
    /// rpp-adapter after U6) records `human`; the resident runtime keeps
    /// the `runtime:<pid>` default.
    #[must_use]
    pub fn with_tackled_by(mut self, by: TackledBy) -> Self {
        self.by = by;
        self
    }

    /// Plan and execute one dispatch in-process.
    ///
    /// This is the `plan → execute` sequence `cs tackle` performs, minus
    /// the CLI-only guards and adapter arms named in the module docs. The
    /// pin carries a prior dispatch's adapter/model so a **re-dispatch**
    /// reproduces the first resolution instead of re-reading ambient
    /// environment — the same contract
    /// [`crate::SubprocessExecutor`] honours by stamping `--adapter` /
    /// `--model` and stripping the model env vars.
    ///
    /// # Errors
    ///
    /// See [`TackleExecError`]; on any error after the ledger commit, the
    /// ledger entry has been rolled back and the partial worktree removed.
    pub fn tackle(
        &self,
        id: &MoleculeId,
        pin: &DispatchPin,
    ) -> Result<TackleReceipt, TackleExecError> {
        let state_dir = self.paths.state_dir.clone();
        let store = FileStore::new(&state_dir);
        let mol = store.load_molecule(id)?;
        if !mol.status.is_alive() {
            return Err(TackleExecError::NotTackleable {
                id: Box::new(id.clone()),
                status: mol.status.to_string(),
            });
        }

        // Resolve the formula (best-effort, like the runtime's native-tail
        // drain): an id that does not resolve degrades the per-step pins,
        // it does not block dispatch.
        let formulas_dir = &self.paths.formulas_dir;
        let formula_path = formulas_dir.join(format!("{}.formula.toml", mol.formula_id.as_str()));
        let formula = std::fs::read_to_string(&formula_path)
            .ok()
            .and_then(|text| cosmon_core::formula::Formula::parse(&text).ok());

        // Refuse the execution kinds this executor does not cover, with a
        // typed error the caller can route on (module docs, "what this
        // executor covers").
        if let Some(step) = formula.as_ref().and_then(|f| f.steps.get(mol.current_step)) {
            let kind = if step.is_gate() {
                Some("gate")
            } else if step.is_native() {
                Some("native")
            } else if step.is_query() {
                Some("query")
            } else if step.is_llm() {
                Some("llm")
            } else {
                None
            };
            if let Some(kind) = kind {
                return Err(TackleExecError::UnsupportedStep {
                    id: Box::new(id.clone()),
                    step_id: step.id.clone(),
                    kind,
                });
            }
        }

        // Selection inputs. A pinned re-dispatch reproduces the recorded
        // resolution: the pin occupies the flag rung of both chains and the
        // ambient model env is ignored — the in-process equivalent of
        // `SubprocessExecutor::apply_dispatch_pin`'s `--adapter`/`--model` +
        // env strip.
        let pinned = pin.is_pinned();
        let env_default_adapter = if pinned {
            None
        } else {
            std::env::var("COSMON_DEFAULT_ADAPTER").ok()
        };
        let env_model = if pinned { None } else { env_default_model() };
        let config_path = self.paths.config_path.clone();
        let project_config =
            cosmon_filestore::load_project_config(&config_path).unwrap_or_default();
        let global_cfg_path = global_adapter_config_path();
        let global_adapters = load_global_adapters(&global_cfg_path);

        let selection = resolve_selection(&SelectionRequest {
            adapter_flag: pin.adapter.as_deref(),
            model_flag: pin.model.as_deref(),
            formula: formula.as_ref(),
            current_step: mol.current_step,
            env_default_adapter: env_default_adapter.as_deref(),
            env_default_model: env_model.as_ref().map(|(v, k)| (v.as_str(), *k)),
            project_adapters: project_config.adapters.as_ref(),
            config_path: &config_path,
            global_adapters: global_adapters.as_ref(),
            global_config_path: &global_cfg_path,
            formula_absence: None,
            // No `--harness` rung on this path: the in-process executor has no
            // CLI flag. A `[steps.harness]` pin still resolves below and is
            // refused at the spawn seam rather than dropped (ADR-177: a setting
            // is never silently dropped).
            harness_flag: &cosmon_core::harness_settings::HarnessMap::new(),
        })?;

        // Attribution events, co-minted with the dispatch exactly as the
        // CLI does — before any filesystem side effect, best-effort by the
        // emit helpers' own contract.
        emit_adapter_selected(
            &state_dir,
            id,
            selection.adapter.as_str(),
            selection.adapter_source.clone(),
            None,
            selection.loop_ownership,
        );
        emit_model_selected(
            &state_dir,
            id,
            selection.adapter.as_str(),
            selection.preferred_model.as_deref(),
            selection.model_source.clone(),
        );

        // Prompt inputs. The briefing is read as-written; the fleet-template
        // injection and committee-posture delivery stay with the CLI until
        // U6 (they are dispatch-adjacent conveniences, not the contract).
        let mol_dir = store.molecule_dir(id);
        let briefing = std::fs::read_to_string(mol_dir.join("briefing.md")).ok();
        let repo_root = git_repo_root(&self.cwd)?;
        let plan = TacklePlan::from_parts(
            selection,
            &PromptRequest {
                molecule: molecule_brief(&mol),
                formula: formula.as_ref(),
                briefing: briefing.as_deref(),
                config: &project_config,
                molecule_dir: &mol_dir,
                workdir: None,
                no_worktree: false,
                repo_root: Some(&repo_root),
            },
            mol.base_branch.clone(),
            None,
        );

        self.execute(&store, &state_dir, &repo_root, &mol, &plan)
    }

    /// The effect half proper: worktree → ledger → spawn, with the
    /// rollback symmetry on failure.
    ///
    /// The worktree cleanup lives HERE, on the single exit seam around
    /// [`Self::dispatch_in_worktree`], so **every** post-worktree error path
    /// — a malformed identifier, a refused ledger commit, a failed spawn —
    /// removes the worktree and branch just created. When each error path
    /// carried its own cleanup, the ones that arrived later (`WorkerId::new`,
    /// `commit_dispatch`, `AgentId::new`) simply had none, and the worktree
    /// leaked in contradiction of the rollback contract documented on
    /// [`Self::tackle`].
    fn execute(
        &self,
        store: &FileStore,
        state_dir: &Path,
        repo_root: &Path,
        mol: &MoleculeData,
        plan: &TacklePlan,
    ) -> Result<TackleReceipt, TackleExecError> {
        let worktree_path = plan
            .worktree_path
            .clone()
            .unwrap_or_else(|| repo_root.join(".worktrees").join(plan.molecule_id.as_str()));
        let ownership = create_worktree(
            repo_root,
            &worktree_path,
            &plan.branch_name,
            plan.base_branch.as_deref(),
        )?;

        match self.dispatch_in_worktree(store, state_dir, repo_root, mol, plan, &worktree_path) {
            Ok(receipt) => Ok(receipt),
            // The one failure that must NOT clean up: a session was
            // spawned, its briefing failed, and its teardown could not be
            // confirmed. Removing the worktree under a possibly-live worker
            // is the §8ab shape the retention exists to avoid.
            Err(e @ TackleExecError::OrphanRetained { .. }) => Err(e),
            Err(e) => {
                match remove_worktree_and_branch(
                    repo_root,
                    &worktree_path,
                    &plan.branch_name,
                    ownership,
                ) {
                    None => Err(e),
                    Some(preserved) => Err(TackleExecError::RolledBackPreserving {
                        source: Box::new(e),
                        preserved,
                    }),
                }
            }
        }
    }

    /// Ledger → spawn inside an already-created worktree.
    ///
    /// Split from [`Self::execute`] so the caller owns the worktree cleanup
    /// on ANY error returned from here (see `execute`'s docs). This function
    /// still owns the ledger rollback, because only it knows whether the
    /// commit landed.
    fn dispatch_in_worktree(
        &self,
        store: &FileStore,
        state_dir: &Path,
        repo_root: &Path,
        mol: &MoleculeData,
        plan: &TacklePlan,
        worktree_path: &Path,
    ) -> Result<TackleReceipt, TackleExecError> {
        // Harness settings (ADR-177 / issue #65). This executor spawns through
        // the agent-definition seam, which carries no per-adapter override
        // channel, so a `[steps.harness]` pin that reached here has nowhere to
        // go. Refuse before the ledger commit, naming the adapter — never drop
        // it. An empty map (every dispatch that pins nothing) renders to an
        // empty slice and this is a no-op.
        let _harness_args = cosmon_core::harness_settings::render_harness_args(
            plan.adapter.as_str(),
            &plan.harness,
        )
        .map_err(TackleExecError::UnsupportedHarnessCarrier)?;

        let session_name =
            cosmon_core::slugify::session_name_for(mol.display_topic(), plan.molecule_id.as_str());
        let wid = WorkerId::new(&session_name)?;
        // Identifier derivation is pure and fallible — do ALL of it before
        // the ledger commit, so a malformed id can never strand a committed
        // ledger entry (it used to sit between commit and spawn, where its
        // `?` skipped the rollback).
        let agent_id = AgentId::new(&session_name)?;

        // Ledger BEFORE spawn — the token is what authorises the spawn
        // below; see `crate::dispatch_ledger` for the six molecules lost to
        // the other order.
        let pre_dispatch_snapshot = mol.clone();
        let (_updated, recorded) = dispatch_ledger::commit_dispatch(
            store,
            mol,
            &DispatchRecord {
                worker: &wid,
                session_name: &session_name,
                adapter: &plan.adapter,
                loop_ownership: plan.loop_ownership,
                model: plan.preferred_model.as_deref(),
                tackled_by: self.by.clone(),
                worktree_path,
                repo_root,
            },
        )?;
        debug_assert_eq!(recorded.molecule(), &plan.molecule_id);

        let agent = AgentDefinition {
            id: agent_id,
            role: mol
                .assigned_role
                .unwrap_or(cosmon_core::agent::AgentRole::Implementation),
            command: plan.adapter.as_str().to_owned(),
            args: Vec::new(),
            // ADR-079 §5 obligation 3: the worker runs *in* the molecule
            // worktree. Creating the worktree above is not enough — a backend
            // that spawns a bare session inherits this process's cwd (in the
            // RPP image, `/cosmon`), and the worker's `cs` walk-up then
            // resolves to the wrong project.
            cwd: Some(worktree_path.to_path_buf()),
        };
        // A failure inside the spawn rolls the ledger back and the caller
        // removes this attempt's worktree — `cs tackle`'s symmetry
        // contract, kept — UNLESS the attempt may have left a live session
        // behind, in which case both are retained (§8ab, see
        // `SpawnAttemptFailure`).
        match self.spawn_recorded(store, &agent, &recorded, &plan.prompt) {
            Ok(()) => {}
            Err(SpawnAttemptFailure::Rolled(reason)) => {
                dispatch_ledger::rollback_dispatch(store, &pre_dispatch_snapshot, &wid);
                emit_worker_spawn_rolled_back(
                    state_dir,
                    &plan.molecule_id,
                    &wid,
                    plan.adapter.as_str(),
                    "spawn",
                );
                return Err(TackleExecError::Spawn {
                    id: Box::new(plan.molecule_id.clone()),
                    reason,
                });
            }
            Err(SpawnAttemptFailure::Unterminated {
                reason,
                termination,
            }) => {
                // Deliberately NO ledger rollback: the record is the only
                // thing that makes the possibly-live worker findable.
                return Err(TackleExecError::OrphanRetained {
                    id: Box::new(plan.molecule_id.clone()),
                    session_name,
                    reason,
                    termination,
                    worktree: worktree_path.to_path_buf(),
                });
            }
        }

        Ok(TackleReceipt {
            molecule_id: plan.molecule_id.clone(),
            worker: wid,
            session_name,
            branch_name: plan.branch_name.clone(),
            worktree_path: worktree_path.to_path_buf(),
        })
    }

    /// Spawn through the port and deliver the briefing — reachable only with
    /// a [`dispatch_ledger::DispatchRecorded`] token in hand.
    ///
    /// This is the library path's spawn seam, the sibling of the CLI's
    /// `spawn_and_prompt`: the token parameter is what makes
    /// spawn-before-record fail to compile here too (the `dispatch_ledger`
    /// module docs make that claim for both paths). The injection target is
    /// taken from the token itself — [`DispatchRecorded::worker`] — so a
    /// commit for one molecule cannot be paired with a prompt delivery to
    /// another.
    ///
    /// On success, the PID the backend witnessed for the spawned session
    /// (when it surfaces one — see [`cosmon_core::transport::SpawnHandle::pid`])
    /// is stamped on the ledger entry with its launch fingerprint, exactly as
    /// `cs tackle` step 9 does: without it, `orphan_scan`'s PID liveness axis
    /// is blind for every adapter-dispatched molecule. Best-effort by the
    /// same contract — a failed stamp costs the PID axis, never the dispatch.
    ///
    /// [`DispatchRecorded::worker`]: dispatch_ledger::DispatchRecorded::worker
    fn spawn_recorded(
        &self,
        store: &FileStore,
        agent: &AgentDefinition,
        recorded: &dispatch_ledger::DispatchRecorded,
        prompt: &str,
    ) -> Result<(), SpawnAttemptFailure> {
        let handle = self
            .backend
            .spawn(agent, &RuntimeConfig::default())
            .map_err(|e| SpawnAttemptFailure::Rolled(e.to_string()))?;
        let provenance = InjectionProvenance::new(
            InjectionOrigin::TackleBriefing,
            "library-executor briefing delivery",
        );
        // Post-spawn failure — the seam §8ab names. The session already
        // exists and is detached: propagating the delivery error alone
        // would leave it running while the caller erased its registration
        // and its working directory. Terminate it first; only a CONFIRMED
        // teardown licenses the ordinary rollback.
        if let Err(delivery) =
            self.backend
                .send_input_observed(recorded.worker(), prompt, &provenance)
        {
            return Err(match self.backend.terminate(recorded.worker()) {
                Ok(()) => SpawnAttemptFailure::Rolled(delivery.to_string()),
                Err(termination) => SpawnAttemptFailure::Unterminated {
                    reason: delivery.to_string(),
                    termination: termination.to_string(),
                },
            });
        }

        if let Some(pid) = handle.pid {
            let start_time = cosmon_process_witness::process_start_time(pid);
            if !dispatch_ledger::stamp_pid_witness(store, recorded.molecule(), pid, start_time) {
                eprintln!(
                    "library executor: warning — could not stamp the PID witness \
                     for {}; the dispatch is recorded and supervised, but the \
                     PID liveness axis falls back to the session probe alone.",
                    recorded.molecule()
                );
            }
        }
        Ok(())
    }
}

impl<B: TransportBackend> Executor for LibraryExecutor<B> {
    fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
        self.dispatch_with_pin(id, &DispatchPin::default())
    }

    fn dispatch_with_pin(&self, id: &MoleculeId, pin: &DispatchPin) -> Result<(), RuntimeError> {
        self.tackle(id, pin)
            .map(|_receipt| ())
            .map_err(|e| match e {
                // An unsupported step kind is a PERMANENT condition: the formula
                // does not change between ticks, so an identical retry reproduces
                // the refusal exactly. Mapping it to the retryable `Dispatch`
                // class made `Runtime::run` re-dispatch it every poll interval
                // until `max_runtime` and report the known-at-first-tick refusal
                // as a timeout. The non-retryable class stops the loop with a
                // typed reason instead ([`RuntimeError::DispatchRefused`]).
                refusal @ TackleExecError::UnsupportedStep { .. } => {
                    RuntimeError::DispatchRefused {
                        id: id.clone(),
                        reason: refusal.to_string(),
                    }
                }
                other => RuntimeError::Dispatch {
                    id: id.clone(),
                    reason: other.to_string(),
                },
            })
    }
}

/// Create the worker's isolation worktree and its `feat/<mol>` branch.
///
/// Moved verbatim from `cs tackle` (issue #54 / U5) so the CLI and the
/// library executor share one implementation — a second copy of the
/// idempotence probes below is a first copy that will one day disagree.
///
/// Idempotent: an existing worktree is reused, an existing branch is
/// tolerated, an "already checked out" worktree is tolerated. Every other
/// git failure surfaces — proceeding would paper over a disk-full /
/// corrupt-repo / permission problem and cascade into a surface lie
/// downstream.
///
/// # The ownership receipt
///
/// Returns a [`WorktreeOwnership`] saying which of the two resources this
/// call actually created. Idempotence and rollback are two halves of one
/// contract: because reuse is legitimate, the undo path must be able to
/// tell a resource it allocated from one it merely found (see
/// [`WorktreeOwnership`] for the work this protects).
///
/// # Errors
///
/// Returns [`TackleExecError::Git`] when git cannot be run or fails for a
/// non-idempotence reason.
pub fn create_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    branch: &str,
    start_point: Option<&str>,
) -> Result<WorktreeOwnership, TackleExecError> {
    // If worktree already exists, reuse it — and own neither it nor the
    // branch it already carries.
    if worktree_path.exists() {
        return Ok(WorktreeOwnership::OWNS_NOTHING);
    }

    // Newcomer first-run guard (task-20260722-44ce, reported by an external
    // tester). When the branch is cut from HEAD (no blocker start-point) and
    // the repo has NO commits — an unborn HEAD, the state a fresh `git init`
    // leaves behind — `git branch feat/<mol>` fails with `fatal: not a valid
    // object name: 'main'` (git resolves the symbolic HEAD to its unborn
    // target). That was a hard first-run wall for the documented
    // `cs init` → `git init` → `cs demo` path. Materialize the base branch
    // with one empty seed commit so the branch cut below just works. This
    // fires *only* on a genuinely commit-less repo — never over existing
    // history.
    if start_point.is_none() {
        ensure_base_commit(repo_root)?;
    }

    // Create branch from start_point (blocker's branch) or HEAD (main).
    // Pre-fix (task-20260416-ef31): the result of `git branch` was
    // silently discarded. A disk-full / permission / corrupt-repo failure
    // would fall through, `git worktree add` would then also fail
    // confusingly, and the tmux session still got written with a surface
    // "Running" row — one of the mechanisms behind the surface-lie class.
    // We now check every non-"already exists" failure and surface it.
    let lossy = repo_root.to_string_lossy();
    let mut args: Vec<String> = vec![
        "-C".to_owned(),
        lossy.into_owned(),
        "branch".to_owned(),
        branch.to_owned(),
    ];
    if let Some(sp) = start_point {
        args.push(sp.to_owned());
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    // `LC_ALL=C` pins git's stderr to the English locale so the
    // "already exists" idempotence probe below survives non-English
    // operator locales. See `done.rs::try_merge_branch` for the structural
    // rationale and the 2026-05-22 (drain-worker f877) discovery.
    let branch_out = std::process::Command::new("git")
        .env("LC_ALL", "C")
        .args(refs)
        .output()
        .map_err(|e| TackleExecError::Git(format!("failed to run git branch: {e}")))?;
    let mut ownership = WorktreeOwnership::OWNS_ALL;
    if !branch_out.status.success() {
        let stderr = String::from_utf8_lossy(&branch_out.stderr);
        // The ONLY tolerated failure is "branch already exists" — tackle is
        // idempotent when re-invoked on the same molecule, so the branch
        // may legitimately predate this call (e.g. `--force` respawn,
        // partial prior tackle, manual `git branch`). Any other failure is
        // unexpected and MUST surface: proceeding would silently paper
        // over a disk-full / corrupt-repo / permission problem and then
        // cascade into a surface lie downstream.
        if !stderr.contains("already exists") {
            return Err(TackleExecError::Git(format!(
                "git branch {branch} failed: {}",
                stderr.trim()
            )));
        }
        // Tolerated, but NOT ours: a branch that predates this call is a
        // prior dispatch's, and rollback leaves it alone.
        ownership.branch_created = false;
    }

    // Create worktree directory parent.
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| TackleExecError::Git(format!("failed to create worktree parent: {e}")))?;
    }

    // `LC_ALL=C` pins git's stderr to the English locale so the
    // "already checked out" / "already exists" idempotence probe below
    // survives non-English operator locales (drain-worker f877,
    // 2026-05-22).
    let output = std::process::Command::new("git")
        .env("LC_ALL", "C")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "worktree",
            "add",
            &worktree_path.to_string_lossy(),
            branch,
        ])
        .output()
        .map_err(|e| TackleExecError::Git(format!("failed to run git worktree add: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // If worktree already checked out, that's fine.
        if stderr.contains("already checked out") || stderr.contains("already exists") {
            pin_operator_identity(repo_root, worktree_path);
            ownership.worktree_created = false;
            return Ok(ownership);
        }
        return Err(TackleExecError::Git(format!(
            "git worktree add failed: {}",
            stderr.trim()
        )));
    }

    // Pin the operator identity at the worktree seam (delib-20260717-194b,
    // F2). This is the single choke point every adapter passes through, so
    // feature commits are BORN operator-authored — no post-hoc rewrite, no
    // SHA churn, no ancestry-guard breakage. The `cs done` author-slot
    // assertion (F4) is the backstop for when this silently no-ops (env
    // precedence, a late amend); pinning here reduces the failure *rate*,
    // the assertion *closes* the hole. Best-effort: a failure to resolve or
    // set identity never blocks tackle (the assertion catches the residue).
    pin_operator_identity(repo_root, worktree_path);

    Ok(ownership)
}

/// Materialize the base branch when the repository has no commits yet.
///
/// A freshly `git init`'d repository has an *unborn HEAD*: the symbolic ref
/// `HEAD` points at `refs/heads/main` (or whatever `init.defaultBranch`
/// names), but that ref does not resolve to any object because no commit
/// exists. In that state `git branch feat/<mol>` fails with
/// `fatal: not a valid object name: 'main'` — the exact wall an external
/// tester hit twice on the documented `cs init` → `git init` → `cs demo`
/// first-run path.
///
/// We detect that case *specifically* — `git rev-parse --verify HEAD`
/// returning non-zero means the repo has no commits — and seed a single
/// empty commit so the base branch resolves and the feature branch can be
/// cut from it. A repo that already has history returns early untouched:
/// cosmon MUST NEVER fabricate a commit over existing work.
///
/// The seed commit is authored with the operator's configured git identity
/// when one is present (walking local → global → system); if none is
/// configured — a bare CI checkout with no `user.*` — a neutral fallback
/// identity is supplied via `-c` so the commit still succeeds instead of
/// failing the newcomer's very first command with a git-identity error.
///
/// # Errors
///
/// Returns [`TackleExecError::Git`] when git cannot be run or the seed
/// commit fails.
pub fn ensure_base_commit(repo_root: &Path) -> Result<(), TackleExecError> {
    // Probe for an unborn HEAD. `rev-parse --verify HEAD` exits non-zero
    // with an unborn HEAD and zero once any commit exists. `--quiet`
    // suppresses the "Needed a single revision" noise on the expected miss.
    let head = std::process::Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "rev-parse",
            "--quiet",
            "--verify",
            "HEAD",
        ])
        .output()
        .map_err(|e| TackleExecError::Git(format!("failed to run git rev-parse: {e}")))?;
    if head.status.success() {
        // The repo already has at least one commit — leave history untouched.
        return Ok(());
    }

    // Unborn HEAD confirmed: seed one empty commit. Supply an author
    // identity only when the repo config has none, so a configured operator
    // keeps their own identity and a bare checkout still commits cleanly.
    let mut args: Vec<String> = vec!["-C".to_owned(), repo_root.to_string_lossy().into_owned()];
    if git_config_value(repo_root, "user.name").is_none()
        || git_config_value(repo_root, "user.email").is_none()
    {
        args.push("-c".to_owned());
        args.push("user.name=cosmon".to_owned());
        args.push("-c".to_owned());
        args.push("user.email=cosmon@localhost".to_owned());
    }
    args.extend([
        "commit".to_owned(),
        "--allow-empty".to_owned(),
        "-m".to_owned(),
        "cosmon: initial commit".to_owned(),
    ]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = std::process::Command::new("git")
        .env("LC_ALL", "C")
        .args(refs)
        .output()
        .map_err(|e| TackleExecError::Git(format!("failed to run git commit: {e}")))?;
    if !out.status.success() {
        return Err(TackleExecError::Git(format!(
            "the repository has no commits and cosmon could not create an \
             initial commit to branch from: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// Pin the operator's git identity onto a freshly-created worktree
/// (delib-20260717-194b, F2).
///
/// Resolves the operator identity from `repo_root`'s effective git config
/// (`user.name` / `user.email`, which walks local → global → system) and
/// writes it into the worktree so every worker git process commits with the
/// operator in the author AND committer slots. The maker (Noogram) and the
/// real adapter are credited ONLY on `Co-Authored-By:` trailers, never in
/// the author slot (direction-of-control, tolnay Q3).
///
/// Best-effort and non-fatal: when no identity is configured (a bare CI
/// checkout) nothing is written and the worktree inherits whatever the repo
/// config already carries.
fn pin_operator_identity(repo_root: &Path, worktree_path: &Path) {
    for key in ["user.name", "user.email"] {
        if let Some(value) = git_config_value(repo_root, key) {
            let _ = std::process::Command::new("git")
                .args([
                    "-C",
                    &worktree_path.to_string_lossy(),
                    "config",
                    key,
                    &value,
                ])
                .output();
        }
    }
}

/// Read a single git config value from `repo_root`'s effective config.
///
/// Returns `None` when the key is unset or the probe fails, so the caller
/// can fall back cleanly rather than inventing a value.
#[must_use]
pub fn git_config_value(repo_root: &Path, key: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C", &repo_root.to_string_lossy(), "config", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Best-effort undo of [`create_worktree`], used on the dispatch-failure
/// path — **scoped to what this attempt created**.
///
/// Mirrors the CLI's `cleanup_partial_tackle` for the subset this executor
/// creates: the worktree directory and the feature branch. Both removals
/// are gated on the `ownership` receipt, because the commands involved are
/// `git worktree remove --force` and `git branch -D`: run over a reused
/// worktree they destroy the uncommitted work of the crashed dispatch this
/// one was retrying. Best-effort throughout — this runs on a path already
/// returning an error, and a cleanup failure must not mask the original
/// cause (a leftover worktree is the recoverable shape; `cs tackle` reuses
/// it idempotently).
///
/// Returns the note naming what was deliberately preserved, or `None` when
/// the attempt owned everything it touched.
fn remove_worktree_and_branch(
    repo_root: &Path,
    worktree_path: &Path,
    branch: &str,
    ownership: WorktreeOwnership,
) -> Option<String> {
    if ownership.worktree_created {
        let _ = std::process::Command::new("git")
            .args([
                "-C",
                &repo_root.to_string_lossy(),
                "worktree",
                "remove",
                "--force",
                &worktree_path.to_string_lossy(),
            ])
            .output();
    }
    if ownership.branch_created {
        let _ = std::process::Command::new("git")
            .args(["-C", &repo_root.to_string_lossy(), "branch", "-D", branch])
            .output();
    }
    ownership.preserved_note(worktree_path, branch)
}

/// Resolve the repository root the dispatch cwd lives in.
///
/// The library sibling of the CLI's `find_repo_root`: a plain
/// `git rev-parse --show-toplevel` from the executor's configured root. The
/// library executor requires a repository — worker isolation is a worktree
/// by construction (ADR-110 / I2), and the `--no-worktree` escape hatch
/// stays CLI-only.
fn git_repo_root(cwd: &Path) -> Result<PathBuf, TackleExecError> {
    let out = std::process::Command::new("git")
        .args(["-C", &cwd.to_string_lossy(), "rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| TackleExecError::Git(format!("failed to run git rev-parse: {e}")))?;
    if !out.status.success() {
        return Err(TackleExecError::Git(format!(
            "not a git repository (from {}): {}",
            cwd.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if root.is_empty() {
        return Err(TackleExecError::Git(
            "git rev-parse --show-toplevel returned an empty root".to_owned(),
        ));
    }
    Ok(PathBuf::from(root))
}

/// Project the molecule record onto the six fields the tackle decision
/// reads — the same borrowed projection the CLI builds.
fn molecule_brief(mol: &MoleculeData) -> MoleculeBrief<'_> {
    MoleculeBrief {
        id: &mol.id,
        kind: mol.kind,
        formula_id: &mol.formula_id,
        current_step: mol.current_step,
        total_steps: mol.total_steps,
        variables: &mol.variables,
    }
}

/// `(value, var_name)` of the first set-and-non-empty model env var —
/// `$COSMON_DEFAULT_MODEL` first, the legacy `$ANTHROPIC_MODEL` second, the
/// same tier order the CLI reads.
fn env_default_model() -> Option<(String, &'static str)> {
    std::env::var("COSMON_DEFAULT_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|v| (v, "COSMON_DEFAULT_MODEL"))
        .or_else(|| {
            std::env::var("ANTHROPIC_MODEL")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|v| (v, "ANTHROPIC_MODEL"))
        })
}

/// Path of the operator's global adapter config
/// (`~/.config/cosmon/config.toml`, honouring `$COSMON_CONFIG_HOME`).
fn global_adapter_config_path() -> PathBuf {
    let config_home = std::env::var_os("COSMON_CONFIG_HOME").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into())).join(".config"),
        PathBuf::from,
    );
    config_home.join("cosmon").join("config.toml")
}

/// Best-effort read of the global `[adapters]` table. A missing or garbled
/// file falls through to `None` — it never aborts a dispatch.
fn load_global_adapters(path: &Path) -> Option<AdaptersConfig> {
    #[derive(serde::Deserialize)]
    struct GlobalAdaptersOnly {
        #[serde(default)]
        adapters: Option<AdaptersConfig>,
    }
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: GlobalAdaptersOnly = toml::from_str(&text).ok()?;
    parsed.adapters
}
