// SPDX-License-Identifier: AGPL-3.0-only

//! `decay`, `merge`, `transform` — the molecule interactions (THESIS Part
//! XVII), as one operation each that both `cs` and the RPP call.
//!
//! * [`decay`] — 1 → N: a source of kind idea, issue or deliberation
//!   completes and leaves product molecules behind it.
//! * [`merge`] — N → 1: the sources complete and a synthesis molecule is
//!   nucleated from them.
//! * [`transform`] — a molecule changes kind and keeps its identity.
//!
//! # Why one module
//!
//! The three verbs end the same way (an `interactions.jsonl` record plus a
//! legacy envelope) and two of them build the same freshly nucleated
//! [`MoleculeData`]. A second copy of either would be the drift this module
//! exists to prevent: `cs decay` and `POST /v1/molecules/{id}/decay` produce
//! the same state because they run the same function.
//!
//! # All or nothing
//!
//! Every function here reads and checks everything it needs *before* it
//! writes anything. [`merge`] in particular loads and validates **every**
//! source first; one source that is missing, outside the store the caller
//! may reach, or of a kind that cannot merge refuses the whole call with no
//! synthesis molecule, no source modified and no event appended. A caller
//! that holds a store scoped to its tenant therefore cannot merge a molecule
//! of another tenant: the id is simply not in that store, and
//! [`MergeError::SourceNotFound`] is the same answer it gives for an id that
//! never existed.
//!
//! # What stays outside
//!
//! The legacy `events.jsonl` envelope is appended through
//! [`InteractionEffects`], because the legacy event writer lives in
//! `cosmon-filestore`, which depends on this crate. The `EventV2` records
//! and the `interactions.jsonl` line are written here.

use std::collections::{BTreeSet, HashMap};
use std::io::Write as _;
use std::path::Path;

use chrono::Utc;
use cosmon_core::error::CosmonError;
use cosmon_core::event::{Envelope, Event};
use cosmon_core::event_v2::EventV2;
use cosmon_core::formula::Formula;
use cosmon_core::id::MoleculeId;
use cosmon_core::interaction::{Interaction, MoleculeLink};
use cosmon_core::kind::MoleculeKind;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::nucleate::{nucleate as core_nucleate, NucleateRequest, NucleateResult};
use serde::Serialize;

use crate::event_log;
use crate::ops::error::OpsError;
use crate::{MoleculeData, StateStore};

/// What an interaction does that this crate cannot: append the legacy event
/// envelope. `cs` and the RPP both implement it with the same writer.
pub trait InteractionEffects {
    /// Append the legacy `molecule_decayed`, `molecule_merged` or
    /// `molecule_transformed` envelope to the event log. Best effort, like
    /// every legacy record: a failure here must not undo the interaction.
    fn append_legacy_event(&self, events_path: &Path, envelope: &Envelope);
}

/// The `decay` rule that `--count` greater than one is refused: the product
/// molecules inherit the source's variables verbatim, so N of them would be
/// N byte-identical children (guard f4e1). Kept here so the `cs` guard and
/// the operation read one rule.
///
/// # Errors
///
/// [`DecayError::HomogeneousCount`] when `count > 1`.
pub fn ensure_decay_count_is_heterogeneous(
    parent: &MoleculeId,
    count: usize,
) -> Result<(), DecayError> {
    if count > 1 {
        return Err(DecayError::HomogeneousCount {
            parent: parent.clone(),
            count,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

/// Errors returned by [`decay`].
#[derive(Debug, thiserror::Error)]
pub enum DecayError {
    /// `count` greater than one would create byte-identical children.
    #[error(
        "refusing to decay {parent} into {count} byte-identical children: decay copies the \
         source's variables verbatim; create the children with separate nucleations that \
         block the source instead"
    )]
    HomogeneousCount {
        /// The molecule that was to decay.
        parent: MoleculeId,
        /// The requested number of products.
        count: usize,
    },
    /// The source molecule is not in the store.
    #[error("molecule not found: {0}")]
    SourceNotFound(MoleculeId),
    /// The source kind does not decay.
    #[error("molecule kind '{0}' cannot decay (only idea, issue and deliberation can)")]
    CannotDecay(MoleculeKind),
    /// `product_kind` is not a molecule kind.
    #[error("invalid product kind: {0}")]
    InvalidKind(String),
    /// No formula file for the products.
    #[error("formula not found: {0}")]
    FormulaNotFound(String),
    /// The formula file cannot be read or parsed.
    #[error("formula unusable: {0}")]
    FormulaParse(String),
    /// The formula cannot be instantiated from the source's variables.
    #[error("nucleation failed: {0}")]
    NucleateFailed(String),
    /// A `blocks` target is not in the store.
    #[error("blocks target not found: {0}")]
    BlockTargetNotFound(MoleculeId),
    /// The state store could not be read or written.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl OpsError for DecayError {
    fn tag(&self) -> &'static str {
        match self {
            Self::HomogeneousCount { .. } => "decay-homogeneous-count",
            Self::SourceNotFound(_) => "molecule-not-found",
            Self::CannotDecay(_) => "cannot-decay",
            Self::InvalidKind(_) => "invalid-kind",
            Self::FormulaNotFound(_) => "formula-not-found",
            Self::FormulaParse(_) => "formula-parse-failed",
            Self::NucleateFailed(_) => "nucleation-failed",
            Self::BlockTargetNotFound(_) => "block-target-not-found",
            Self::StoreUnavailable(_) => "store-unavailable",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::HomogeneousCount { .. } | Self::InvalidKind(_) => 400,
            Self::SourceNotFound(_) | Self::FormulaNotFound(_) | Self::BlockTargetNotFound(_) => {
                404
            }
            Self::CannotDecay(_) | Self::FormulaParse(_) | Self::NucleateFailed(_) => 409,
            Self::StoreUnavailable(_) => 503,
        }
    }
}

/// Errors returned by [`merge`].
#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    /// Fewer than two sources.
    #[error("merge requires at least 2 source molecules")]
    TooFewSources,
    /// A source is not in the store. For a tenant-scoped store this is also
    /// what a source belonging to another tenant looks like.
    #[error("molecule not found: {0}")]
    SourceNotFound(MoleculeId),
    /// A source exists in the store but its kind cannot merge.
    #[error("molecule {id} (kind: {kind}) cannot participate in merge")]
    SourceNotMergeable {
        /// The refused source.
        id: MoleculeId,
        /// Its kind.
        kind: MoleculeKind,
    },
    /// `product_kind` is not a molecule kind.
    #[error("invalid product kind: {0}")]
    InvalidKind(String),
    /// No formula file for the product.
    #[error("formula not found: {0}")]
    FormulaNotFound(String),
    /// The formula file cannot be read or parsed.
    #[error("formula unusable: {0}")]
    FormulaParse(String),
    /// The formula cannot be instantiated from the merged variables.
    #[error("nucleation failed: {0}")]
    NucleateFailed(String),
    /// The state store could not be read or written.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl OpsError for MergeError {
    fn tag(&self) -> &'static str {
        match self {
            Self::TooFewSources => "merge-too-few-sources",
            Self::SourceNotFound(_) => "molecule-not-found",
            Self::SourceNotMergeable { .. } => "source-not-mergeable",
            Self::InvalidKind(_) => "invalid-kind",
            Self::FormulaNotFound(_) => "formula-not-found",
            Self::FormulaParse(_) => "formula-parse-failed",
            Self::NucleateFailed(_) => "nucleation-failed",
            Self::StoreUnavailable(_) => "store-unavailable",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::TooFewSources | Self::InvalidKind(_) => 400,
            Self::SourceNotFound(_) | Self::FormulaNotFound(_) => 404,
            Self::SourceNotMergeable { .. } | Self::FormulaParse(_) | Self::NucleateFailed(_) => {
                409
            }
            Self::StoreUnavailable(_) => 503,
        }
    }
}

/// Errors returned by [`transform`].
#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    /// The molecule is not in the store.
    #[error("molecule not found: {0}")]
    MoleculeNotFound(MoleculeId),
    /// `to` is not a molecule kind.
    #[error("invalid target kind: {0}")]
    InvalidKind(String),
    /// The kind graph has no edge from the current kind to `to`.
    #[error("cannot transform {from} → {to} (valid targets: {valid:?})")]
    NotTransformable {
        /// Current kind.
        from: MoleculeKind,
        /// Requested kind.
        to: MoleculeKind,
        /// Kinds `from` can become.
        valid: Vec<MoleculeKind>,
    },
    /// The state store could not be read or written.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl OpsError for TransformError {
    fn tag(&self) -> &'static str {
        match self {
            Self::MoleculeNotFound(_) => "molecule-not-found",
            Self::InvalidKind(_) => "invalid-kind",
            Self::NotTransformable { .. } => "not-transformable",
            Self::StoreUnavailable(_) => "store-unavailable",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::MoleculeNotFound(_) => 404,
            Self::InvalidKind(_) => 400,
            Self::NotTransformable { .. } => 409,
            Self::StoreUnavailable(_) => 503,
        }
    }
}

/// Read a molecule, telling "absent" apart from "unreadable".
fn load(store: &dyn StateStore, id: &MoleculeId) -> Result<MoleculeData, Loaded> {
    store.load_molecule(id).map_err(|e| match e {
        CosmonError::MoleculeNotFound(_) => Loaded::Absent,
        other => Loaded::Unreadable(other.to_string()),
    })
}

/// Why [`load`] failed.
enum Loaded {
    Absent,
    Unreadable(String),
}

// ---------------------------------------------------------------------------
// shared building blocks
// ---------------------------------------------------------------------------

fn read_formula(formulas_dir: &Path, name: &str) -> Result<Formula, FormulaFailure> {
    let path = formulas_dir.join(format!("{name}.formula.toml"));
    if !path.exists() {
        return Err(FormulaFailure::Missing);
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| FormulaFailure::Unusable(e.to_string()))?;
    Formula::parse(&text).map_err(|e| FormulaFailure::Unusable(e.to_string()))
}

enum FormulaFailure {
    Missing,
    Unusable(String),
}

/// A freshly nucleated product: pending, at step zero, linked back to what
/// it came from. This is the molecule `cs decay` and `cs merge` have always
/// written, field for field.
fn product_molecule(
    nuc: &NucleateResult,
    fleet_id: cosmon_core::id::FleetId,
    kind: MoleculeKind,
    origin: MoleculeLink,
) -> MoleculeData {
    MoleculeData {
        id: nuc.id.clone(),
        fleet_id,
        formula_id: nuc.formula_id.clone(),
        status: nuc.status,
        variables: nuc.variables.clone(),
        assigned_worker: nuc.assigned_worker.clone(),
        created_at: nuc.created_at,
        updated_at: nuc.created_at,
        total_steps: nuc.total_steps,
        current_step: 0,
        completed_steps: Vec::new(),
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: Vec::new(),
        kind: Some(kind),
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links: vec![origin],
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: BTreeSet::new(),
        escalations: Vec::new(),
        freeze_on_last_step: false,
        expires_at: None,
        expiry_policy: None,
        originating_branch: None,
        base_branch: None,
        protected_paths: Vec::new(),
        pending_step: None,
        merged_at: None,
        harvest_reason: None,
        non_integration: None,
        prompt_seal: None,
        briefing_seals: Vec::new(),
        collapse_receipt: None,
        bootstrap_seals: Vec::new(),
        archived: false,
        last_progress_at: None,
        last_output_at: None,
        nudge_count: 0,
        last_nudged_at: None,
        propel_count: 0,
        last_propelled_at: None,
        process: None,
        energy_budget: None,
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
        adapter: None,
    }
}

/// Append an interaction to `<state_dir>/interactions.jsonl`.
fn log_interaction(state_dir: &Path, interaction: &Interaction) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir.join("interactions.jsonl"))?;
    let json = serde_json::to_string(interaction).map_err(std::io::Error::other)?;
    writeln!(file, "{json}")
}

fn ids(ids: &[MoleculeId]) -> Vec<&str> {
    ids.iter().map(MoleculeId::as_str).collect()
}

// ---------------------------------------------------------------------------
// decay
// ---------------------------------------------------------------------------

/// Inputs for [`decay`].
#[derive(Debug, Clone)]
pub struct DecayRequest<'a> {
    /// The molecule that decays.
    pub source: &'a MoleculeId,
    /// Formula the products are nucleated from.
    pub formula: &'a str,
    /// Number of products. More than one is refused
    /// ([`ensure_decay_count_is_heterogeneous`]).
    pub count: usize,
    /// Kind of the products (`task`, `idea`, …).
    pub product_kind: &'a str,
    /// Why the source decays.
    pub reason: &'a str,
    /// Wire consecutive products with `Blocks`/`BlockedBy` (A → B → C).
    pub chain: bool,
    /// The i-th product blocks the i-th molecule here.
    pub blocks: &'a [MoleculeId],
}

/// Result of a successful [`decay`].
#[derive(Debug, Clone)]
pub struct DecayView {
    /// The molecule that decayed (now completed).
    pub source: MoleculeId,
    /// The products, in creation order.
    pub products: Vec<MoleculeId>,
    /// Their kind.
    pub product_kind: MoleculeKind,
    /// Why.
    pub reason: String,
}

/// Wire shape of a [`DecayView`]: what `cs --json decay` prints and the
/// route returns.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DecayJson {
    /// Always `"decay"`.
    pub interaction: &'static str,
    /// The molecule that decayed.
    pub source: String,
    /// The products.
    pub products: Vec<String>,
    /// Their kind.
    pub product_kind: String,
    /// Why.
    pub reason: String,
}

impl DecayJson {
    /// Project a view onto its wire shape.
    #[must_use]
    pub fn from_view(view: &DecayView) -> Self {
        Self {
            interaction: "decay",
            source: view.source.as_str().to_owned(),
            products: ids(&view.products).into_iter().map(str::to_owned).collect(),
            product_kind: view.product_kind.to_string(),
            reason: view.reason.clone(),
        }
    }
}

/// `chain`: product[i] blocks product[i + 1].
fn wire_chain(store: &dyn StateStore, product_ids: &[MoleculeId]) -> Result<(), CosmonError> {
    for pair in product_ids.windows(2) {
        let (upstream, downstream) = (&pair[0], &pair[1]);
        let mut u = store.load_molecule(upstream)?;
        u.typed_links.push(MoleculeLink::Blocks {
            target: downstream.clone(),
        });
        store.save_molecule(upstream, &u)?;
        let mut d = store.load_molecule(downstream)?;
        d.typed_links.push(MoleculeLink::BlockedBy {
            source: upstream.clone(),
        });
        store.save_molecule(downstream, &d)?;
    }
    Ok(())
}

/// `blocks`: each product[i] blocks the i-th target (the last product takes
/// the rest).
fn wire_blocks(
    store: &dyn StateStore,
    product_ids: &[MoleculeId],
    targets: &[MoleculeId],
) -> Result<(), CosmonError> {
    for (i, target_id) in targets.iter().enumerate() {
        let prod_id = &product_ids[i.min(product_ids.len() - 1)];
        let mut prod = store.load_molecule(prod_id)?;
        prod.typed_links.push(MoleculeLink::Blocks {
            target: target_id.clone(),
        });
        store.save_molecule(prod_id, &prod)?;
        let mut target = store.load_molecule(target_id)?;
        target.typed_links.push(MoleculeLink::BlockedBy {
            source: prod_id.clone(),
        });
        store.save_molecule(target_id, &target)?;
    }
    Ok(())
}

/// Decay one molecule into product molecules (`cs decay`).
///
/// Everything that can refuse — the count rule, the source and its kind, the
/// product kind, the formula, every `blocks` target — is checked before the
/// first product is written.
///
/// # Errors
///
/// A [`DecayError`]; the store is untouched for every variant except
/// [`DecayError::StoreUnavailable`] raised mid-write.
pub fn decay(
    store: &dyn StateStore,
    state_dir: &Path,
    formulas_dir: &Path,
    request: &DecayRequest<'_>,
    effects: &dyn InteractionEffects,
) -> Result<DecayView, DecayError> {
    ensure_decay_count_is_heterogeneous(request.source, request.count)?;

    let mut source = load(store, request.source).map_err(|e| match e {
        Loaded::Absent => DecayError::SourceNotFound(request.source.clone()),
        Loaded::Unreadable(m) => DecayError::StoreUnavailable(m),
    })?;
    let source_kind = source.kind.unwrap_or(MoleculeKind::Task);
    if !source_kind.can_decay() {
        return Err(DecayError::CannotDecay(source_kind));
    }
    let product_kind: MoleculeKind = request
        .product_kind
        .parse()
        .map_err(|e| DecayError::InvalidKind(format!("{e}")))?;
    let formula = read_formula(formulas_dir, request.formula).map_err(|e| match e {
        FormulaFailure::Missing => DecayError::FormulaNotFound(request.formula.to_owned()),
        FormulaFailure::Unusable(m) => DecayError::FormulaParse(m),
    })?;
    for target in request.blocks {
        load(store, target).map_err(|e| match e {
            Loaded::Absent => DecayError::BlockTargetNotFound(target.clone()),
            Loaded::Unreadable(m) => DecayError::StoreUnavailable(m),
        })?;
    }

    let store_err = |e: CosmonError| DecayError::StoreUnavailable(e.to_string());
    let mut product_ids = Vec::new();
    let mut rng = rand::thread_rng();
    for _ in 0..request.count {
        let nuc = core_nucleate(
            NucleateRequest {
                formula: &formula,
                variables: source.variables.clone(),
                assign: source.assigned_worker.clone(),
            },
            &mut rng,
        )
        .map_err(|e| DecayError::NucleateFailed(e.to_string()))?;
        let product = product_molecule(
            &nuc,
            source.fleet_id.clone(),
            product_kind,
            MoleculeLink::DecayedFrom {
                id: request.source.clone(),
            },
        );
        store.save_molecule(&nuc.id, &product).map_err(store_err)?;
        product_ids.push(nuc.id);
    }

    if request.chain {
        wire_chain(store, &product_ids).map_err(store_err)?;
    }
    wire_blocks(store, &product_ids, request.blocks).map_err(store_err)?;

    source.status = MoleculeStatus::Completed;
    source.updated_at = Utc::now();
    for pid in &product_ids {
        source
            .typed_links
            .push(MoleculeLink::DecayProduct { id: pid.clone() });
    }
    store
        .save_molecule(request.source, &source)
        .map_err(store_err)?;

    log_interaction(
        state_dir,
        &Interaction::Decay {
            source: request.source.clone(),
            products: product_ids.clone(),
            reason: request.reason.to_owned(),
            timestamp: Utc::now(),
        },
    )
    .map_err(|e| DecayError::StoreUnavailable(e.to_string()))?;
    let events_path = state_dir.join("events.jsonl");
    effects.append_legacy_event(
        &events_path,
        &Envelope::now(Event::MoleculeDecayed {
            molecule_id: request.source.clone(),
            products: product_ids.clone(),
            reason: request.reason.to_owned(),
        }),
    );
    let _ = event_log::emit_one(
        events_path,
        EventV2::DecaySpliced {
            parent: request.source.clone(),
            children: product_ids.clone(),
        },
        None,
    );

    Ok(DecayView {
        source: request.source.clone(),
        products: product_ids,
        product_kind,
        reason: request.reason.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// merge
// ---------------------------------------------------------------------------

/// Inputs for [`merge`].
#[derive(Debug, Clone)]
pub struct MergeRequest<'a> {
    /// The molecules to merge; at least two.
    pub sources: &'a [MoleculeId],
    /// Formula the synthesis molecule is nucleated from.
    pub formula: &'a str,
    /// Kind of the synthesis molecule.
    pub product_kind: &'a str,
    /// Why the sources merge.
    pub reason: &'a str,
}

/// Result of a successful [`merge`].
#[derive(Debug, Clone)]
pub struct MergeView {
    /// The sources (now completed).
    pub sources: Vec<MoleculeId>,
    /// The synthesis molecule.
    pub product: MoleculeId,
    /// Its kind.
    pub product_kind: MoleculeKind,
    /// Why.
    pub reason: String,
}

/// Wire shape of a [`MergeView`]: what `cs --json merge` prints and the
/// route returns.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MergeJson {
    /// Always `"merge"`.
    pub interaction: &'static str,
    /// The sources.
    pub sources: Vec<String>,
    /// The synthesis molecule.
    pub product: String,
    /// Its kind.
    pub product_kind: String,
    /// Why.
    pub reason: String,
}

impl MergeJson {
    /// Project a view onto its wire shape.
    #[must_use]
    pub fn from_view(view: &MergeView) -> Self {
        Self {
            interaction: "merge",
            sources: ids(&view.sources).into_iter().map(str::to_owned).collect(),
            product: view.product.as_str().to_owned(),
            product_kind: view.product_kind.to_string(),
            reason: view.reason.clone(),
        }
    }
}

/// Merge N molecules into one synthesis molecule (`cs merge`).
///
/// Phase one loads and checks **every** source; phase two writes. A source
/// that cannot be loaded from `store` refuses the call, and nothing has been
/// written by then. Callers pass a store scoped to what the caller may
/// reach, which is what makes "a source of another tenant" the same refusal
/// as "a source that does not exist".
///
/// # Errors
///
/// A [`MergeError`]; the store is untouched for every variant except
/// [`MergeError::StoreUnavailable`] raised mid-write.
pub fn merge(
    store: &dyn StateStore,
    state_dir: &Path,
    formulas_dir: &Path,
    request: &MergeRequest<'_>,
    effects: &dyn InteractionEffects,
) -> Result<MergeView, MergeError> {
    if request.sources.len() < 2 {
        return Err(MergeError::TooFewSources);
    }

    // Phase one: read and check all of it.
    let mut sources: Vec<MoleculeData> = Vec::with_capacity(request.sources.len());
    for sid in request.sources {
        let mol = load(store, sid).map_err(|e| match e {
            Loaded::Absent => MergeError::SourceNotFound(sid.clone()),
            Loaded::Unreadable(m) => MergeError::StoreUnavailable(m),
        })?;
        let kind = mol.kind.unwrap_or(MoleculeKind::Task);
        if !kind.can_merge() {
            return Err(MergeError::SourceNotMergeable {
                id: sid.clone(),
                kind,
            });
        }
        sources.push(mol);
    }
    let product_kind: MoleculeKind = request
        .product_kind
        .parse()
        .map_err(|e| MergeError::InvalidKind(format!("{e}")))?;
    let formula = read_formula(formulas_dir, request.formula).map_err(|e| match e {
        FormulaFailure::Missing => MergeError::FormulaNotFound(request.formula.to_owned()),
        FormulaFailure::Unusable(m) => MergeError::FormulaParse(m),
    })?;
    let mut merged_vars: HashMap<String, String> = HashMap::new();
    for src in &sources {
        merged_vars.extend(src.variables.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    let nuc = core_nucleate(
        NucleateRequest {
            formula: &formula,
            variables: merged_vars,
            assign: sources[0].assigned_worker.clone(),
        },
        &mut rand::thread_rng(),
    )
    .map_err(|e| MergeError::NucleateFailed(e.to_string()))?;

    // Phase two: write.
    let store_err = |e: CosmonError| MergeError::StoreUnavailable(e.to_string());
    let product = product_molecule(
        &nuc,
        sources[0].fleet_id.clone(),
        product_kind,
        MoleculeLink::MergedFrom {
            ids: request.sources.to_vec(),
        },
    );
    store.save_molecule(&nuc.id, &product).map_err(store_err)?;
    for (sid, mut src) in request.sources.iter().zip(sources) {
        src.status = MoleculeStatus::Completed;
        src.updated_at = Utc::now();
        src.typed_links
            .push(MoleculeLink::MergedInto { id: nuc.id.clone() });
        store.save_molecule(sid, &src).map_err(store_err)?;
    }

    log_interaction(
        state_dir,
        &Interaction::Merge {
            sources: request.sources.to_vec(),
            product: nuc.id.clone(),
            reason: request.reason.to_owned(),
            timestamp: Utc::now(),
        },
    )
    .map_err(|e| MergeError::StoreUnavailable(e.to_string()))?;
    effects.append_legacy_event(
        &state_dir.join("events.jsonl"),
        &Envelope::now(Event::MoleculeMerged {
            sources: request.sources.to_vec(),
            product: nuc.id.clone(),
            reason: request.reason.to_owned(),
        }),
    );

    Ok(MergeView {
        sources: request.sources.to_vec(),
        product: nuc.id,
        product_kind,
        reason: request.reason.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// transform
// ---------------------------------------------------------------------------

/// Inputs for [`transform`].
#[derive(Debug, Clone)]
pub struct TransformRequest<'a> {
    /// The molecule whose kind changes.
    pub molecule: &'a MoleculeId,
    /// The target kind.
    pub to: &'a str,
    /// Why.
    pub reason: &'a str,
}

/// Result of a successful [`transform`].
#[derive(Debug, Clone)]
pub struct TransformView {
    /// The molecule.
    pub molecule: MoleculeId,
    /// Its kind before.
    pub from: MoleculeKind,
    /// Its kind now.
    pub to: MoleculeKind,
    /// Why.
    pub reason: String,
}

/// Wire shape of a [`TransformView`]: what `cs --json transform` prints and
/// the route returns.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TransformJson {
    /// Always `"transform"`.
    pub interaction: &'static str,
    /// The molecule.
    pub molecule: String,
    /// Its kind before.
    pub from: String,
    /// Its kind now.
    pub to: String,
    /// Why.
    pub reason: String,
}

impl TransformJson {
    /// Project a view onto its wire shape.
    #[must_use]
    pub fn from_view(view: &TransformView) -> Self {
        Self {
            interaction: "transform",
            molecule: view.molecule.as_str().to_owned(),
            from: view.from.to_string(),
            to: view.to.to_string(),
            reason: view.reason.clone(),
        }
    }
}

/// Change a molecule's kind along an edge of the kind graph (`cs transform`).
///
/// # Errors
///
/// A [`TransformError`]; the store is untouched for every variant except
/// [`TransformError::StoreUnavailable`] raised mid-write.
pub fn transform(
    store: &dyn StateStore,
    state_dir: &Path,
    request: &TransformRequest<'_>,
    effects: &dyn InteractionEffects,
) -> Result<TransformView, TransformError> {
    let mut mol = load(store, request.molecule).map_err(|e| match e {
        Loaded::Absent => TransformError::MoleculeNotFound(request.molecule.clone()),
        Loaded::Unreadable(m) => TransformError::StoreUnavailable(m),
    })?;
    let from = mol.kind.unwrap_or(MoleculeKind::Task);
    let to: MoleculeKind = request
        .to
        .parse()
        .map_err(|e| TransformError::InvalidKind(format!("{e}")))?;
    if !from.can_transform_to(to) {
        return Err(TransformError::NotTransformable {
            from,
            to,
            valid: from.valid_transforms().to_vec(),
        });
    }

    mol.kind = Some(to);
    mol.updated_at = Utc::now();
    mol.typed_links
        .push(MoleculeLink::TransformedFrom { kind: from });
    store
        .save_molecule(request.molecule, &mol)
        .map_err(|e| TransformError::StoreUnavailable(e.to_string()))?;

    log_interaction(
        state_dir,
        &Interaction::Transform {
            molecule: request.molecule.clone(),
            from,
            to,
            reason: request.reason.to_owned(),
            timestamp: Utc::now(),
        },
    )
    .map_err(|e| TransformError::StoreUnavailable(e.to_string()))?;
    effects.append_legacy_event(
        &state_dir.join("events.jsonl"),
        &Envelope::now(Event::MoleculeTransformed {
            molecule_id: request.molecule.clone(),
            from_kind: from,
            to_kind: to,
            reason: request.reason.to_owned(),
        }),
    );

    Ok(TransformView {
        molecule: request.molecule.clone(),
        from,
        to,
        reason: request.reason.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::error::is_kebab_case;

    /// Every tag these errors put on the wire is kebab-case.
    #[test]
    fn error_tags_are_kebab_case() {
        let id = MoleculeId::new("task-20261028-aaaa").unwrap();
        let tags = [
            DecayError::HomogeneousCount {
                parent: id.clone(),
                count: 2,
            }
            .tag(),
            DecayError::CannotDecay(MoleculeKind::Task).tag(),
            DecayError::BlockTargetNotFound(id.clone()).tag(),
            MergeError::TooFewSources.tag(),
            MergeError::SourceNotMergeable {
                id,
                kind: MoleculeKind::Decision,
            }
            .tag(),
            TransformError::InvalidKind("x".into()).tag(),
            TransformError::NotTransformable {
                from: MoleculeKind::Task,
                to: MoleculeKind::Idea,
                valid: Vec::new(),
            }
            .tag(),
        ];
        for tag in tags {
            assert!(is_kebab_case(tag), "{tag} is not kebab-case");
        }
    }

    /// The count rule: one product is fine, two would be identical.
    #[test]
    fn decay_refuses_more_than_one_product() {
        let id = MoleculeId::new("idea-20261028-aaaa").unwrap();
        assert!(ensure_decay_count_is_heterogeneous(&id, 1).is_ok());
        assert!(matches!(
            ensure_decay_count_is_heterogeneous(&id, 2),
            Err(DecayError::HomogeneousCount { count: 2, .. })
        ));
    }
}
