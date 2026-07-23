// SPDX-License-Identifier: AGPL-3.0-only

//! Auto-wiring of the cross-provider committee on **critical** tasks with a
//! **dual, separate, conjunctive** witness admission (ADR-153, C4 of
//! `delib-20260711-c6c8`).
//!
//! # Where this sits
//!
//! C1 (ADR-150) shipped the directional routing policy that *produces* a partial
//! `Incarnation`. C2 (ADR-151, [`crate::criticality`]) shipped the monotone
//! criticality fold that says *how much assurance* a subject demands. C3
//! (ADR-152, [`crate::sor`]) shipped the pure budget-aware router that *chooses a
//! seat*. This module is C4: when a fleet or spore opts its work into
//! cross-provider review, it **proposes and wires**
//! the [`cross-provider-committee`](../../../.cosmon/formulas/cross-provider-committee.formula.toml)
//! formula — reusing `cmb-verify`, the `Refutes`/`RefutedBy` edges
//! ([`crate::interaction`]), and the conjunctive verdict-door — **without** the
//! old "generator family = galaxy default" fallback.
//!
//! # The one structural gap C4 closes: a SECOND witness axis
//!
//! ADR-147 (tier a, [`crate::provider_diversity`]) makes the committee
//! *provider-family* diverse: no two seats may resolve to the same
//! [`EndpointTuple`]. That is a real witness — a Claude auditing a Claude is
//! error-*correlated*, so it shares blind spots. But it is **not enough on its
//! own**: two *different* providers both handed the generator's confident prose
//! as "the mechanism," both told to *confirm*, are still an echo of the same
//! framing. Channel independence without *posture* independence is a costume.
//!
//! So roster admission here requires **two separate witnesses, joined
//! conjunctively** — a seat sits only if BOTH pass, and neither can be traded for
//! the other:
//!
//! 1. **Provider-family witness** ([`FamilyWitness`]) — the seat's resolved
//!    [`EndpointTuple`] is distinct from the generator's AND from every other
//!    admitted seat's. Reuses [`crate::provider_diversity::resolve_endpoint_tuple`]
//!    verbatim. Carries the same **§8b ceiling, made visible, never hidden**: the
//!    family label is *config-derived, not attested*, so a motivated proxy-costume
//!    (a seat whose `base_url` fronts a Claude behind an `openai` label) survives
//!    tier (a). [`FamilyWitness::proxy_costume_ceiling`] states that limit on the
//!    record; binding family to an attested token is tier (b), the ADR-147
//!    follow-on.
//!
//! 2. **Persona/role witness** ([`PersonaWitness`]) — the seat plays a *distinct
//!    role* (`role_id`) from the generator AND every other seat, AND carries a
//!    **versioned adversarial briefing contract that was really injected**
//!    ([`AdversarialBriefing::injected`]), AND ships a **falsification-attempt
//!    artefact** ([`PersonaWitness::falsification_artifact`]) — proof the refuter
//!    actually *tried to break* the fix, not merely read it. A briefing that is
//!    declared but not injected, or a seat with no falsification artefact, fails
//!    this witness even if its provider family is impeccably distinct.
//!
//! # The SOR may not bargain a witness (the load-bearing separation)
//!
//! C3's [`crate::sor::select`] ranks by an integer score over quality, headroom,
//! availability and cost. That score must **never** be able to seat a witness-
//! failed candidate, however cheap or fast it is. So admission runs *upstream* of
//! and *independent* from the router: [`plan_committee`] computes the admissible
//! roster first, and only [`RosterPlan::admissible_seat_ids`] are ever offered to
//! the SOR. A rejected witness is not a low score the router can outweigh — it is
//! a seat that is **not on the ballot**. [`sor_may_not_resurrect`] is the
//! executable statement of that invariant, exercised by the budget-blocked-seat
//! test: SOR refusing an admissible seat on budget is fine (a typed refusal); SOR
//! seating a witness-*rejected* one is structurally impossible because it never
//! enters the candidate list.
//!
//! # The decision rule: a conjunctive verdict-door, never a majority vote
//!
//! [`committee_verdict`] folds seat outcomes with the exact door the formula
//! encodes: **refuted** if ANY seat returns `refuted` OR any falsifier goes red;
//! **confirmed** ONLY if EVERY seat returns `confirmed` (and no falsifier is red);
//! **inconclusive** otherwise. One concrete red falsifier beats ten "looks fine" —
//! a vote would let a majority drown a single true refutation, the exact failure
//! the whole diversity invariant exists to prevent.
//!
//! # Zero I/O
//!
//! Like [`crate::criticality`] and [`crate::sor`], this module is pure. It decides
//! *who may sit* and *what the jury concluded*; the seam that nucleates the seat
//! molecules, injects the briefing, and folds the verdicts is the `cs` shell and
//! the committee formula.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::ProviderBiasConfig;
use crate::criticality::CriticalityLevel;
use crate::provider_diversity::{resolve_endpoint_tuple, EndpointTuple};

/// The [`AdversarialBriefing`] schema version, bumped whenever the *meaning* of
/// the adversarial contract changes so a seat is only admitted under a briefing
/// the current policy recognises.
pub const ADVERSARIAL_BRIEFING_VERSION: u32 = 1;

/// Basename of the **regeneration-stable** durable file that carries a
/// committee seat's adversarial posture contract.
///
/// # Why a separate file, not `briefing.md`
///
/// Witness (2) requires the seat's per-step briefing to *deliver* the
/// adversarial contract ([`AdversarialBriefing::injected`]). The natural place
/// to write it — inline in `briefing.md` under a `## Committee posture`
/// heading — is **clobbered on every step advance**: `cs evolve` regenerates
/// `briefing.md` wholesale from the formula step, dropping any injected
/// section (committee-20260723-c0a1, witness 2 = `BriefingNotInjected`). So the
/// contract lives here instead, in a file *no* regeneration touches, and the
/// regenerated `briefing.md` only carries a stable *pointer* to it
/// ([`committee_posture_reference`]). The contract therefore survives every
/// step advance, while the pointer is cheaply re-established each time.
pub const COMMITTEE_POSTURE_FILE: &str = "committee-posture.md";

/// Render the durable committee-posture document written once to
/// `MOLECULE_DIR/`[`COMMITTEE_POSTURE_FILE`] at injection time.
///
/// The header pins the contract's [`ADVERSARIAL_BRIEFING_VERSION`] and content
/// hash so an audit can confirm *which* contract a seat received; `body` is the
/// adversarial contract prose itself. This file is **never** rewritten by
/// `cs evolve`, so the hash it declares is the one the seat carries for its
/// whole life.
#[must_use]
pub fn render_committee_posture(version: u32, contract_hash: &str, body: &str) -> String {
    format!(
        "# Committee posture (adversarial contract)\n\n\
         <!-- This file is DURABLE and regeneration-stable. `cs evolve` does NOT\n\
              rewrite it; the per-step `briefing.md` only points here. Editing or\n\
              deleting it breaks the seat's persona witness. -->\n\n\
         - **contract-version:** {version}\n\
         - **contract-hash:** {contract_hash}\n\n\
         ---\n\n\
         {body}\n"
    )
}

/// The stable pointer stanza a regenerated per-step `briefing.md` carries so a
/// seat is always directed to its durable adversarial contract.
///
/// `cs evolve` re-appends this constant stanza after it regenerates
/// `briefing.md`, but only when [`COMMITTEE_POSTURE_FILE`] exists in the
/// molecule directory. Because the stanza is a constant and the contract lives
/// in the separate durable file, the delivery survives every step advance — the
/// exact hole (`BriefingNotInjected`) this closes.
#[must_use]
pub const fn committee_posture_reference() -> &'static str {
    "## Committee posture\n\n\
     This molecule is a **cross-provider committee seat**. Its adversarial \
     contract is authoritative and lives in the durable, regeneration-stable \
     file `committee-posture.md` in this molecule's directory. Read it now and \
     honour it: it is NOT reproduced inline here because `cs evolve` \
     regenerates this briefing on every step and would clobber an inline copy. \
     `committee-posture.md` is never regenerated — it is the contract you were \
     seated under.\n"
}

/// The role a seat plays on the committee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SeatRole {
    /// The seat that produced the diagnosis + fix + falsifier under audit. There
    /// is exactly one generator; it is not admitted, it is the thing refuters are
    /// diverse *against*.
    Generator,
    /// An adversarial refuter — a `cmb-verify` molecule carrying a typed
    /// `Refutes` edge, whose job is to try to make the falsifier (or a sharper
    /// one) go red.
    Refuter,
}

/// Witness (1): the provider-family axis. A resolved [`EndpointTuple`] plus the
/// honest statement of the ceiling it buys.
///
/// This is a thin, self-documenting wrapper over the ADR-147 tier-(a) resolution
/// so the persona witness sits beside it as a *peer*, not a sub-field: the two
/// witnesses are separate axes and the type reflects that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilyWitness {
    /// The resolved `(provider, base_url, model-family)` tuple — derived from
    /// `base_url` + `model`, never the adapter section name (ADR-147).
    pub endpoint: EndpointTuple,
}

impl FamilyWitness {
    /// Resolve a seat's family witness from the project `[adapters]` inventory,
    /// exactly as [`resolve_endpoint_tuple`] does.
    #[must_use]
    pub fn resolve(adapters: Option<&crate::config::AdaptersConfig>, seat: &str) -> Self {
        Self {
            endpoint: resolve_endpoint_tuple(adapters, seat),
        }
    }

    /// The §8b honesty line, on the record: the family label is *config-derived,
    /// not attested*, so a motivated proxy-costume survives tier (a). Binding
    /// family to an attested token is tier (b) (`SameFamilyRefusal`), the ADR-147
    /// follow-on. This makes the witness **visible and attributable, not
    /// incorruptible.**
    #[must_use]
    pub const fn proxy_costume_ceiling() -> &'static str {
        "tier-(a) family is derived from operator config (base_url + model), not an \
         attested token; a proxy-costume that fronts one family behind another \
         label survives this witness — binding family to an attested token is \
         tier (b) SameFamilyRefusal (ADR-147 follow-on)"
    }
}

/// The versioned adversarial briefing contract a refuter must carry — and must
/// have **really injected** into its own briefing, not merely declared.
///
/// The `injected` flag is the load-bearing field: an adversarial contract that
/// exists in policy but was never written into the seat's `briefing.md` is a
/// posture the refuter never actually received. C4 requires *evidence of
/// injection*, so a paper contract cannot pass the persona witness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdversarialBriefing {
    /// Contract schema version — must match [`ADVERSARIAL_BRIEFING_VERSION`] to
    /// be recognised by the current policy.
    pub version: u32,
    /// Content hash (`blake3:<hex>` or a stable label) of the injected contract
    /// text, so an audit can confirm *which* contract was delivered.
    pub contract_hash: String,
    /// Whether the contract was **actually injected** into the seat's briefing.
    /// `false` means "declared but not delivered" — the persona witness fails.
    pub injected: bool,
}

impl AdversarialBriefing {
    /// Build an [`AdversarialBriefing`] whose `injected` flag is derived from
    /// the **durable-file delivery** two-fact test, closing the
    /// `BriefingNotInjected` hole (committee-20260723-c0a1).
    ///
    /// The contract counts as delivered — and the persona witness passes — only
    /// when BOTH facts hold:
    ///
    /// - `posture_file_present`: the durable [`COMMITTEE_POSTURE_FILE`] exists
    ///   in the seat's molecule directory (the contract survives regeneration
    ///   because *this* file is never rewritten by `cs evolve`), and
    /// - `briefing_references_posture`: the seat's regenerated per-step
    ///   `briefing.md` carries the stable [`committee_posture_reference`]
    ///   pointer at it.
    ///
    /// An inline `## Committee posture` section written straight into
    /// `briefing.md` is *not* durable delivery — the next `cs evolve` clobbers
    /// it — so it can never satisfy this constructor.
    #[must_use]
    pub fn from_durable_injection(
        version: u32,
        contract_hash: impl Into<String>,
        posture_file_present: bool,
        briefing_references_posture: bool,
    ) -> Self {
        Self {
            version,
            contract_hash: contract_hash.into(),
            injected: posture_file_present && briefing_references_posture,
        }
    }

    /// Whether this briefing is a valid, current, *delivered* adversarial
    /// contract: recognised version, non-empty hash, and really injected.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.version == ADVERSARIAL_BRIEFING_VERSION
            && !self.contract_hash.trim().is_empty()
            && self.injected
    }
}

/// Witness (2): the persona/role axis — a distinct role, a delivered adversarial
/// contract, and proof a falsification was attempted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaWitness {
    /// Stable persona/role identity. Two seats sharing a `role_id` are the same
    /// posture wearing two provider hats — the persona witness rejects the
    /// second (the same-persona-refuter failure).
    pub role_id: String,
    /// The versioned adversarial briefing contract, present and injected. `None`
    /// means no contract was carried — the witness fails.
    pub briefing: Option<AdversarialBriefing>,
    /// Path/locus of the **falsification-attempt artefact** the refuter produced
    /// (e.g. `MOLECULE_DIR/falsification-attempt.md`). `None` means the refuter
    /// shipped no evidence it tried to break the fix — the witness fails.
    pub falsification_artifact: Option<String>,
}

impl PersonaWitness {
    /// Whether the persona witness *itself* is complete — a valid injected
    /// briefing and a falsification artefact. Role-distinctness is a *pairwise*
    /// property checked by [`plan_committee`], not by this per-seat predicate.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.briefing
            .as_ref()
            .is_some_and(AdversarialBriefing::is_valid)
            && self
                .falsification_artifact
                .as_ref()
                .is_some_and(|a| !a.trim().is_empty())
    }
}

/// A candidate seat before dual-witness admission: an identity, a role, and the
/// two witness bundles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatCandidate {
    /// Stable seat identity — a molecule id, or a planned seat label at convene
    /// time before nucleation.
    pub seat_id: String,
    /// The role this seat plays.
    pub role: SeatRole,
    /// Provider-family witness (axis 1).
    pub family: FamilyWitness,
    /// Persona/role witness (axis 2).
    pub persona: PersonaWitness,
}

/// The typed reason a seat failed dual-witness admission. Every rejection is
/// recorded; none is silent, and the two witness axes reject with *distinct*
/// reasons so an audit sees *which* independence a seat lacked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SeatRejection {
    /// Witness 1 failed: this seat's resolved endpoint tuple equals the
    /// generator's or another seat's — the same-family / proxy-costume collapse.
    FamilyCollision {
        /// The tuple both seats resolved to.
        endpoint: EndpointTuple,
        /// The other seat id sharing the tuple (the generator, or a peer).
        collides_with: String,
    },
    /// Witness 2 failed: this seat shares a `role_id` with the generator or
    /// another seat — the same-persona refuter.
    PersonaCollision {
        /// The shared role id.
        role_id: String,
        /// The other seat sharing the role.
        collides_with: String,
    },
    /// Witness 2 failed: the adversarial briefing contract is absent, the wrong
    /// version, or **declared but not injected**.
    BriefingNotInjected,
    /// Witness 2 failed: the seat shipped no falsification-attempt artefact.
    FalsificationArtifactMissing,
}

impl SeatRejection {
    /// A stable, human-auditable label for the rejection reason.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::FamilyCollision { .. } => "family-collision",
            Self::PersonaCollision { .. } => "persona-collision",
            Self::BriefingNotInjected => "briefing-not-injected",
            Self::FalsificationArtifactMissing => "falsification-artifact-missing",
        }
    }

    /// Which witness axis this rejection belongs to (1 = provider-family, 2 =
    /// persona/role).
    #[must_use]
    pub const fn witness_axis(&self) -> u8 {
        match self {
            Self::FamilyCollision { .. } => 1,
            Self::PersonaCollision { .. }
            | Self::BriefingNotInjected
            | Self::FalsificationArtifactMissing => 2,
        }
    }
}

/// One admitted seat, kept beside its two resolved witnesses for the receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmittedSeat {
    /// The seat id.
    pub seat_id: String,
    /// The resolved family tuple that passed witness 1.
    pub endpoint: EndpointTuple,
    /// The role id that passed witness 2.
    pub role_id: String,
}

/// One rejected seat + its typed reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedSeat {
    /// The rejected seat's id.
    pub seat_id: String,
    /// Why it was rejected.
    pub reason: SeatRejection,
}

/// The committee requirement derived from an explicit review opt-in,
/// effective criticality, and the `[provider_bias]` floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitteeRequirement {
    /// Whether a fleet or spore explicitly demands a committee.
    pub required: bool,
    /// The floor on distinct provider families the jury must span, generator
    /// included. `root` → 2 (generator + ≥1 distinct refuter); `security`/`max` →
    /// 3. Raised (never lowered) by the config
    /// `min_distinct_provider_endpoints`.
    pub min_distinct_families: usize,
}

/// Derive the committee requirement for an explicitly reviewed task.
///
/// `cross_provider` is a fleet/spore policy input, never inferred from a task's
/// criticality. Once enabled, criticality determines the diversity floor and
/// `[provider_bias]` may only strengthen it.
#[must_use]
pub fn committee_requirement(
    level: CriticalityLevel,
    bias: &ProviderBiasConfig,
    cross_provider: bool,
) -> CommitteeRequirement {
    let stake_floor = match level {
        CriticalityLevel::Routine | CriticalityLevel::Root => 2,
        CriticalityLevel::Security | CriticalityLevel::Max => 3,
    };
    let config_floor = bias
        .effective()
        .min_distinct_provider_endpoints
        .map_or(0, |n| n as usize);
    CommitteeRequirement {
        required: cross_provider,
        min_distinct_families: if cross_provider {
            stake_floor.max(config_floor)
        } else {
            0
        },
    }
}

/// The result of planning a committee: the admissible roster, the rejects, the
/// requirement it was measured against, and whether the floor is met.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterPlan {
    /// The requirement this roster was measured against.
    pub requirement: CommitteeRequirement,
    /// The generator seat's resolved family + role (the axis everything is
    /// diverse *against*).
    pub generator: AdmittedSeat,
    /// Refuter seats that passed BOTH witnesses, in input order.
    pub admitted: Vec<AdmittedSeat>,
    /// Refuter seats that failed at least one witness, with typed reasons.
    pub rejected: Vec<RejectedSeat>,
    /// Whether the admitted roster (generator + admitted refuters) spans at least
    /// [`CommitteeRequirement::min_distinct_families`] distinct families. `false`
    /// is a **missing-seat** finding: the committee cannot be convened.
    pub floor_met: bool,
}

impl RosterPlan {
    /// The seat ids the SOR ([`crate::sor::select`]) may choose among — the
    /// admitted refuters only. A witness-rejected seat is **never** on this list,
    /// so no router score can resurrect it.
    #[must_use]
    pub fn admissible_seat_ids(&self) -> Vec<String> {
        self.admitted.iter().map(|s| s.seat_id.clone()).collect()
    }

    /// The distinct families the admitted roster spans (generator included).
    #[must_use]
    pub fn distinct_families(&self) -> usize {
        let mut fams: std::collections::BTreeSet<&str> =
            std::iter::once(self.generator.endpoint.family.as_str()).collect();
        fams.extend(self.admitted.iter().map(|s| s.endpoint.family.as_str()));
        fams.len()
    }
}

/// Plan a committee: admit each refuter under the **dual conjunctive witness**,
/// measured against the generator and the already-admitted peers, then check the
/// distinct-family floor.
///
/// A refuter is admitted iff BOTH witnesses pass:
///
/// - **Witness 1 (family):** its endpoint tuple differs from the generator's and
///   from every already-admitted refuter's.
/// - **Witness 2 (persona):** its `role_id` differs from the generator's and from
///   every already-admitted refuter's, its adversarial briefing is valid and
///   injected, and it ships a falsification artefact.
///
/// The check is order-stable: a candidate is compared against the generator and
/// the seats admitted *before* it, so the first of two colliding seats is
/// admitted and the second rejected — deterministic and independent of a global
/// pass. A seat that fails is recorded with the **first** witness axis that
/// rejected it (family before persona), never silently dropped.
#[must_use]
pub fn plan_committee(
    generator: &SeatCandidate,
    refuters: &[SeatCandidate],
    requirement: CommitteeRequirement,
) -> RosterPlan {
    let generator_admitted = AdmittedSeat {
        seat_id: generator.seat_id.clone(),
        endpoint: generator.family.endpoint.clone(),
        role_id: generator.persona.role_id.clone(),
    };

    let mut admitted: Vec<AdmittedSeat> = Vec::new();
    let mut rejected: Vec<RejectedSeat> = Vec::new();

    // Endpoint tuple → the seat that first claimed it (generator seeds the map).
    let mut seen_endpoints: BTreeMap<EndpointTuple, String> = BTreeMap::new();
    seen_endpoints.insert(generator.family.endpoint.clone(), generator.seat_id.clone());
    // role_id → the seat that first claimed it.
    let mut seen_roles: BTreeMap<String, String> = BTreeMap::new();
    seen_roles.insert(generator.persona.role_id.clone(), generator.seat_id.clone());

    for seat in refuters {
        // Witness 1 — provider-family distinctness (checked first so a same-family
        // costume is named as a family collision, the ADR-147 axis).
        if let Some(other) = seen_endpoints.get(&seat.family.endpoint) {
            rejected.push(RejectedSeat {
                seat_id: seat.seat_id.clone(),
                reason: SeatRejection::FamilyCollision {
                    endpoint: seat.family.endpoint.clone(),
                    collides_with: other.clone(),
                },
            });
            continue;
        }

        // Witness 2 — persona/role. Role-distinctness first, then contract
        // delivery, then falsification evidence.
        if let Some(other) = seen_roles.get(&seat.persona.role_id) {
            rejected.push(RejectedSeat {
                seat_id: seat.seat_id.clone(),
                reason: SeatRejection::PersonaCollision {
                    role_id: seat.persona.role_id.clone(),
                    collides_with: other.clone(),
                },
            });
            continue;
        }
        if seat.persona.briefing.as_ref().is_none_or(|b| !b.is_valid()) {
            rejected.push(RejectedSeat {
                seat_id: seat.seat_id.clone(),
                reason: SeatRejection::BriefingNotInjected,
            });
            continue;
        }
        if seat
            .persona
            .falsification_artifact
            .as_ref()
            .is_none_or(|a| a.trim().is_empty())
        {
            rejected.push(RejectedSeat {
                seat_id: seat.seat_id.clone(),
                reason: SeatRejection::FalsificationArtifactMissing,
            });
            continue;
        }

        // Both witnesses pass — seat it, and record its tuple + role so later
        // seats are measured against it too.
        seen_endpoints.insert(seat.family.endpoint.clone(), seat.seat_id.clone());
        seen_roles.insert(seat.persona.role_id.clone(), seat.seat_id.clone());
        admitted.push(AdmittedSeat {
            seat_id: seat.seat_id.clone(),
            endpoint: seat.family.endpoint.clone(),
            role_id: seat.persona.role_id.clone(),
        });
    }

    let mut plan = RosterPlan {
        requirement,
        generator: generator_admitted,
        admitted,
        rejected,
        floor_met: false,
    };
    plan.floor_met =
        !requirement.required || plan.distinct_families() >= requirement.min_distinct_families;
    plan
}

/// Whether the SOR may seat `candidate_seat_id` — `true` only when it is on the
/// admissible list. The executable statement of *"the SOR chooses only among
/// admissible seats and cannot bargain a witness"*: a witness-rejected seat is
/// not a low score to outweigh, it is simply absent from the ballot.
#[must_use]
pub fn sor_may_not_resurrect(plan: &RosterPlan, candidate_seat_id: &str) -> bool {
    plan.admitted.iter().any(|s| s.seat_id == candidate_seat_id)
}

/// A single seat's returned verdict plus whether its falsifier went red under a
/// refuter's hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SeatVerdict {
    /// The seat could not break the fix and the falsifier held.
    Confirmed,
    /// The seat refuted the diagnosis or made a falsifier go red.
    Refuted,
    /// The seat reached no decisive verdict.
    Inconclusive,
}

/// One seat's outcome carried into the conjunctive door.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatOutcome {
    /// The seat that produced this outcome.
    pub seat_id: String,
    /// The verdict the seat returned.
    pub verdict: SeatVerdict,
    /// Whether a falsifier went red under this seat — a concrete red beats any
    /// amount of "looks fine."
    pub falsifier_red: bool,
}

/// The committee's aggregate verdict — the conjunctive door.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommitteeVerdict {
    /// Every seat confirmed and no falsifier went red.
    Confirmed,
    /// At least one seat refuted, or at least one falsifier went red.
    Refuted,
    /// Neither unanimous confirmation nor any refutation — e.g. some seats
    /// inconclusive, or no seat reported at all.
    Inconclusive,
}

/// Fold seat outcomes into the committee verdict with the **conjunctive
/// verdict-door**:
///
/// - **`Refuted`** if ANY seat returned [`SeatVerdict::Refuted`] OR any
///   `falsifier_red` is set — one concrete red falsifier is decisive.
/// - **`Confirmed`** ONLY if there is at least one seat and EVERY seat returned
///   [`SeatVerdict::Confirmed`] with no falsifier red.
/// - **`Inconclusive`** otherwise (some seat inconclusive, or the outcome set is
///   empty — an empty jury cannot confirm anything).
///
/// This is a door, not a vote: a lone refutation among a hundred confirmations
/// still refutes. A majority vote would let a mono-posture crowd drown a single
/// true refuter — the exact failure the dual-witness admission exists to prevent.
#[must_use]
pub fn committee_verdict(outcomes: &[SeatOutcome]) -> CommitteeVerdict {
    if outcomes
        .iter()
        .any(|o| o.verdict == SeatVerdict::Refuted || o.falsifier_red)
    {
        return CommitteeVerdict::Refuted;
    }
    if !outcomes.is_empty()
        && outcomes
            .iter()
            .all(|o| o.verdict == SeatVerdict::Confirmed && !o.falsifier_red)
    {
        return CommitteeVerdict::Confirmed;
    }
    CommitteeVerdict::Inconclusive
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderRequirementSet;

    fn endpoint(provider: &str, family: &str) -> EndpointTuple {
        EndpointTuple {
            provider: provider.into(),
            base_url: String::new(),
            family: family.into(),
        }
    }

    fn briefing() -> AdversarialBriefing {
        AdversarialBriefing {
            version: ADVERSARIAL_BRIEFING_VERSION,
            contract_hash: "blake3:deadbeef".into(),
            injected: true,
        }
    }

    fn seat(id: &str, role: SeatRole, family: &str, role_id: &str) -> SeatCandidate {
        SeatCandidate {
            seat_id: id.into(),
            role,
            family: FamilyWitness {
                endpoint: endpoint(family, family),
            },
            persona: PersonaWitness {
                role_id: role_id.into(),
                briefing: Some(briefing()),
                falsification_artifact: Some("falsification-attempt.md".into()),
            },
        }
    }

    fn root_req() -> CommitteeRequirement {
        CommitteeRequirement {
            required: true,
            min_distinct_families: 2,
        }
    }

    // ── committee_requirement ────────────────────────────────────────────

    #[test]
    fn requirement_is_opt_in_and_scales_with_stake() {
        let bias = ProviderBiasConfig::default();
        assert_eq!(
            committee_requirement(CriticalityLevel::Security, &bias, false),
            CommitteeRequirement {
                required: false,
                min_distinct_families: 0
            }
        );
        assert_eq!(
            committee_requirement(CriticalityLevel::Routine, &bias, true).min_distinct_families,
            2
        );
        assert_eq!(
            committee_requirement(CriticalityLevel::Root, &bias, true).min_distinct_families,
            2
        );
        assert_eq!(
            committee_requirement(CriticalityLevel::Security, &bias, true).min_distinct_families,
            3
        );
        assert_eq!(
            committee_requirement(CriticalityLevel::Max, &bias, true).min_distinct_families,
            3
        );
    }

    #[test]
    fn config_floor_raises_but_never_lowers_stake_floor() {
        let bias = ProviderBiasConfig {
            baseline: ProviderRequirementSet {
                min_distinct_provider_endpoints: Some(4),
                ..Default::default()
            },
            ..Default::default()
        };
        // Config floor 4 > root stake floor 2 → 4.
        assert_eq!(
            committee_requirement(CriticalityLevel::Root, &bias, true).min_distinct_families,
            4
        );
        // A config floor BELOW the stake floor cannot lower it: max stake is 3.
        let low = ProviderBiasConfig {
            baseline: ProviderRequirementSet {
                min_distinct_provider_endpoints: Some(1),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            committee_requirement(CriticalityLevel::Max, &low, true).min_distinct_families,
            3
        );
    }

    // ── dual-witness admission ───────────────────────────────────────────

    #[test]
    fn distinct_family_and_persona_seat_is_admitted() {
        let gen = seat("gen", SeatRole::Generator, "anthropic", "author");
        let ref1 = seat("ref1", SeatRole::Refuter, "openai", "skeptic");
        let plan = plan_committee(&gen, &[ref1], root_req());
        assert_eq!(plan.admitted.len(), 1);
        assert!(plan.rejected.is_empty());
        assert!(plan.floor_met);
        assert_eq!(plan.admissible_seat_ids(), vec!["ref1"]);
        assert_eq!(plan.distinct_families(), 2);
    }

    #[test]
    fn same_family_alias_fails_witness_one() {
        // A refuter whose resolved endpoint equals the generator's — the
        // proxy-costume / same-family alias.
        let gen = seat("gen", SeatRole::Generator, "anthropic", "author");
        let alias = seat("alias", SeatRole::Refuter, "anthropic", "skeptic");
        let plan = plan_committee(&gen, &[alias], root_req());
        assert!(plan.admitted.is_empty());
        assert_eq!(plan.rejected.len(), 1);
        assert_eq!(plan.rejected[0].reason.witness_axis(), 1);
        assert!(matches!(
            plan.rejected[0].reason,
            SeatRejection::FamilyCollision { .. }
        ));
        // Floor not met → missing-seat.
        assert!(!plan.floor_met);
    }

    #[test]
    fn same_persona_refuter_fails_witness_two() {
        // Distinct family, but the same posture as the generator — an echo of the
        // same framing wearing a different provider hat.
        let gen = seat("gen", SeatRole::Generator, "anthropic", "author");
        let same = seat("same", SeatRole::Refuter, "openai", "author");
        let plan = plan_committee(&gen, &[same], root_req());
        assert!(plan.admitted.is_empty());
        assert_eq!(plan.rejected[0].reason.witness_axis(), 2);
        assert!(matches!(
            plan.rejected[0].reason,
            SeatRejection::PersonaCollision { .. }
        ));
        assert!(!plan.floor_met);
    }

    #[test]
    fn two_refuters_sharing_a_persona_reject_the_second() {
        let gen = seat("gen", SeatRole::Generator, "anthropic", "author");
        let ref1 = seat("ref1", SeatRole::Refuter, "openai", "skeptic");
        let ref2 = seat("ref2", SeatRole::Refuter, "xai", "skeptic"); // dup persona
        let plan = plan_committee(&gen, &[ref1, ref2], root_req());
        assert_eq!(plan.admitted.len(), 1);
        assert_eq!(plan.admitted[0].seat_id, "ref1");
        assert_eq!(plan.rejected.len(), 1);
        assert!(matches!(
            &plan.rejected[0].reason,
            SeatRejection::PersonaCollision { collides_with, .. } if collides_with == "ref1"
        ));
    }

    #[test]
    fn briefing_declared_but_not_injected_fails() {
        let gen = seat("gen", SeatRole::Generator, "anthropic", "author");
        let mut paper = seat("paper", SeatRole::Refuter, "openai", "skeptic");
        paper.persona.briefing = Some(AdversarialBriefing {
            injected: false, // declared but not delivered
            ..briefing()
        });
        let plan = plan_committee(&gen, &[paper], root_req());
        assert!(plan.admitted.is_empty());
        assert_eq!(plan.rejected[0].reason, SeatRejection::BriefingNotInjected);
    }

    #[test]
    fn wrong_briefing_version_fails() {
        let gen = seat("gen", SeatRole::Generator, "anthropic", "author");
        let mut stale = seat("stale", SeatRole::Refuter, "openai", "skeptic");
        stale.persona.briefing = Some(AdversarialBriefing {
            version: ADVERSARIAL_BRIEFING_VERSION + 1,
            ..briefing()
        });
        let plan = plan_committee(&gen, &[stale], root_req());
        assert_eq!(plan.rejected[0].reason, SeatRejection::BriefingNotInjected);
    }

    #[test]
    fn missing_falsification_artifact_fails() {
        let gen = seat("gen", SeatRole::Generator, "anthropic", "author");
        let mut lazy = seat("lazy", SeatRole::Refuter, "openai", "skeptic");
        lazy.persona.falsification_artifact = None;
        let plan = plan_committee(&gen, &[lazy], root_req());
        assert_eq!(
            plan.rejected[0].reason,
            SeatRejection::FalsificationArtifactMissing
        );
    }

    #[test]
    fn missing_seat_when_floor_not_met() {
        // security stake wants 3 distinct families; only 2 are supplied.
        let req = CommitteeRequirement {
            required: true,
            min_distinct_families: 3,
        };
        let gen = seat("gen", SeatRole::Generator, "anthropic", "author");
        let ref1 = seat("ref1", SeatRole::Refuter, "openai", "skeptic");
        let plan = plan_committee(&gen, &[ref1], req);
        assert_eq!(plan.admitted.len(), 1);
        assert_eq!(plan.distinct_families(), 2);
        assert!(!plan.floor_met, "2 families cannot meet a floor of 3");
    }

    // ── SOR-may-not-bargain-a-witness ────────────────────────────────────

    #[test]
    fn sor_only_sees_admissible_seats_and_cannot_resurrect_a_rejected_one() {
        use crate::sor::{select, LocalConsumption, ScoreWeights, SorRequest};

        let gen = seat("gen", SeatRole::Generator, "anthropic", "author");
        // A witness-REJECTED seat (same persona) and an admitted one.
        let rejected = seat("rejected", SeatRole::Refuter, "openai", "author");
        let admitted = seat("admitted", SeatRole::Refuter, "xai", "skeptic");
        let plan = plan_committee(&gen, &[rejected.clone(), admitted], root_req());
        assert_eq!(plan.admissible_seat_ids(), vec!["admitted"]);
        // The rejected seat may not be resurrected, whatever a router might score.
        assert!(!sor_may_not_resurrect(&plan, "rejected"));
        assert!(sor_may_not_resurrect(&plan, "admitted"));

        // The SOR is only ever offered the admissible seats. Build a candidate
        // ONLY from the admissible id — the rejected one never enters the ballot.
        let cand = crate::sor::SorCandidate {
            adapter: "xai".into(),
            model: Some("grok-4".into()),
            effort: None,
            spawnable: true,
            honors_pin: true,
            capacity_ok: true,
            diversity_ok: true,
            consumption: LocalConsumption::Available(0),
            budget_cap: Some(10),
            calibration: crate::sor::CalibrationObs::new(
                Some("v1".into()),
                Some(800),
                Some(now() - chrono::Duration::minutes(1)),
                chrono::Duration::hours(24),
                now(),
            ),
            availability: crate::sor::Observation::observed(
                Some(900),
                crate::sor::ObservationSource::Probe,
                Some(now() - chrono::Duration::minutes(1)),
                chrono::Duration::hours(1),
                now(),
            ),
            cost: crate::sor::Observation::observed(
                Some(100),
                crate::sor::ObservationSource::PriceTable,
                Some(now() - chrono::Duration::minutes(1)),
                chrono::Duration::hours(1),
                now(),
            ),
        };
        let req = SorRequest {
            subject: "committee-1".into(),
            revision: "rev-1".into(),
            criticality: CriticalityLevel::Root,
            criticality_actors: vec!["policy".into()],
            policy_digest: "policy:committee".into(),
            attempt: 1,
            supersedes: None,
        };
        let dec = select(&req, &[cand], &ScoreWeights::default(), now()).unwrap();
        assert_eq!(dec.chosen.adapter, "xai");
    }

    #[test]
    fn budget_blocked_admissible_seat_is_a_typed_sor_refusal_not_a_witness_bypass() {
        // A seat that passes BOTH witnesses but is budget-exhausted: the SOR
        // refuses it with a typed reason. Crucially, the router does NOT fall back
        // to a witness-rejected seat — the refusal is honest, the witness holds.
        use crate::sor::{select, LocalConsumption, ScoreWeights, SorRefusal, SorRequest};

        let gen = seat("gen", SeatRole::Generator, "anthropic", "author");
        let admitted = seat("admitted", SeatRole::Refuter, "openai", "skeptic");
        let plan = plan_committee(&gen, &[admitted], root_req());
        assert_eq!(plan.admissible_seat_ids(), vec!["admitted"]);

        // The single admissible seat is budget-exhausted (cap 10, consumed 10).
        let cand = crate::sor::SorCandidate {
            adapter: "openai".into(),
            model: Some("gpt-5".into()),
            effort: None,
            spawnable: true,
            honors_pin: true,
            capacity_ok: true,
            diversity_ok: true,
            consumption: LocalConsumption::Available(10),
            budget_cap: Some(10),
            calibration: crate::sor::CalibrationObs::new(
                Some("v1".into()),
                Some(800),
                Some(now() - chrono::Duration::minutes(1)),
                chrono::Duration::hours(24),
                now(),
            ),
            availability: crate::sor::Observation::missing(crate::sor::ObservationSource::Probe),
            cost: crate::sor::Observation::missing(crate::sor::ObservationSource::PriceTable),
        };
        let req = SorRequest {
            subject: "committee-1".into(),
            revision: "rev-1".into(),
            criticality: CriticalityLevel::Root,
            criticality_actors: vec!["policy".into()],
            policy_digest: "policy:committee".into(),
            attempt: 1,
            supersedes: None,
        };
        let err = select(&req, &[cand], &ScoreWeights::default(), now()).unwrap_err();
        // Typed refusal — never a silent fall-back to the rejected seat.
        assert!(matches!(err, SorRefusal::NoAdmissibleCandidate { .. }));
        // The witness plan is unchanged: the rejected seat is still off the ballot.
        assert!(!sor_may_not_resurrect(&plan, "rejected"));
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-07-12T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    // ── conjunctive verdict-door ─────────────────────────────────────────

    fn outcome(id: &str, verdict: SeatVerdict, red: bool) -> SeatOutcome {
        SeatOutcome {
            seat_id: id.into(),
            verdict,
            falsifier_red: red,
        }
    }

    #[test]
    fn all_confirmed_is_confirmed() {
        let outcomes = vec![
            outcome("a", SeatVerdict::Confirmed, false),
            outcome("b", SeatVerdict::Confirmed, false),
        ];
        assert_eq!(committee_verdict(&outcomes), CommitteeVerdict::Confirmed);
    }

    #[test]
    fn one_refutation_amid_confirmations_refutes() {
        // The load-bearing verdict-door test: a lone refuter in a sea of
        // confirmations still refutes — never a majority vote.
        let outcomes = vec![
            outcome("a", SeatVerdict::Confirmed, false),
            outcome("b", SeatVerdict::Refuted, false),
            outcome("c", SeatVerdict::Confirmed, false),
        ];
        assert_eq!(committee_verdict(&outcomes), CommitteeVerdict::Refuted);
    }

    #[test]
    fn a_single_red_falsifier_refutes_even_when_every_verdict_confirms() {
        let outcomes = vec![
            outcome("a", SeatVerdict::Confirmed, false),
            outcome("b", SeatVerdict::Confirmed, true), // falsifier went red
        ];
        assert_eq!(committee_verdict(&outcomes), CommitteeVerdict::Refuted);
    }

    #[test]
    fn an_inconclusive_seat_makes_the_committee_inconclusive() {
        let outcomes = vec![
            outcome("a", SeatVerdict::Confirmed, false),
            outcome("b", SeatVerdict::Inconclusive, false),
        ];
        assert_eq!(committee_verdict(&outcomes), CommitteeVerdict::Inconclusive);
    }

    #[test]
    fn empty_jury_cannot_confirm() {
        assert_eq!(committee_verdict(&[]), CommitteeVerdict::Inconclusive);
    }

    // ── durable committee-posture delivery (witness 2 hole) ──────────────

    #[test]
    fn posture_document_pins_version_and_hash() {
        let doc = render_committee_posture(
            ADVERSARIAL_BRIEFING_VERSION,
            "blake3:cafe",
            "Refute the fix. Try to make the falsifier go red.",
        );
        assert!(doc.contains(&format!(
            "contract-version:** {ADVERSARIAL_BRIEFING_VERSION}"
        )));
        assert!(doc.contains("contract-hash:** blake3:cafe"));
        assert!(doc.contains("Refute the fix"));
        // States its own durability so a reader (or a script) is warned off.
        assert!(doc.to_lowercase().contains("durable"));
    }

    #[test]
    fn posture_reference_names_the_durable_file_not_an_inline_copy() {
        let stanza = committee_posture_reference();
        assert!(stanza.contains(COMMITTEE_POSTURE_FILE));
        // The pointer must explain *why* it is a pointer, not an inline copy —
        // that is the whole point of surviving regeneration.
        assert!(stanza.contains("cs evolve"));
    }

    #[test]
    fn durable_injection_requires_both_file_and_reference() {
        // Both facts present → delivered → witness passes.
        let ok = AdversarialBriefing::from_durable_injection(
            ADVERSARIAL_BRIEFING_VERSION,
            "blake3:cafe",
            true,
            true,
        );
        assert!(ok.injected);
        assert!(ok.is_valid());

        // File present but briefing does not reference it → not delivered.
        let no_ref = AdversarialBriefing::from_durable_injection(
            ADVERSARIAL_BRIEFING_VERSION,
            "blake3:cafe",
            true,
            false,
        );
        assert!(!no_ref.injected);
        assert!(!no_ref.is_valid());

        // Briefing references the file but the durable file is missing → the
        // pointer is dangling, so the contract was never actually delivered.
        let dangling = AdversarialBriefing::from_durable_injection(
            ADVERSARIAL_BRIEFING_VERSION,
            "blake3:cafe",
            false,
            true,
        );
        assert!(!dangling.injected);
    }

    #[test]
    fn a_seat_with_durable_posture_delivery_is_admitted() {
        // End-to-end: a refuter whose contract is delivered via the durable
        // file (not a clobbered inline section) passes witness 2.
        let gen = seat("gen", SeatRole::Generator, "anthropic", "author");
        let mut delivered = seat("delivered", SeatRole::Refuter, "openai", "skeptic");
        delivered.persona.briefing = Some(AdversarialBriefing::from_durable_injection(
            ADVERSARIAL_BRIEFING_VERSION,
            "blake3:cafe",
            true,
            true,
        ));
        let plan = plan_committee(&gen, &[delivered], root_req());
        assert_eq!(plan.admitted.len(), 1);
        assert!(plan.rejected.is_empty());
    }

    #[test]
    fn proxy_costume_ceiling_is_stated_not_hidden() {
        // The §8b honesty line is a first-class, testable artefact.
        assert!(FamilyWitness::proxy_costume_ceiling().contains("tier (b)"));
        assert!(FamilyWitness::proxy_costume_ceiling().contains("attested"));
    }
}
