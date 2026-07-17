// SPDX-License-Identifier: AGPL-3.0-only

//! `tag` — add or remove typed labels on a molecule.
//!
//! Second verb extracted under `cosmon_state::ops`. The `cs tag <id>`
//! CLI handler and the cs-api `POST /molecules/{id}/tag` route both
//! call [`tag`] instead of shelling out — the first **mutant** verb
//! promoted under the library-first pattern.
//!
//! `tag` is the smallest pure-state mutation in the cosmon vocabulary:
//! load → mutate one `BTreeSet<Tag>` field → save. No worker dispatch,
//! no DAG traversal, no side-effects on disk other than the `state.json`
//! rewrite. That makes it the right test of the "library-first for
//! writers" hypothesis: if a single in-process function can replace the
//! shell-out without creating a second writer in concurrence with cs-cli,
//! the pattern generalises.
//!
//! # Single-writer hazard
//!
//! `state.json` is written via `crate::FileStore::save_molecule` which
//! uses the canonical tempfile + rename pattern (`atomic_write`). On
//! POSIX, `rename` is atomic at the filesystem level — readers always see
//! either the old or the new file, never a torn one. Two concurrent
//! `tag` calls from the **same** process serialize through the
//! filesystem rename, so the last writer wins and the JSON stays valid.
//! The cross-process case (cs-cli subprocess vs. in-process cs-api call
//! against the same state dir) is exercised by the integration test
//! `single_writer_in_process_vs_subprocess` in
//! `crates/cosmon-state/tests/tag_concurrency.rs`.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use cosmon_core::id::MoleculeId;
use cosmon_core::kind::MoleculeKind;
use cosmon_core::tag::Tag;

use crate::instrumentation::{emit_authz_decision, AuthzDecision};
use crate::ops::error::OpsError;
use crate::StateStore;
use cosmon_core::error::CosmonError;

/// Errors returned by [`tag`].
///
/// Mirrors the per-verb pattern set by
/// [`super::observe::ObserveError`]: dedicated enum, no flattening
/// through a mega-`CosmonError`, every variant maps to a stable
/// kebab-case [`OpsError::tag`] and an HTTP status. The `EmptyRequest`
/// variant captures the "nothing to do" misuse that previously lived in
/// both the cs-cli and cs-api handlers — moving it here gives both
/// callers a single place to surface the error.
#[derive(Debug, thiserror::Error)]
pub enum TagError {
    /// No molecule exists with the given ID.
    #[error("molecule not found: {0}")]
    MoleculeNotFound(MoleculeId),
    /// Caller did not supply any tag to add or remove.
    #[error("tag request must include at least one `add` or `remove`")]
    EmptyRequest,
    /// A runtime authority reservation cannot be removed through the normal
    /// mutation surface. These tags are monotone for the molecule lifetime.
    #[error("protected runtime reservation cannot be removed: {0}")]
    ProtectedReservation(Tag),
    /// An operator-only decision opt-in cannot be granted through the normal
    /// worker-reachable tag mutation surface.
    #[error("protected runtime decision opt-in cannot be added: {0}")]
    ProtectedDecisionOptIn(Tag),
    /// The state store could not be read or written (I/O failure, lock
    /// contention, schema mismatch). Maps to HTTP 503 — the request is
    /// well-formed, the substrate is just unavailable right now.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl TagError {
    /// Translate a [`CosmonError`] into the verb-specific error variant
    /// while keeping the rendered message stable.
    fn from_cosmon(id: &MoleculeId, err: CosmonError) -> Self {
        match err {
            CosmonError::MoleculeNotFound(_) => Self::MoleculeNotFound(id.clone()),
            other => Self::StoreUnavailable(other.to_string()),
        }
    }
}

impl OpsError for TagError {
    fn tag(&self) -> &'static str {
        match self {
            Self::MoleculeNotFound(_) => "molecule-not-found",
            Self::EmptyRequest => "empty-tag-request",
            Self::ProtectedReservation(_) => "protected-runtime-reservation",
            Self::ProtectedDecisionOptIn(_) => "protected-runtime-decision-opt-in",
            Self::StoreUnavailable(_) => "store-unavailable",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::MoleculeNotFound(_) => 404,
            Self::EmptyRequest => 400,
            Self::ProtectedReservation(_) | Self::ProtectedDecisionOptIn(_) => 403,
            Self::StoreUnavailable(_) => 503,
        }
    }
}

/// Result of a successful [`tag`] call.
///
/// Carries the requested adds / removes verbatim (so callers can echo
/// the user's intent), plus the final tag set and the before / after
/// counts. Wire renderers turn this into the JSON shape consumed by
/// `cs tag --json` and `POST /molecules/{id}/tag`.
#[derive(Debug, Clone)]
pub struct TagDelta {
    /// Molecule that was retagged.
    pub id: MoleculeId,
    /// Number of tags before the mutation.
    pub before_count: usize,
    /// Number of tags after the mutation.
    pub after_count: usize,
    /// Tags the caller asked to add — surfaced verbatim, even if some
    /// were already present (idempotent semantics).
    pub requested_add: Vec<Tag>,
    /// Tags the caller asked to remove — surfaced verbatim, even if some
    /// were not present.
    pub requested_remove: Vec<Tag>,
    /// Final tag set after mutation, in canonical sorted order.
    pub final_tags: BTreeSet<Tag>,
}

/// Add and/or remove tags on a molecule.
///
/// Idempotent: adding a tag that is already present is a no-op, and
/// removing a tag that is not present is a no-op.
///
/// `subject_kind` is the V0 placeholder for the future
/// `Subject` type that T-SUBJECT will introduce. V0 vocabulary:
/// `"operator"` for the trusted CLI subject, `"jwt:<sub>"` once
/// T-RPP-V0 wires JWT-bearing callers, `"absent"` when no
/// authentication context is available. The value flows through to
/// the [`AuthzDecisionEvaluated`](crate::instrumentation::AuthzDecisionEvaluated)
/// emitted at every tag.
///
/// # Errors
///
/// Returns [`TagError::EmptyRequest`] when both `add` and `remove` are
/// empty, [`TagError::MoleculeNotFound`] when no molecule has the given
/// ID in the store, and [`TagError::StoreUnavailable`] for any other
/// I/O failure surfaced by the underlying [`StateStore`].
///
/// # Atomicity
///
/// Calls [`StateStore::save_molecule`], which the file-backed adapter
/// implements via the canonical tempfile + rename pattern
/// (`atomic_write` in `cosmon-filestore`). The persisted `state.json` is
/// either the pre-call snapshot or the post-mutation snapshot — never a
/// torn file. Concurrent in-process callers serialize through the rename;
/// see module docs for the cross-process story.
///
/// # `#[verb]` registration
///
/// The annotation registers `tag` in the `cs-thin` verb registry as
/// `POST /v1/molecules/:id/tags` (principal `tenant`), so that the
/// mechanical HTTP client surfaces this verb in `cs-thin verbs --check`
/// and the `api_surface_freeze` test can assert the §8p subset matches
/// the rpp-adapter routes byte-for-byte. The macro emits a `TagVerb`
/// marker — the function itself is unchanged.
#[cosmon_thin_macro::verb(method = "POST", path = "/v1/molecules/:id/tags", principal = "tenant")]
pub fn tag(
    store: &dyn StateStore,
    state_dir: &Path,
    subject_kind: &str,
    id: &MoleculeId,
    add: &[Tag],
    remove: &[Tag],
) -> Result<TagDelta, TagError> {
    let started = Instant::now();
    // V0 trivial check: operator subject implicitly granted; every
    // other subject yields `Absent` so the instrumentation surfaces
    // the missing rule. The future `check_authz` (T-RPP-V0) replaces
    // this match with a real scope-by-verb grid.
    let decision = if subject_kind == "operator" {
        AuthzDecision::Allow
    } else {
        AuthzDecision::Absent
    };
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    emit_authz_decision(state_dir, "tag", subject_kind, None, decision, latency_ms);

    if add.is_empty() && remove.is_empty() {
        return Err(TagError::EmptyRequest);
    }
    if let Some(tag) = remove.iter().find(|tag| is_runtime_reservation(tag)) {
        return Err(TagError::ProtectedReservation(tag.clone()));
    }

    let mut mol = store
        .load_molecule(id)
        .map_err(|e| TagError::from_cosmon(id, e))?;
    // Decisions are human-reserved by default. `auto:ok` is therefore not a
    // cosmetic scheduler preference: adding it crosses the same authority
    // boundary as removing `hold:human`. Do not let the worker constrained by
    // that boundary grant itself the exception.
    if mol.kind == Some(MoleculeKind::Decision) {
        if let Some(tag) = add.iter().find(|tag| tag.as_str() == "auto:ok") {
            return Err(TagError::ProtectedDecisionOptIn(tag.clone()));
        }
    }

    let before_count = mol.tags.len();
    for t in add {
        mol.tags.insert(t.clone());
    }
    for t in remove {
        mol.tags.remove(t);
    }
    let after_count = mol.tags.len();

    mol.updated_at = chrono::Utc::now();
    store
        .save_molecule(id, &mol)
        .map_err(|e| TagError::from_cosmon(id, e))?;

    Ok(TagDelta {
        id: id.clone(),
        before_count,
        after_count,
        requested_add: add.to_vec(),
        requested_remove: remove.to_vec(),
        final_tags: mol.tags,
    })
}

/// Runtime authority marks are deliberately monotone on a molecule. They are
/// removed only by terminal lifecycle teardown, never by `cs tag`, whose
/// caller may be the worker the mark constrains.
fn is_runtime_reservation(tag: &Tag) -> bool {
    matches!(
        tag.as_str(),
        "hold:human" | "needs-review" | "needs-review-cross-provider" | "security"
    ) || tag.as_str().starts_with("security:")
}

// ---------------------------------------------------------------------------
// Wire-format renderer — keeps the byte layout consumed by `cs tag --json`
// and `POST /molecules/{id}/tag` stable across cs-cli and cs-api.
// ---------------------------------------------------------------------------

/// JSON body emitted by `cs tag <id> --json` and `POST /molecules/{id}/tag`.
///
/// Stable wire format: external scripts that parse `cs tag --json` and
/// the iOS pilot that consumes `POST /molecules/{id}/tag` rely on these
/// field names. Matches the legacy cs-cli output byte-for-byte (modulo
/// JSON whitespace) so promotion to the library-first path is invisible
/// to downstream consumers.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TagJson {
    /// Molecule ID.
    pub id: String,
    /// Final tag set, sorted lexically.
    pub tags: Vec<String>,
    /// Tags the caller asked to add (verbatim).
    pub added: Vec<String>,
    /// Tags the caller asked to remove (verbatim).
    pub removed: Vec<String>,
    /// Signed difference `after - before`. Negative when the net effect
    /// is a removal, zero when the request was a no-op (idempotent).
    pub delta: i64,
}

impl TagJson {
    /// Render a [`TagDelta`] into the canonical JSON wire shape.
    #[must_use]
    pub fn from_delta(delta: &TagDelta) -> Self {
        Self {
            id: delta.id.to_string(),
            tags: delta
                .final_tags
                .iter()
                .map(|t| t.as_str().to_owned())
                .collect(),
            added: delta
                .requested_add
                .iter()
                .map(|t| t.as_str().to_owned())
                .collect(),
            removed: delta
                .requested_remove
                .iter()
                .map(|t| t.as_str().to_owned())
                .collect(),
            delta: i64::try_from(delta.after_count).unwrap_or(i64::MAX)
                - i64::try_from(delta.before_count).unwrap_or(i64::MAX),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use chrono::Utc;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId, WorkerId};
    use cosmon_core::molecule::MoleculeStatus;
    use tempfile::TempDir;

    use super::*;
    use crate::{MoleculeData, MoleculeFilter};

    /// Fake [`StateStore`] backed by an in-memory `HashMap`. Mirrors the
    /// same shape used in `observe::tests` so the two suites can be read
    /// side by side.
    #[derive(Default)]
    struct FakeStore {
        molecules: std::sync::Mutex<HashMap<MoleculeId, MoleculeData>>,
    }

    impl FakeStore {
        fn insert(&self, mol: MoleculeData) {
            self.molecules.lock().unwrap().insert(mol.id.clone(), mol);
        }
    }

    impl StateStore for FakeStore {
        fn load_fleet(&self) -> Result<crate::Fleet, CosmonError> {
            Ok(crate::Fleet::default())
        }
        fn save_fleet(&self, _fleet: &crate::Fleet) -> Result<(), CosmonError> {
            Ok(())
        }
        fn load_molecule(&self, id: &MoleculeId) -> Result<MoleculeData, CosmonError> {
            self.molecules
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or_else(|| CosmonError::MoleculeNotFound(id.clone()))
        }
        fn save_molecule(&self, id: &MoleculeId, data: &MoleculeData) -> Result<(), CosmonError> {
            self.molecules
                .lock()
                .unwrap()
                .insert(id.clone(), data.clone());
            Ok(())
        }
        fn list_molecules(
            &self,
            _filter: &MoleculeFilter,
        ) -> Result<Vec<MoleculeData>, CosmonError> {
            Ok(self.molecules.lock().unwrap().values().cloned().collect())
        }
    }

    fn make_molecule(id: &str) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Pending,
            variables: HashMap::new(),
            assigned_worker: Some(WorkerId::new("ruby").unwrap()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 1,
            current_step: 0,
            completed_steps: vec![],
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: BTreeSet::new(),
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    #[test]
    fn tag_adds_new_tags_idempotently() {
        let store = FakeStore::default();
        store.insert(make_molecule("task-20260503-aaaa"));
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260503-aaaa").unwrap();
        let hot = Tag::new("temp:hot").unwrap();

        // First add: tag inserts.
        let d = tag(
            &store,
            tmp.path(),
            "operator",
            &id,
            std::slice::from_ref(&hot),
            &[],
        )
        .unwrap();
        assert_eq!(d.before_count, 0);
        assert_eq!(d.after_count, 1);
        assert!(d.final_tags.contains(&hot));

        // Second add of the same tag: idempotent — count stays 1.
        let d = tag(
            &store,
            tmp.path(),
            "operator",
            &id,
            std::slice::from_ref(&hot),
            &[],
        )
        .unwrap();
        assert_eq!(d.before_count, 1);
        assert_eq!(d.after_count, 1);
    }

    #[test]
    fn tag_removes_existing_tag_idempotently() {
        let store = FakeStore::default();
        let mut mol = make_molecule("task-20260503-bbbb");
        mol.tags.insert(Tag::new("temp:warm").unwrap());
        store.insert(mol);
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260503-bbbb").unwrap();
        let warm = Tag::new("temp:warm").unwrap();

        let d = tag(
            &store,
            tmp.path(),
            "operator",
            &id,
            &[],
            std::slice::from_ref(&warm),
        )
        .unwrap();
        assert_eq!(d.before_count, 1);
        assert_eq!(d.after_count, 0);

        // Second remove: idempotent.
        let d = tag(&store, tmp.path(), "operator", &id, &[], &[warm]).unwrap();
        assert_eq!(d.before_count, 0);
        assert_eq!(d.after_count, 0);
    }

    #[test]
    fn tag_swaps_temp_label_in_one_call() {
        let store = FakeStore::default();
        let mut mol = make_molecule("task-20260503-cccc");
        mol.tags.insert(Tag::new("temp:warm").unwrap());
        store.insert(mol);
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260503-cccc").unwrap();
        let warm = Tag::new("temp:warm").unwrap();
        let hot = Tag::new("temp:hot").unwrap();

        let d = tag(
            &store,
            tmp.path(),
            "operator",
            &id,
            std::slice::from_ref(&hot),
            &[warm],
        )
        .unwrap();
        assert_eq!(d.before_count, 1);
        assert_eq!(d.after_count, 1);
        assert!(d.final_tags.contains(&hot));
        assert_eq!(d.final_tags.len(), 1);
    }

    #[test]
    fn tag_returns_not_found_for_unknown_id() {
        let store = FakeStore::default();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260503-zzzz").unwrap();
        let hot = Tag::new("temp:hot").unwrap();
        let err = tag(&store, tmp.path(), "operator", &id, &[hot], &[]).unwrap_err();
        assert!(matches!(err, TagError::MoleculeNotFound(_)));
    }

    #[test]
    fn tag_rejects_empty_request() {
        let store = FakeStore::default();
        store.insert(make_molecule("task-20260503-dddd"));
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260503-dddd").unwrap();
        let err = tag(&store, tmp.path(), "operator", &id, &[], &[]).unwrap_err();
        assert!(matches!(err, TagError::EmptyRequest));
    }

    #[test]
    fn runtime_reservations_are_not_strippable_by_tag_callers() {
        let store = FakeStore::default();
        let mut mol = make_molecule("task-20260503-reserved");
        let hold = Tag::new("hold:human").unwrap();
        let review = Tag::new("needs-review").unwrap();
        let security = Tag::new("security:high").unwrap();
        mol.tags.insert(hold.clone());
        mol.tags.insert(review.clone());
        mol.tags.insert(security.clone());
        store.insert(mol);
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260503-reserved").unwrap();

        for reservation in [&hold, &review, &security] {
            let err = tag(
                &store,
                tmp.path(),
                "operator",
                &id,
                &[],
                std::slice::from_ref(reservation),
            )
            .unwrap_err();
            assert!(matches!(err, TagError::ProtectedReservation(_)));
        }
        let persisted = store.load_molecule(&id).unwrap();
        assert!(persisted.tags.contains(&hold));
        assert!(persisted.tags.contains(&review));
        assert!(persisted.tags.contains(&security));
    }

    #[test]
    fn decision_auto_ok_cannot_be_self_granted_by_tag_callers() {
        let store = FakeStore::default();
        let mut mol = make_molecule("task-20260503-decision");
        mol.kind = Some(MoleculeKind::Decision);
        store.insert(mol);
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260503-decision").unwrap();
        let auto_ok = Tag::new("auto:ok").unwrap();

        let err = tag(&store, tmp.path(), "operator", &id, &[auto_ok], &[]).unwrap_err();
        assert!(matches!(err, TagError::ProtectedDecisionOptIn(_)));
        assert!(
            !store
                .load_molecule(&id)
                .unwrap()
                .tags
                .contains(&Tag::new("auto:ok").unwrap()),
            "a worker-reachable tag call must not unreserve a decision"
        );
    }

    #[test]
    fn tag_error_tags_are_stable_kebab_case() {
        use crate::ops::error::is_kebab_case;
        let nf = TagError::MoleculeNotFound(MoleculeId::new("task-20260503-aaaa").unwrap());
        let er = TagError::EmptyRequest;
        let pr = TagError::ProtectedReservation(Tag::new("hold:human").unwrap());
        let po = TagError::ProtectedDecisionOptIn(Tag::new("auto:ok").unwrap());
        let su = TagError::StoreUnavailable("fs gone".into());

        assert_eq!(nf.tag(), "molecule-not-found");
        assert_eq!(er.tag(), "empty-tag-request");
        assert_eq!(pr.tag(), "protected-runtime-reservation");
        assert_eq!(po.tag(), "protected-runtime-decision-opt-in");
        assert_eq!(su.tag(), "store-unavailable");
        assert!(is_kebab_case(nf.tag()));
        assert!(is_kebab_case(er.tag()));
        assert!(is_kebab_case(pr.tag()));
        assert!(is_kebab_case(po.tag()));
        assert!(is_kebab_case(su.tag()));
    }

    #[test]
    fn tag_error_http_status_mapping() {
        let nf = TagError::MoleculeNotFound(MoleculeId::new("task-20260503-aaaa").unwrap());
        let er = TagError::EmptyRequest;
        let pr = TagError::ProtectedReservation(Tag::new("hold:human").unwrap());
        let po = TagError::ProtectedDecisionOptIn(Tag::new("auto:ok").unwrap());
        let su = TagError::StoreUnavailable("fs gone".into());
        assert_eq!(nf.http_status(), 404);
        assert_eq!(er.http_status(), 400);
        assert_eq!(pr.http_status(), 403);
        assert_eq!(po.http_status(), 403);
        assert_eq!(su.http_status(), 503);
    }

    #[test]
    fn cross_provider_review_marker_is_non_strippable() {
        let tag = Tag::new("needs-review-cross-provider").unwrap();
        assert!(is_runtime_reservation(&tag));
    }

    #[test]
    fn tag_json_renders_canonical_shape() {
        let store = FakeStore::default();
        store.insert(make_molecule("task-20260503-eeee"));
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260503-eeee").unwrap();
        let hot = Tag::new("temp:hot").unwrap();

        let d = tag(
            &store,
            tmp.path(),
            "operator",
            &id,
            std::slice::from_ref(&hot),
            &[],
        )
        .unwrap();
        let json = TagJson::from_delta(&d);
        assert_eq!(json.id, "task-20260503-eeee");
        assert_eq!(json.tags, vec!["temp:hot"]);
        assert_eq!(json.added, vec!["temp:hot"]);
        assert!(json.removed.is_empty());
        assert_eq!(json.delta, 1);

        // Wire stability: the field names match the legacy cs-cli payload.
        let s = serde_json::to_string(&json).unwrap();
        assert!(s.contains("\"id\":\"task-20260503-eeee\""));
        assert!(s.contains("\"tags\":[\"temp:hot\"]"));
        assert!(s.contains("\"added\":[\"temp:hot\"]"));
        assert!(s.contains("\"removed\":[]"));
        assert!(s.contains("\"delta\":1"));
    }

    /// Tagging emits exactly one `AuthzDecisionEvaluated` to
    /// `{state_dir}/instrumentation/authz.jsonl` with `decision=Allow`
    /// for the trusted CLI subject. Same shape as the observe smoke
    /// added in T-AUTHZ-INSTR.
    #[test]
    fn tag_emits_authz_decision_for_operator() {
        use crate::instrumentation::{
            read_authz_ndjson, AuthzDecision, AUTHZ_NDJSON_RELATIVE_PATH,
        };

        let store = FakeStore::default();
        store.insert(make_molecule("task-20260503-ffff"));
        let tmp = TempDir::new().unwrap();
        std::env::remove_var("COSMON_AUTHZ_INSTRUMENTATION_PATH");

        let id = MoleculeId::new("task-20260503-ffff").unwrap();
        let hot = Tag::new("temp:hot").unwrap();
        let _ = tag(&store, tmp.path(), "operator", &id, &[hot], &[]).unwrap();

        let path = tmp.path().join(AUTHZ_NDJSON_RELATIVE_PATH);
        let events = read_authz_ndjson(&path).unwrap();
        assert_eq!(events.len(), 1, "expected one authz event, got {events:?}");
        let ev = &events[0];
        assert_eq!(ev.verb, "tag");
        assert_eq!(ev.subject_kind, "operator");
        assert_eq!(ev.decision, AuthzDecision::Allow);
    }

    /// Non-operator subjects yield `Absent` in V0. Same V0 grid as
    /// observe.
    #[test]
    fn tag_emits_absent_for_jwt_subject() {
        use crate::instrumentation::{
            read_authz_ndjson, AuthzDecision, AUTHZ_NDJSON_RELATIVE_PATH,
        };

        let store = FakeStore::default();
        store.insert(make_molecule("task-20260503-gggg"));
        let tmp = TempDir::new().unwrap();
        std::env::remove_var("COSMON_AUTHZ_INSTRUMENTATION_PATH");

        let id = MoleculeId::new("task-20260503-gggg").unwrap();
        let hot = Tag::new("temp:hot").unwrap();
        let _ = tag(&store, tmp.path(), "jwt:tenant_auditor", &id, &[hot], &[]).unwrap();

        let path = tmp.path().join(AUTHZ_NDJSON_RELATIVE_PATH);
        let events = read_authz_ndjson(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].subject_kind, "jwt:tenant_auditor");
        assert_eq!(events[0].decision, AuthzDecision::Absent);
    }
}
