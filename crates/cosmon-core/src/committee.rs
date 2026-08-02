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
//! # A roster is planned before it runs — so the floor is re-checked after
//!
//! [`plan_committee`] admits seats against a floor, and until
//! [`fold_committee`] existed nothing checked that floor again once the seats
//! had actually run. Two field failures came through that gap
//! (converge-20260727-a302):
//!
//! - A seat's provider **refused the work mid-review**, twice in two days. The
//!   seat had been the only one bearing the diversity floor, so the round's
//!   remaining confirmation was an echo of the generator — and the plain
//!   [`committee_verdict`] door, which sees outcomes and not the roster behind
//!   them, folded it as a clean pass. [`SeatDelivery`] gives *"never allowed to
//!   look"* a type distinct from *"looked and could not decide"*, and
//!   [`jury_integrity`] re-counts the floor over the seats that delivered.
//! - An operator switched a stalled seat to a sibling model from inside the
//!   pane; the roster kept reporting the **specified** model while a different
//!   one answered. [`SeatOutcome::realized_endpoint`] carries what answered, and
//!   the delivered floor is counted on it — because provider diversity is a
//!   claim about what answered, never about what was configured. A seat whose
//!   realized endpoint was never observed carries none of the floor either:
//!   unknown is not a synonym for compliant, and the alternative is to let
//!   absence of evidence stand in for evidence of diversity.
//!
//! [`RosterPlan::floor_is_single_point_of_failure`] names the upstream defect
//! that made both bite: a roster can be perfectly admissible and still rest its
//! whole diversity guarantee on one seat.
//!
//! Recording those facts is not the same as acting on them, and for a while the
//! module only recorded. [`JuryIntegrity::is_intact`] answered honestly that a
//! jury had been rescued by hand, and nothing consulted it, so the round still
//! certified while carrying its own admission in the same struct. A caveat the
//! reader cannot act on is not a control. [`fold_committee`] now reads it: a
//! jury that is not intact may **refute**, and may never **certify**.
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

/// What a seat's `committee-posture.md` actually SAYS, once read rather than
/// merely counted.
///
/// The witness that consumes this ([`RosterSpec::with_observed_delivery`]) used
/// to ask only whether the file existed, so a seat whose entire contract was
/// `# posture\n` was certified as having received one — presence standing in
/// for content, which is the defect class this lineage keeps reproducing. The
/// parsed header is what lets the gate compare the file against the contract
/// the roster declares, instead of against nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostureContract {
    /// The `contract-version` the FILE declares, to be matched against the
    /// version the roster's [`AdversarialBriefing`] declares.
    pub version: u32,
    /// The `contract-hash` the FILE declares, to be matched against the hash
    /// the roster's [`AdversarialBriefing`] declares.
    pub contract_hash: String,
    /// The adversarial contract prose itself, after the header's `---` rule,
    /// trimmed. Never empty: a contract with no body is not a contract, and
    /// [`parse_committee_posture`] returns `None` rather than an empty one.
    pub body: String,
}

/// Read a `committee-posture.md` back as [`render_committee_posture`] wrote it,
/// or `None` when the text is not a rendered adversarial contract at all.
///
/// # What `None` means, precisely
///
/// `None` is returned when the text carries no `contract-version` line, no
/// `contract-hash` line, or no non-empty body after the header's `---` rule.
/// Those are exactly the states in which the file is a *placeholder*: it
/// occupies the path the witness looks at without carrying the thing the
/// witness is a witness for. A stub such as `# posture\n` parses to `None` and
/// therefore fails delivery, where under the presence-only witness it passed.
///
/// The parse is deliberately tolerant about everything else — heading wording,
/// the HTML comment, blank lines — because the gate's job is to refuse an empty
/// contract, not to refuse a contract whose renderer was revised.
#[must_use]
pub fn parse_committee_posture(text: &str) -> Option<PostureContract> {
    let field = |name: &str| -> Option<String> {
        text.lines().find_map(|l| {
            l.trim()
                .strip_prefix("- **")?
                .strip_prefix(name)?
                .strip_prefix(":**")
                .map(|v| v.trim().to_string())
        })
    };
    let version: u32 = field("contract-version")?.parse().ok()?;
    let contract_hash = field("contract-hash").filter(|h| !h.is_empty())?;
    // The body is what follows the LAST header rule — `---` on its own line —
    // so a body that itself contains a rule cannot truncate the parse.
    let (_, body) = text.rsplit_once("\n---\n")?;
    let body = body.trim();
    (!body.is_empty()).then(|| PostureContract {
        version,
        contract_hash,
        body: body.to_string(),
    })
}

/// The exact bytes a `contract-hash` is a digest **of**.
///
/// [`render_committee_posture`] writes the contract prose as `{body}\n` and
/// [`parse_committee_posture`] hands it back trimmed, so the one byte string
/// both halves can agree on is the trimmed prose with a single trailing
/// newline. Naming it here, once, is what makes "the hash is the body's digest"
/// a decidable statement rather than a question of whose whitespace survived.
///
/// Measured 2026-08-01 across the 29 live `committee-posture.md` files in the
/// default fleet: 20 verify under this normalisation, and none verifies under
/// any of the five others tried (the post-rule text raw, untrimmed,
/// left-stripped, the whole file, or header-plus-body) crossed with six digest
/// algorithms. The convention was already in the corpus; this only writes it
/// down.
fn contract_digest_input(body: &str) -> String {
    format!("{}\n", body.trim())
}

/// The digest algorithms this gate can actually recompute, and therefore the
/// only ones under which a `contract-hash` can be verified.
///
/// Both are already workspace dependencies. An algorithm outside this list is
/// refused rather than waved through ([`ContractHashVerdict::Unverifiable`]):
/// an algorithm name the verifier cannot compute is an opaque label with extra
/// syllables, and accepting it would reopen by the back door the exact hole
/// this check closes.
const SUPPORTED_CONTRACT_DIGESTS: [&str; 2] = ["blake3", "sha256"];

/// Hex digest of `bytes` under `algorithm`, or `None` when the algorithm is
/// not one of [`SUPPORTED_CONTRACT_DIGESTS`].
fn digest_with(algorithm: &str, bytes: &[u8]) -> Option<String> {
    match algorithm {
        "blake3" => Some(cosmon_hash::Hash::of_bytes(bytes).to_hex()),
        "sha256" => {
            use sha2::{Digest, Sha256};
            Some(format!("{:x}", Sha256::digest(bytes)))
        }
        _ => None,
    }
}

/// Whether `s` is a 64-character lowercase hex string — the width of every
/// digest in [`SUPPORTED_CONTRACT_DIGESTS`].
fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// The canonical `contract-hash` for a contract body: what a convener must
/// write into `roster.json` and into the rendered
/// [`COMMITTEE_POSTURE_FILE`] for the seat to be admitted.
///
/// # Why this function has to exist for the check to be a control
///
/// Requiring a digest to verify without publishing the way to compute one
/// would refuse every future convene as well as the forged ones — a gate that
/// nobody can pass is the outage a control is supposed to prevent. This is the
/// counterweight: the hash a convener needs is computed, not guessed.
///
/// # Computing it without Rust
///
/// No `cs` verb authors a `committee-posture.md`; a convener writes it, so the
/// hash has to be reachable from a shell. Because the digested bytes are just
/// the contract prose with a single trailing newline, a body file that ends in
/// exactly one newline hashes directly — and `sha256` is a supported algorithm
/// precisely so this works with no extra tooling:
///
/// ```text
/// shasum -a 256 body.md      # → sha256:<that hex>
/// b3sum body.md              # → blake3:<that hex>
/// ```
///
/// Verified 2026-08-01 against a live contract: `shasum -a 256` over the
/// extracted body reproduces the `sha256:` hash that file declares, byte for
/// byte.
///
/// ```
/// use cosmon_core::committee::{committee_contract_hash, verify_contract_hash,
///                              ContractHashVerdict};
///
/// let body = "Audit the artefacts. The generator's confidence is not evidence.";
/// let hash = committee_contract_hash(body);
/// assert!(hash.starts_with("blake3:"));
/// assert!(matches!(
///     verify_contract_hash(&hash, body),
///     ContractHashVerdict::Verified { .. }
/// ));
/// ```
#[must_use]
pub fn committee_contract_hash(body: &str) -> String {
    let hex = cosmon_hash::Hash::of_bytes(contract_digest_input(body).as_bytes()).to_hex();
    format!("blake3:{hex}")
}

/// What [`verify_contract_hash`] found when it recomputed a declared
/// `contract-hash` over the body that sits beneath it.
///
/// The three outcomes are kept apart because the refusals mean different
/// things to whoever reads them: a *forged* hash is a claim about the body
/// that the body contradicts, while an *unverifiable* one is a hash that names
/// something this gate cannot compute. Collapsing them would send a reader
/// hunting for tampering when the actual fix is to restate the hash under a
/// supported algorithm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractHashVerdict {
    /// The declared hash is the digest of the body, under a named algorithm.
    Verified {
        /// The algorithm that reproduced it — `blake3` or `sha256`, the two
        /// this gate can recompute.
        algorithm: &'static str,
    },
    /// The declared hash is digest-shaped and is **not** the body's digest.
    Forged {
        /// Reader-facing detail: what was declared, what the body actually
        /// digests to, and under which algorithm.
        detail: String,
    },
    /// The declared hash cannot be checked at all — it names no algorithm this
    /// gate can compute, or it is not digest-shaped in the first place.
    Unverifiable {
        /// Reader-facing detail naming what was declared and what is required.
        detail: String,
    },
}

impl ContractHashVerdict {
    /// The refusal line, or `None` when the digest verified.
    #[must_use]
    pub fn refusal(&self) -> Option<&str> {
        match self {
            Self::Verified { .. } => None,
            Self::Forged { detail } | Self::Unverifiable { detail } => Some(detail),
        }
    }
}

/// Recompute a declared `contract-hash` over the contract body it sits above,
/// and say whether it is that body's digest.
///
/// # The hole this closes, and the false sentence that held it open
///
/// This field used to be a **self-attested label**: the witness compared the
/// seat's file against the convener's roster and never asked whether the hash
/// content-addressed anything. The stated reason was that live rosters carry
/// an opaque label whose digest is not the body's, so requiring verification
/// "would refuse every committee convened to date — an outage, not a control."
///
/// That sentence was measured on 2026-08-01 against the 29 live
/// `committee-posture.md` files in the default fleet, under the normalisation
/// [`parse_committee_posture`] itself computes, and it is **false in both of
/// its claims**:
///
/// - **Not one of the 29 is an opaque label.** All 29 are digest-shaped: 8
///   bare 64-hex, 21 prefixed (`blake3:`, `sha256:`, `blake2b-256:`,
///   `blake3-substitute:sha256:`).
/// - **20 of the 29 already verify** against their own body. Requiring
///   verification refuses 9 — 31%, not 100%.
///
/// And the 9 are not honest hashes under a normalisation nobody wrote down.
/// They match nothing under six algorithms crossed with six normalisations,
/// and two of them are self-evidently fabricated: three *different* contracts
/// (1209, 1209 and 1135 bytes of distinct prose) declare the single identical
/// value `blake3:7bf51880…`, and one declares `sha256:` followed by 32 hex
/// characters, which is half the width of a sha256. So the justification did
/// not merely overstate the cost — it described the corpus backwards, and the
/// hole it licensed was hiding exactly the fabrications a digest check exists
/// to catch.
///
/// # The shape the check therefore has
///
/// Verification is **algorithm-agnostic by declared prefix**, because the
/// corpus forces it: of the 20 hashes that verify, 19 are sha256 or
/// blake2b-256 and only one is blake3. A blake3-only verifier would refuse 28
/// of 29 — that really would be the outage. So the declared prefix selects the
/// algorithm, and a bare hex string (the legacy shape) is tried under each
/// supported algorithm.
///
/// # What it still cannot see
///
/// A digest binds the hash to the body; it does not bind the body to anything
/// outside the pair. A **convener** authors both `roster.json` and the seat's
/// rendered contract, so one that writes a fabricated body and then digests it
/// correctly passes — as it did before. This closes the gap between the hash
/// and the body, which is the gap that was open; it does not make either party
/// honest, and nothing inside one party's own files ever could.
///
/// ```
/// use cosmon_core::committee::{committee_contract_hash, verify_contract_hash,
///                              ContractHashVerdict};
///
/// let body = "Try to make the falsifier go red.";
/// // A hash over the body verifies…
/// assert!(verify_contract_hash(&committee_contract_hash(body), body).refusal().is_none());
/// // …and the same hash over a body someone swapped underneath it does not.
/// assert!(matches!(
///     verify_contract_hash(&committee_contract_hash(body), "Be agreeable."),
///     ContractHashVerdict::Forged { .. }
/// ));
/// ```
#[must_use]
pub fn verify_contract_hash(declared: &str, body: &str) -> ContractHashVerdict {
    let input = contract_digest_input(body);
    let bytes = input.as_bytes();
    let declared = declared.trim();
    // The hex is the last colon-separated segment; everything before it names
    // the algorithm. Compound prefixes occur in the live corpus
    // (`blake3-substitute:sha256:<hex>`), so the algorithm is the LAST token
    // that names one rather than the first token outright.
    let (prefix, hex) = declared.rsplit_once(':').unwrap_or(("", declared));
    let hex = hex.trim().to_ascii_lowercase();

    if prefix.is_empty() {
        // Bare hex: it names no algorithm, so it is a digest exactly if it is
        // one under some algorithm this gate can compute.
        if !is_hex64(&hex) {
            return ContractHashVerdict::Unverifiable {
                detail: format!(
                    "contract-hash `{declared}` is not a digest — it names no \
                     algorithm and is not 64 hex characters. A stable label is \
                     no longer accepted: it is a self-attestation, not a \
                     content address. Write the value \
                     `committee_contract_hash` computes for this body"
                ),
            };
        }
        for algorithm in SUPPORTED_CONTRACT_DIGESTS {
            if digest_with(algorithm, bytes).as_deref() == Some(hex.as_str()) {
                return ContractHashVerdict::Verified { algorithm };
            }
        }
        return ContractHashVerdict::Forged {
            detail: format!(
                "contract-hash `{declared}` is digest-shaped but is not this \
                 body's digest under any supported algorithm ({}); the body \
                 digests to `{}`",
                SUPPORTED_CONTRACT_DIGESTS.join(" or "),
                committee_contract_hash(body),
            ),
        };
    }

    let Some(algorithm) = prefix
        .split(':')
        .filter_map(|token| {
            SUPPORTED_CONTRACT_DIGESTS
                .into_iter()
                .find(|a| *a == token.trim().to_ascii_lowercase())
        })
        .next_back()
    else {
        return ContractHashVerdict::Unverifiable {
            detail: format!(
                "contract-hash `{declared}` names no algorithm this gate can \
                 recompute (supported: {}), so its digest cannot be checked. \
                 An algorithm name that nothing verifies is an opaque label \
                 with extra syllables — restate the hash as \
                 `{}`",
                SUPPORTED_CONTRACT_DIGESTS.join(", "),
                committee_contract_hash(body),
            ),
        };
    };

    if !is_hex64(&hex) {
        return ContractHashVerdict::Forged {
            detail: format!(
                "contract-hash `{declared}` names {algorithm} but carries {} \
                 hex characters; a {algorithm} digest is 64. It cannot be a \
                 digest of anything, whatever body sits beneath it — the body \
                 digests to `{}`",
                hex.len(),
                committee_contract_hash(body),
            ),
        };
    }

    if digest_with(algorithm, bytes).as_deref() == Some(hex.as_str()) {
        ContractHashVerdict::Verified { algorithm }
    } else {
        ContractHashVerdict::Forged {
            detail: format!(
                "contract-hash `{declared}` is NOT the {algorithm} digest of \
                 the contract body beneath it — that body digests to `{}:{}`. \
                 The hash is a claim about the body and the body refutes it",
                algorithm,
                digest_with(algorithm, bytes).unwrap_or_default(),
            ),
        }
    }
}

/// The two delivery facts observed in ONE seat's own molecule directory, as
/// [`RosterSpec::with_observed_delivery`] asks its injected port for them.
///
/// It is a struct rather than a tuple because the posture leg now has three
/// distinguishable states — absent, present-but-not-the-contract, and the
/// declared contract — and a refusal that cannot say which of the three it saw
/// sends a reader to the wrong file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ObservedDelivery {
    /// Whether [`COMMITTEE_POSTURE_FILE`] exists at all in the seat's
    /// directory. Kept beside `posture` so a stub file can be reported as a
    /// stub rather than as a missing file.
    pub posture_file_exists: bool,
    /// The parsed contract, or `None` when the file is absent or is not a
    /// rendered contract. See [`parse_committee_posture`].
    pub posture: Option<PostureContract>,
    /// Whether the seat's regenerated `briefing.md` carries the pointer at the
    /// durable file ([`committee_posture_reference`]).
    pub pointer: bool,
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
    /// The **model id** the seat was pinned to, when the adapter names one.
    ///
    /// The tuple deliberately stops at *family*, because family is the
    /// error-independence axis the floor is counted on. But two siblings inside
    /// one family (`gpt-5.6-sol` and `gpt-5.6-terra`) are the same family and a
    /// **different pin**, and a mid-round switch between them is exactly the
    /// event the field keeps producing. Without this field such a switch is
    /// invisible to [`SeatDrift`] — realized tuple equals specified tuple, so
    /// nothing is recorded — and the roster goes on reporting the model that
    /// was rostered while another one answered. Family governs the *score*;
    /// this governs the *record*.
    #[serde(default)]
    pub model: Option<String>,
}

impl FamilyWitness {
    /// Resolve a seat's family witness from the project `[adapters]` inventory,
    /// exactly as [`resolve_endpoint_tuple`] does, carrying the adapter's
    /// pinned model alongside the resolved tuple.
    #[must_use]
    pub fn resolve(adapters: Option<&crate::config::AdaptersConfig>, seat: &str) -> Self {
        Self {
            endpoint: resolve_endpoint_tuple(adapters, seat),
            model: adapters
                .and_then(|a| a.entry(seat))
                .and_then(|e| e.default_model.clone()),
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
    /// Content hash of the injected contract text, so an audit can confirm
    /// *which* contract was delivered — and, since it is now recomputed rather
    /// than taken on the convener's word, that the named contract is the one
    /// actually beneath it.
    ///
    /// Write the value [`committee_contract_hash`] computes. A stable label is
    /// no longer accepted: it made this field a self-attestation wearing a
    /// checksum. The verifier reads the algorithm from the prefix and also
    /// accepts the legacy bare-64-hex shape; see [`verify_contract_hash`].
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
    /// - `posture_contract_delivered`: the durable [`COMMITTEE_POSTURE_FILE`]
    ///   in the seat's molecule directory **is the contract this briefing
    ///   declares** — it parses as a rendered contract
    ///   ([`parse_committee_posture`]) and its header's version and hash are
    ///   the ones named here. The contract survives regeneration because *this*
    ///   file is never rewritten by `cs evolve`; and
    /// - `briefing_references_posture`: the seat's regenerated per-step
    ///   `briefing.md` carries the stable [`committee_posture_reference`]
    ///   pointer at it.
    ///
    /// An inline `## Committee posture` section written straight into
    /// `briefing.md` is *not* durable delivery — the next `cs evolve` clobbers
    /// it — so it can never satisfy this constructor.
    ///
    /// # The first fact used to be presence, and presence is not content
    ///
    /// It was once enough that a file *existed* at that path. A seat whose
    /// entire alleged adversarial contract was `# posture\n` therefore passed
    /// the witness (measured on the committed positive test, 2026-07-29): the
    /// gate still passed when the constrained party said something EMPTY. The
    /// caller is now required to have READ the file; see
    /// [`RosterSpec::with_observed_delivery`], which is where the comparison
    /// happens and where the limits of what it can catch are stated.
    #[must_use]
    pub fn from_durable_injection(
        version: u32,
        contract_hash: impl Into<String>,
        posture_contract_delivered: bool,
        briefing_references_posture: bool,
    ) -> Self {
        Self {
            version,
            contract_hash: contract_hash.into(),
            injected: posture_contract_delivered && briefing_references_posture,
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
    /// The `[adapters.<name>]` section this seat actually sits on — the only
    /// field on this struct a gate can **check against something**.
    ///
    /// [`Self::family`] is what the convener *says*. This is where the truth
    /// lives: given the name, [`FamilyWitness::resolve`] derives the endpoint
    /// tuple from the project's own `[adapters]` inventory, and
    /// [`RosterSpec::violations`] refuses the roster when the two disagree.
    /// Without it the family witness is a self-declaration — a roster could
    /// simply claim two distinct families and pass, which is the defect this
    /// field exists to close.
    ///
    /// `None` is itself a violation, not an exemption: a seat that names no
    /// adapter has made an unresolvable claim. It is optional in the schema
    /// only so a roster written before this field existed still *parses* and
    /// can be refused with a sentence, rather than failing as unreadable JSON.
    #[serde(default)]
    pub adapter: Option<String>,
    /// Provider-family witness (axis 1), **as declared**. Checked against the
    /// resolution of [`Self::adapter`]; never trusted on its own.
    pub family: FamilyWitness,
    /// Persona/role witness (axis 2).
    pub persona: PersonaWitness,
    /// The seat this one **replaced** on a re-convocation, when it is a
    /// replacement.
    ///
    /// A round is re-convened when a seat fails for a reason that is not a
    /// quality verdict — a provider refusal, a collapse, a machine that slept.
    /// The replacement is a *different molecule*, so the roster's membership
    /// changes, and until 2026-07-28 nothing in the schema could say so.
    /// Measured on `converge-20260728-7161`: after re-convocation `roster.json`
    /// still named the COLLAPSED seat as floor-bearing while the seat that
    /// actually sat appeared on no roster at all. A reader auditing from the
    /// roster alone would have concluded the floor was carried by a molecule
    /// that never executed, and the only record of the truth was prose in a
    /// ledger.
    ///
    /// [`RosterSpec::reconvocation_violations`] is what makes this field
    /// load-bearing rather than decorative: a roster naming a collapsed seat
    /// that no seat records replacing is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_seat_id: Option<String>,
    /// Why the replaced seat was replaced — in the seat's own record, not in a
    /// ledger beside it.
    ///
    /// Required whenever [`Self::replaced_seat_id`] is set, and required to be
    /// non-empty: *"replaced"* with no cause is the shape that lets a
    /// re-convocation launder a quality refusal into a jury failure, which is
    /// exactly the substitution the max-rounds rule exists to prevent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_reason: Option<String>,
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

    /// Whether this rejection is about **delivery** (something the seat has not
    /// received or produced *yet*) rather than **structure** (something about
    /// the roster that no amount of dispatching will change).
    ///
    /// # Why the distinction is load-bearing, and not bookkeeping
    ///
    /// A convener writes `roster.json` *before* any seat is dispatched. At that
    /// instant no seat can carry an injected briefing or a falsification
    /// artefact — and neither gap is one the convener could close by writing
    /// harder. An *injected* briefing takes two facts
    /// ([`AdversarialBriefing::from_durable_injection`]): the durable
    /// [`COMMITTEE_POSTURE_FILE`], which the convening driver does write, AND a
    /// `briefing.md` that points at it — and that pointer is established only by
    /// a verb that writes the briefing (`cs tackle`, `cs evolve`, `cs
    /// complete`), none of which has run. The falsification-attempt artefact
    /// arrives later still: the **seat worker** writes it during its own
    /// execution, so no `cs` verb authors it at all. So at convene every
    /// refuter is rejected on a delivery axis, no
    /// refuter is admitted, the admitted roster spans exactly one family, and a
    /// floor counted over admitted seats alone is **unmeetable by construction**
    /// — a bar no convene step can ever clear.
    ///
    /// That is a gate that always fails, which is an outage, not a control. The
    /// floor is therefore counted over the families the roster can *reach*
    /// ([`RosterPlan::reachable_families`]) — the seats that already cleared
    /// every structural witness and are merely waiting to be dispatched — while
    /// a genuinely single-family roster still fails it, because no dispatch
    /// would give it a second family either. Missing delivery is reported by
    /// its own line, which is the accurate thing to say about it.
    #[must_use]
    pub const fn is_delivery(&self) -> bool {
        match self {
            Self::BriefingNotInjected | Self::FalsificationArtifactMissing => true,
            Self::FamilyCollision { .. } | Self::PersonaCollision { .. } => false,
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
    /// The model the seat was pinned to at plan time — the *specified* side of
    /// any later `specified ~> realized` record. See [`FamilyWitness::model`].
    #[serde(default)]
    pub model: Option<String>,
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
    /// The endpoint tuple this seat resolved to.
    ///
    /// Kept on the rejection — not only on admission — because a seat rejected
    /// on a *delivery* axis ([`SeatRejection::is_delivery`]) already cleared
    /// witness 1, so its family is a real, distinct family the roster reaches.
    /// [`RosterPlan::reachable_families`] needs it to tell a convene-shaped
    /// roster (families present, contracts not yet delivered) apart from a
    /// single-family one, which is a difference the seat *count* cannot see.
    #[serde(default)]
    pub endpoint: EndpointTuple,
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
    /// [`CommitteeRequirement::min_distinct_families`] distinct families —
    /// the **realized** floor, over seats that are seated right now.
    ///
    /// This is the property that must hold when a committee actually sits. It
    /// is deliberately NOT the property a convene-time gate can demand: see
    /// [`Self::floor_reachable`].
    pub floor_met: bool,
    /// Whether the roster can *ever* span the floor — counted over
    /// [`Self::reachable_families`], which includes seats held back only by a
    /// delivery axis ([`SeatRejection::is_delivery`]).
    ///
    /// `false` is the real **missing-seat** finding: no dispatch, no contract
    /// and no artefact will give this roster another family, so the committee
    /// cannot be convened as written and the convener must widen it. `true`
    /// with [`Self::floor_met`] `false` is a roster that is correctly shaped
    /// and simply not dispatched yet — a fact already reported, precisely, by
    /// the per-seat delivery lines.
    pub floor_reachable: bool,
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

    /// The distinct families the roster **reaches** — the admitted ones plus
    /// those of seats rejected only on a delivery axis.
    ///
    /// A seat is rejected in witness order: family collision first, then role
    /// collision, then contract delivery, then falsification evidence. So a
    /// seat carrying [`SeatRejection::is_delivery`] has already been proven to
    /// hold an endpoint tuple distinct from the generator's and from every peer
    /// admitted before it. Its family is real; only its paperwork is missing.
    ///
    /// This is the count the convene-time floor is measured on. See
    /// [`SeatRejection::is_delivery`] for why measuring it on
    /// [`Self::distinct_families`] instead produces a bar nothing can clear.
    #[must_use]
    pub fn reachable_families(&self) -> usize {
        let mut fams: std::collections::BTreeSet<&str> =
            std::iter::once(self.generator.endpoint.family.as_str()).collect();
        fams.extend(self.admitted.iter().map(|s| s.endpoint.family.as_str()));
        fams.extend(
            self.rejected
                .iter()
                .filter(|r| r.reason.is_delivery())
                .map(|r| r.endpoint.family.as_str()),
        );
        fams.len()
    }

    /// The admitted seats whose *removal alone* would drop the roster below its
    /// family floor — the seats that actually **bear** the diversity guarantee.
    ///
    /// A seat is floor-bearing when it is the sole admitted holder of its
    /// family and the roster is exactly at the floor. A seat that duplicates a
    /// family already covered by a peer bears none of the floor: it reads on
    /// the roster, it does not hold it up. Naming this explicitly is what lets
    /// a driver tell "two independent families" apart from "one family plus a
    /// second reader," which is not something the seat *count* can distinguish.
    #[must_use]
    pub fn floor_bearing_seats(&self) -> Vec<String> {
        if !self.requirement.required {
            return Vec::new();
        }
        self.admitted
            .iter()
            .filter(|candidate| {
                let mut fams: std::collections::BTreeSet<&str> =
                    std::iter::once(self.generator.endpoint.family.as_str()).collect();
                fams.extend(
                    self.admitted
                        .iter()
                        .filter(|s| s.seat_id != candidate.seat_id)
                        .map(|s| s.endpoint.family.as_str()),
                );
                fams.len() < self.requirement.min_distinct_families
            })
            .map(|s| s.seat_id.clone())
            .collect()
    }

    /// Whether **some single seat's** refusal would vacate the floor — a jury
    /// with no diversity slack, one provider refusal away from vacuous.
    ///
    /// True exactly when [`floor_bearing_seats`](Self::floor_bearing_seats) is
    /// non-empty, which is the standard reading of *single point of failure*:
    /// there exists one seat whose loss breaks the guarantee. Note this is
    /// broader than "the floor rests on exactly one seat" — a roster sitting
    /// exactly at a floor of 3 has two load-bearing seats and is still
    /// vacated by either one refusing. Testing for `len() == 1` would call
    /// that roster safe, which is the same under-counting the whole module
    /// exists to stop.
    ///
    /// This is the *recipe*-level defect the kernel cannot fix on its own
    /// (converge-20260727-a302): a roster may be perfectly admissible and still
    /// be structurally fragile, because a conjunctive CLEAN-and-CLEAN door over
    /// a roster like this reads as two independent families while only one of
    /// them carries any error-independence at all. A caller that convenes a
    /// fragile roster must either widen it until the floor has slack, or record
    /// the fragility — it may never report a clean jury as though the floor
    /// were redundant.
    #[must_use]
    pub fn floor_is_single_point_of_failure(&self) -> bool {
        self.floor_met && !self.floor_bearing_seats().is_empty()
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
        model: generator.family.model.clone(),
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
                endpoint: seat.family.endpoint.clone(),
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
                endpoint: seat.family.endpoint.clone(),
            });
            continue;
        }
        if seat.persona.briefing.as_ref().is_none_or(|b| !b.is_valid()) {
            rejected.push(RejectedSeat {
                seat_id: seat.seat_id.clone(),
                reason: SeatRejection::BriefingNotInjected,
                endpoint: seat.family.endpoint.clone(),
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
                endpoint: seat.family.endpoint.clone(),
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
            model: seat.family.model.clone(),
            role_id: seat.persona.role_id.clone(),
        });
    }

    let mut plan = RosterPlan {
        requirement,
        generator: generator_admitted,
        admitted,
        rejected,
        floor_met: false,
        floor_reachable: false,
    };
    plan.floor_met =
        !requirement.required || plan.distinct_families() >= requirement.min_distinct_families;
    plan.floor_reachable =
        !requirement.required || plan.reachable_families() >= requirement.min_distinct_families;
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

/// What became of a seat between admission and the fold — the **liveness**
/// axis, kept separate from the seat's *opinion* ([`SeatVerdict`]).
///
/// A seat that never ran has no opinion; the two are different facts and
/// collapsing them is how a jury silently shrinks. `Inconclusive` says "I
/// looked and could not decide"; [`Self::ProviderRefusal`] says "I was never
/// allowed to look." Only the first is a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SeatDelivery {
    /// The seat executed and produced its verdict artefacts.
    #[default]
    Delivered,
    /// The seat's **provider refused the work mid-review** — a policy refusal
    /// on the provider side, not a verdict. Observed twice in two days on the
    /// codex seat of `converge-clean-room`, each time rescued by hand from
    /// inside the pane, leaving no trace that the jury had needed rescuing.
    /// Typing it is what turns that silent rescue into a recorded event.
    ProviderRefusal,
    /// The seat was dispatched but never reached a terminal state (a stall, a
    /// rate limit, a sleeping machine).
    Stalled,
    /// The seat was on the roster but was never dispatched at all.
    NotDispatched,
}

impl SeatDelivery {
    /// A stable, human-auditable label for the delivery disposition.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::ProviderRefusal => "provider-refusal",
            Self::Stalled => "stalled",
            Self::NotDispatched => "not-dispatched",
        }
    }

    /// Whether this seat actually reviewed. Only a delivering seat may carry
    /// any part of the diversity floor.
    #[must_use]
    pub const fn delivered(self) -> bool {
        matches!(self, Self::Delivered)
    }
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
    /// Whether the seat actually reviewed, or was refused / stalled / never
    /// dispatched. Defaults to [`SeatDelivery::Delivered`] so an older record
    /// that predates this axis still deserialises — such a record asserts a
    /// verdict, and a verdict implies the seat ran.
    #[serde(default)]
    pub delivery: SeatDelivery,
    /// The endpoint tuple the seat **actually ran on**, when it was observed.
    ///
    /// The roster's [`AdmittedSeat::endpoint`] is the *specified* tuple, resolved
    /// from config at plan time. When an operator switches a stalled seat to a
    /// sibling model mid-round, the specified tuple stops describing reality —
    /// and the diversity floor is a claim about *what answered*, never about
    /// what was configured.
    ///
    /// `None` means the realized endpoint was **not observed**, which is not
    /// the same fact as *observed to equal the specified one*. An unobserved
    /// seat therefore carries **no part** of the diversity floor — see
    /// [`JuryIntegrity::unobserved`]. Reading `None` as "same as rostered"
    /// would turn absence of evidence into evidence of diversity, the exact
    /// shape this lineage exists to refuse; the formula states the same rule in
    /// prose — *a bare pin with no realized id is an UNCONFIRMED seat, not a
    /// confirmed match*.
    ///
    /// Unlike [`Self::delivery`], the serde default is fail-**closed**. A
    /// stored verdict implies the seat ran, so `delivery` may default to
    /// [`SeatDelivery::Delivered`]; nothing about a stored verdict implies
    /// anyone looked at *which endpoint* produced it.
    #[serde(default)]
    pub realized_endpoint: Option<EndpointTuple>,
    /// The **model id that actually answered**, when observed.
    ///
    /// Finer than [`Self::realized_endpoint`], which stops at family. The
    /// recurring field event is a switch *inside* one family — `gpt-5.6-sol`
    /// stalls on a provider guardrail, the operator re-points the pane at
    /// `gpt-5.6-terra` — which leaves the tuple identical and would otherwise
    /// leave no trace at all. Three occurrences in two days on one seat; this
    /// is the normal operating condition of that seat, not an edge case.
    #[serde(default)]
    pub realized_model: Option<String>,
    /// Whether a **human re-pointed this seat after dispatch**.
    ///
    /// Distinct from a model difference, because the two do not imply each
    /// other: an operator may switch a seat back to its rostered pin (no
    /// difference, still a rescue), and a difference may arise without anyone
    /// touching the pane. It is recorded because the rescue is a fact about the
    /// jury that sat: a jury that needed hand-holding to produce a verdict is
    /// never [`JuryIntegrity::is_intact`], however good the verdict looks.
    #[serde(default)]
    pub switched_after_dispatch: bool,
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

/// The **required** `mechanism_polarity` field of a cmb-verify `verdict.json`:
/// what the seat's *stated mechanism* CLAIMED.
///
/// It exists because [`SeatVerdict`] is a **relative** verdict. `Confirmed`
/// means *the stated mechanism holds* — and whether that is good news depends
/// entirely on what the mechanism claimed, which the seat receives as an input
/// and no reader can infer. Without this field the door is unreadable, and a
/// reader that assumes a polarity reads the other one exactly backwards: a seat
/// that REPRODUCED a defect is filed as clean.
///
/// `cmb-verify.formula.toml`, step `verify-or-refute`, is the prose definition;
/// this type and [`map_through_polarity`] are its executable form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MechanismPolarity {
    /// The stated mechanism asserts something is BROKEN — the bug-intake
    /// polarity, where `confirmed` means the defect reproduces.
    Defect,
    /// The stated mechanism asserts something now HOLDS — the committee
    /// polarity, where `confirmed` means the audited fix survived.
    Fix,
}

impl MechanismPolarity {
    /// The stable wire label, as it appears in `verdict.json`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Defect => "defect",
            Self::Fix => "fix",
        }
    }

    /// Parse the `mechanism_polarity` field, tolerating case and surrounding
    /// space and nothing else.
    ///
    /// Returns `None` for any other spelling rather than guessing: a polarity a
    /// reader picked is the defect this field exists to prevent, so an
    /// unrecognised value must fail closed exactly like an absent one.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "defect" => Some(Self::Defect),
            "fix" => Some(Self::Fix),
            _ => None,
        }
    }
}

/// The `converge-clean-room` contract's vocabulary — the one a seat writes on
/// the first line of `referee-report.md`.
///
/// Unlike [`SeatVerdict`] this door is **absolute**: `Clean` means nothing was
/// found, in every context. The two vocabularies are the same judgement in two
/// alphabets, and the translation between them runs through
/// [`MechanismPolarity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING-KEBAB-CASE")]
pub enum ConvergeVerdict {
    /// Nothing was found. A thing a seat SAYS — never a thing a reader fails to
    /// find.
    Clean,
    /// The seat found something.
    Findings,
    /// No verdict was reachable.
    Inconclusive,
}

impl ConvergeVerdict {
    /// The stable wire label, as it appears after `VERDICT:`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Clean => "CLEAN",
            Self::Findings => "FINDINGS",
            Self::Inconclusive => "INCONCLUSIVE",
        }
    }

    /// Parse a bare verdict word, tolerating case and surrounding space.
    ///
    /// `None` for anything else — including the empty string. Fail-closed for
    /// the same reason as [`MechanismPolarity::parse`].
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "CLEAN" => Some(Self::Clean),
            "FINDINGS" => Some(Self::Findings),
            "INCONCLUSIVE" => Some(Self::Inconclusive),
            _ => None,
        }
    }

    /// Read the contract's first line of `referee-report.md`: `VERDICT: CLEAN`,
    /// `VERDICT: FINDINGS (3)`, `VERDICT: INCONCLUSIVE`.
    ///
    /// The trailing `(N)` count is discarded — it says how much was found, not
    /// whether anything was. A line that is not shaped `VERDICT: <word>` yields
    /// `None`, which the reader treats as a missing report rather than as
    /// clean.
    #[must_use]
    pub fn from_report_line(line: &str) -> Option<Self> {
        let rest = line.trim().strip_prefix("VERDICT:")?;
        let word = rest.split('(').next().unwrap_or(rest);
        Self::parse(word)
    }
}

impl SeatVerdict {
    /// The stable wire label, as it appears in `verdict.json`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Refuted => "refuted",
            Self::Inconclusive => "inconclusive",
        }
    }

    /// Parse a `verdict.json` `verdict` field **only when it speaks this
    /// door**, tolerating case and surrounding space.
    ///
    /// `None` for `CLEAN` / `FINDINGS` / `PASS` / `BLOCKED` and every other
    /// spelling. That is deliberate and is what scopes the polarity rule: a
    /// verdict written in the absolute [`ConvergeVerdict`] vocabulary needs no
    /// polarity, because its meaning does not depend on one.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "confirmed" => Some(Self::Confirmed),
            "refuted" => Some(Self::Refuted),
            "inconclusive" => Some(Self::Inconclusive),
            _ => None,
        }
    }
}

/// **The mapping between the two vocabularies, in code, once.**
///
/// | polarity | cmb-verify     | this contract  | means                          |
/// |----------|----------------|----------------|--------------------------------|
/// | `fix`    | `confirmed`    | `CLEAN`        | the audited fix holds          |
/// | `fix`    | `refuted`      | `FINDINGS`     | a falsifier went red           |
/// | `defect` | `confirmed`    | `FINDINGS`     | the claimed defect REPRODUCES  |
/// | `defect` | `refuted`      | `CLEAN`        | the claimed defect is not there|
/// | either   | `inconclusive` | `INCONCLUSIVE` | no verdict was reachable       |
///
/// Taking a [`MechanismPolarity`] **by value with no default** is the whole
/// point: there is no way to call this function without having decided the
/// polarity, so the prose instruction "state it, do not assume it" is enforced
/// by the signature rather than by a reader's diligence.
#[must_use]
pub const fn map_through_polarity(
    polarity: MechanismPolarity,
    verdict: SeatVerdict,
) -> ConvergeVerdict {
    match (polarity, verdict) {
        (MechanismPolarity::Fix, SeatVerdict::Confirmed)
        | (MechanismPolarity::Defect, SeatVerdict::Refuted) => ConvergeVerdict::Clean,
        (MechanismPolarity::Fix, SeatVerdict::Refuted)
        | (MechanismPolarity::Defect, SeatVerdict::Confirmed) => ConvergeVerdict::Findings,
        (_, SeatVerdict::Inconclusive) => ConvergeVerdict::Inconclusive,
    }
}

/// What one seat actually emitted, as read off disk — every field optional
/// because **absent is a real state that must fail closed**, and collapsing it
/// into a default is how the polarity got assumed in the first place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatEmission {
    /// The seat that produced these artefacts.
    pub seat_id: String,
    /// The `mechanism_polarity` field of `verdict.json`. `None` covers both
    /// "absent" and "present but unrecognised" — neither licenses a reading.
    pub mechanism_polarity: Option<MechanismPolarity>,
    /// The cmb-verify door from `verdict.json`, when the seat spoke it.
    pub verdict: Option<SeatVerdict>,
    /// The absolute verdict from the first line of `referee-report.md`, when
    /// the seat wrote one.
    pub reported: Option<ConvergeVerdict>,
}

/// Why a seat's emission could not be read as a verdict at all.
///
/// Every variant is NOT-CLEAN. None of them is `Findings` either: a seat whose
/// artefacts cannot be read has not reviewed anything, and pretending it
/// returned a quality opinion would be the same laundering in the other
/// direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatReadingRefusal {
    /// `verdict.json` carried no verdict this door recognises.
    NoVerdict,
    /// `verdict.json` spoke the relative door without its `mechanism_polarity`.
    /// The residual the both-files rule does not close, arriving from the
    /// simplest direction: the field is simply not there.
    MissingPolarity,
    /// No readable `VERDICT:` first line on `referee-report.md`. The contract
    /// requires an affirmative CLEAN in BOTH files; one file is not both.
    NoReport,
    /// Both files were readable, both were affirmative, and they say opposite
    /// things once the polarity is applied — the agreeing-but-wrong pair: a
    /// seat emitting `confirmed` and `VERDICT: CLEAN` together while its
    /// polarity is `defect` is saying "the defect reproduces" and "nothing
    /// found" in one breath.
    Incoherent {
        /// What the (polarity, cmb-verdict) pair maps to through the table.
        implied: ConvergeVerdict,
        /// What `referee-report.md` actually claimed.
        reported: ConvergeVerdict,
    },
}

impl SeatReadingRefusal {
    /// A human-actionable sentence naming the seat, what is wrong, and what
    /// would clear it.
    #[must_use]
    pub fn explain(self, seat: &str) -> String {
        match self {
            Self::NoVerdict => format!(
                "{seat}: its `verdict.json` carries no verdict in either door \
                 (`confirmed|refuted|inconclusive` or `CLEAN|FINDINGS|INCONCLUSIVE`). \
                 A missing verdict is NOT-CLEAN, never a pass"
            ),
            Self::MissingPolarity => format!(
                "{seat}: its `verdict.json` speaks the cmb-verify door \
                 (`confirmed|refuted|inconclusive`) with no `mechanism_polarity` \
                 field. That door is RELATIVE — `confirmed` means the stated \
                 mechanism holds, which is CLEAN for a claimed fix and FINDINGS \
                 for a claimed defect — so the verdict is unreadable without it. \
                 Add `\"mechanism_polarity\": \"defect\"|\"fix\"`; a reader may \
                 not pick one"
            ),
            Self::NoReport => format!(
                "{seat}: no readable `VERDICT: CLEAN|FINDINGS (N)|INCONCLUSIVE` \
                 on the first line of `referee-report.md`. The contract requires \
                 an AFFIRMATIVE clean in BOTH files; a verdict.json alone is one \
                 file"
            ),
            Self::Incoherent { implied, reported } => format!(
                "{seat}: its two files agree in form and contradict in substance — \
                 `verdict.json` maps through its stated polarity to {}, while \
                 `referee-report.md` declares VERDICT: {}. Two files agreeing is \
                 not two files being right. Fix the polarity or fix the verdict; \
                 the pair may not stand",
                implied.label(),
                reported.label(),
            ),
        }
    }
}

/// Read one seat's emission into a single absolute verdict, or refuse.
///
/// This is the executable form of the converge contract's reader rule, and it
/// is deliberately harder to satisfy than any of its prose restatements:
///
/// 1. a seat speaking the relative door WITHOUT `mechanism_polarity` is refused
///    ([`SeatReadingRefusal::MissingPolarity`]) — never mapped under an assumed
///    polarity, however obvious the convening loop makes it;
/// 2. a seat with no readable report line is refused, because the rule is an
///    affirmative verdict in BOTH files;
/// 3. a (polarity, verdict, VERDICT) triple that is not one row of
///    [`map_through_polarity`] is refused
///    ([`SeatReadingRefusal::Incoherent`]).
///
/// A seat that wrote only the absolute vocabulary in both files needs no
/// polarity — its verdict does not depend on one — and is read directly.
///
/// # Examples
///
/// ```
/// use cosmon_core::committee::{
///     read_seat_emission, ConvergeVerdict, MechanismPolarity, SeatEmission,
///     SeatReadingRefusal, SeatVerdict,
/// };
///
/// // A seat auditing a shipped fix, coherent: `confirmed` + `VERDICT: CLEAN`.
/// let ok = SeatEmission {
///     seat_id: "review-claude".into(),
///     mechanism_polarity: Some(MechanismPolarity::Fix),
///     verdict: Some(SeatVerdict::Confirmed),
///     reported: Some(ConvergeVerdict::Clean),
/// };
/// assert_eq!(read_seat_emission(&ok), Ok(ConvergeVerdict::Clean));
///
/// // The agreeing-but-wrong pair: the SAME two affirmative files, under the
/// // bug-intake polarity, say "the defect reproduces" and "nothing found".
/// let lying = SeatEmission {
///     mechanism_polarity: Some(MechanismPolarity::Defect),
///     ..ok.clone()
/// };
/// assert!(matches!(
///     read_seat_emission(&lying),
///     Err(SeatReadingRefusal::Incoherent { .. })
/// ));
///
/// // And the field simply absent is refused rather than assumed.
/// let bare = SeatEmission {
///     mechanism_polarity: None,
///     ..ok
/// };
/// assert_eq!(read_seat_emission(&bare), Err(SeatReadingRefusal::MissingPolarity));
/// ```
pub fn read_seat_emission(emission: &SeatEmission) -> Result<ConvergeVerdict, SeatReadingRefusal> {
    let Some(reported) = emission.reported else {
        return Err(SeatReadingRefusal::NoReport);
    };
    let Some(verdict) = emission.verdict else {
        // No relative door was spoken. The report line is absolute and stands
        // on its own — but only if `verdict.json` was silent, not if it was
        // unreadable, which the caller reports as `NoVerdict`.
        return Ok(reported);
    };
    let Some(polarity) = emission.mechanism_polarity else {
        return Err(SeatReadingRefusal::MissingPolarity);
    };
    let implied = map_through_polarity(polarity, verdict);
    if implied == reported {
        Ok(implied)
    } else {
        Err(SeatReadingRefusal::Incoherent { implied, reported })
    }
}

/// One converge round's reading over the seats that DELIVERED.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundReading {
    /// Whether the round is CLEAN — conjunctive over seats and over both files.
    pub clean: bool,
    /// Each seat that could be read, with its absolute verdict.
    pub readings: Vec<(String, ConvergeVerdict)>,
    /// Each seat that could not be read, with why.
    pub refusals: Vec<(String, SeatReadingRefusal)>,
}

/// Fold a round's seat emissions into CLEAN / NOT-CLEAN, failing closed.
///
/// `clean` requires **at least one** seat, **no** refusal, and every reading
/// equal to [`ConvergeVerdict::Clean`]. An empty seat set is NOT clean: there
/// was nobody to find anything, which is the case that used to silently pass.
#[must_use]
pub fn read_converge_round(emissions: &[SeatEmission]) -> RoundReading {
    let mut readings = Vec::new();
    let mut refusals = Vec::new();
    for emission in emissions {
        match read_seat_emission(emission) {
            Ok(v) => readings.push((emission.seat_id.clone(), v)),
            Err(r) => refusals.push((emission.seat_id.clone(), r)),
        }
    }
    let clean = !emissions.is_empty()
        && refusals.is_empty()
        && readings.iter().all(|(_, v)| *v == ConvergeVerdict::Clean);
    RoundReading {
        clean,
        readings,
        refusals,
    }
}

/// One seat whose realized endpoint diverged from the endpoint it was seated
/// under — the specified/realized drift, surfaced rather than inferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatDrift {
    /// The drifting seat.
    pub seat_id: String,
    /// The tuple the seat was admitted under (config-derived, plan time).
    pub specified: EndpointTuple,
    /// The tuple that actually answered, **when it was observed**.
    ///
    /// `None` records a drift whose destination is unknown: the seat is known
    /// to have been re-pointed by hand, but nobody wrote down which endpoint it
    /// landed on. That the rescue happened is a fact worth keeping even when
    /// where it went is not — the alternative is to drop the only evidence the
    /// jury needed rescuing.
    pub realized: Option<EndpointTuple>,
    /// The model the roster pinned, when it pinned one.
    #[serde(default)]
    pub specified_model: Option<String>,
    /// The model that actually answered, when observed. Differs from
    /// [`Self::specified_model`] on an in-family sibling switch, which the
    /// tuples above cannot express.
    #[serde(default)]
    pub realized_model: Option<String>,
    /// Whether a human re-pointed the seat after dispatch — the rescue itself,
    /// recorded beside what it changed.
    #[serde(default)]
    pub human_switch: bool,
}

impl SeatDrift {
    /// A one-line `specified ~> realized` rendering, at the finest granularity
    /// available — model where the roster pinned one, family otherwise.
    ///
    /// This is the string a reader of the verdict should see, so the switch is
    /// legible where the conclusion is read and not only in the pane where the
    /// operator happened to be looking at the time.
    #[must_use]
    pub fn render(&self) -> String {
        let specified = self
            .specified_model
            .clone()
            .unwrap_or_else(|| self.specified.family.clone());
        let realized = self.realized_model.clone().unwrap_or_else(|| {
            self.realized
                .as_ref()
                .map_or_else(|| "unobserved".to_string(), |ep| ep.family.clone())
        });
        let rescue = if self.human_switch {
            " (switched by hand after dispatch)"
        } else {
            ""
        };
        format!("{}: {specified} ~> {realized}{rescue}", self.seat_id)
    }
}

/// One roster seat that carried no review into the fold, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatNonDelivery {
    /// The seat that did not deliver.
    pub seat_id: String,
    /// Its liveness disposition.
    pub delivery: SeatDelivery,
}

/// Whether the jury that **actually sat** still meets the floor the roster was
/// planned against.
///
/// [`plan_committee`] checks the floor at *plan* time, over seats that have not
/// run yet. Nothing re-checked it afterwards — so a roster planned with a legal
/// floor and then hollowed out by a mid-review provider refusal still folded
/// through the plain conjunctive door as though the jury were intact. This is
/// the re-check on the delivered roster, computed on **realized** endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JuryIntegrity {
    /// The floor the roster was planned against.
    pub required_families: usize,
    /// Distinct families among the generator and the seats that *delivered*
    /// **and whose realized endpoint was observed**.
    ///
    /// A seat with no observed endpoint contributes nothing here — it is
    /// listed in [`Self::unobserved`] instead. Counting it on its *specified*
    /// tuple would score an unknown as compliant. The generator is included on
    /// the same terms as every reader: observed or nothing.
    pub delivered_families: usize,
    /// Whether the delivered jury still spans the floor. `false` forbids
    /// [`CommitteeVerdict::Confirmed`] — the jury is not intact, so it has
    /// nothing to confirm *with*.
    pub floor_met: bool,
    /// Roster seats that carried no review, with their typed reasons. Never
    /// empty when a seat was refused or stalled — this is the trace a silent
    /// human rescue does not leave. Non-empty forbids
    /// [`CommitteeVerdict::Confirmed`] via [`Self::is_intact`], floor or no
    /// floor: a jury that ran a seat short is not the jury that was convened.
    pub non_delivering: Vec<SeatNonDelivery>,
    /// Seats whose realized endpoint diverged from the specified one, or that a
    /// hand re-pointed after dispatch. Non-empty forbids
    /// [`CommitteeVerdict::Confirmed`] via [`Self::is_intact`] even when the
    /// floor survives the divergence.
    pub drift: Vec<SeatDrift>,
    /// Seats whose **realized endpoint was never observed** although they took
    /// part — the third disposition, distinct from both "delivered on a known
    /// endpoint" and "did not deliver". The generator appears here on exactly
    /// the same terms as a reader: it seeds the reference family, so an
    /// unwatched generator withholds a family like any other seat.
    ///
    /// Such a seat reviewed, so its verdict still counts in the conjunctive
    /// door; but nobody watched which weights answered, so it may not be
    /// credited with a family. That asymmetry is the point: an unobserved seat
    /// can still *refute*, and can never *certify diversity*. Non-empty here
    /// costs the round its `Confirmed` twice over: it withholds the family, so
    /// the floor may fall; and it is one of the four conditions
    /// [`Self::is_intact`] requires, so [`fold_committee`] withholds
    /// certification even where the observed seats span the floor without it.
    #[serde(default)]
    pub unobserved: Vec<String>,
}

impl JuryIntegrity {
    /// Whether every roster seat delivered, on an observed endpoint, with no
    /// drift and no hand-rescue, over a met floor — the only shape in which the
    /// jury's diversity claim is fully attested rather than partly assumed.
    ///
    /// Drift counts against intactness even when the floor survives it. A jury
    /// that needed three human rescues in two days and reports itself intact is
    /// the failure this whole re-check exists to close: the roster describes a
    /// jury that did not sit, and *that* is the fact a reader must not have to
    /// go looking for.
    ///
    /// This is a **control, not a caveat**: [`fold_committee`] consults it, and
    /// a fold that is not intact cannot return
    /// [`CommitteeVerdict::Confirmed`]. It may still return
    /// [`CommitteeVerdict::Refuted`] — a compromised jury may refute, it may
    /// not certify.
    #[must_use]
    pub fn is_intact(&self) -> bool {
        self.floor_met
            && self.non_delivering.is_empty()
            && self.unobserved.is_empty()
            && self.drift.is_empty()
    }

    /// The seats a human re-pointed after dispatch, rendered
    /// `specified ~> realized` — the trace of every in-pane rescue, available
    /// where the verdict is read.
    ///
    /// Empty is the load-bearing case: it means nobody reached into a pane, and
    /// it is only trustworthy because the caller reports the rescue explicitly
    /// ([`SeatOutcome::switched_after_dispatch`]) rather than the fold trying
    /// to infer it from a tuple that a sibling switch leaves unchanged.
    #[must_use]
    pub fn hand_rescues(&self) -> Vec<String> {
        self.drift
            .iter()
            .filter(|d| d.human_switch)
            .map(SeatDrift::render)
            .collect()
    }
}

/// Re-check the family floor over the jury that **actually delivered**, using
/// each seat's realized endpoint where one was observed.
///
/// Two independent ways a planned-legal roster becomes an illegal one by the
/// time it votes, both measured in the field:
///
/// 1. **A seat stops answering.** A provider-side refusal mid-review, a stall,
///    a seat never dispatched. It contributed no review, so it contributes no
///    family — whatever the plan said it would.
/// 2. **A seat answers as something else.** An operator switches a stalled seat
///    to a sibling model to unblock the round; the roster keeps reporting the
///    *specified* model while the *realized* one is what answered. If the
///    sibling resolves into a family a peer already covers, the jury has
///    silently lost an axis while still showing two seats.
///
/// # Unknown is not a synonym for compliant
///
/// There is a third state, and it is the one that is easy to get wrong: the
/// realized endpoint was **never observed** ([`SeatOutcome::realized_endpoint`]
/// is `None`). Falling back to the seat's specified tuple there would let a
/// seat that nobody watched be counted as a distinct family — absence of
/// evidence promoted to evidence of diversity, which is precisely the failure
/// (2) is about. So an unobserved seat contributes **no family** and is
/// recorded in [`JuryIntegrity::unobserved`]. Its verdict still counts (it
/// reviewed); only its *diversity* claim is withheld. Where the floor then
/// cannot be met from observed endpoints alone, the honest verdict is
/// [`CommitteeVerdict::Inconclusive`] — the same rule that governs a seat which
/// could not execute.
///
/// # The generator's endpoint is scored on the same terms
///
/// The generator seeds the reference family, and it earns that family the same
/// way every other seat does: by having been **observed**. If `outcomes`
/// carries an entry for [`RosterPlan::generator`]'s seat id with a realized
/// tuple, that tuple is used and any drift recorded, exactly as for a seat. If
/// no observation reached this function — no outcome at all, or an outcome
/// whose `realized_endpoint` is `None` — the generator contributes **no
/// family** and is listed in [`JuryIntegrity::unobserved`].
///
/// The earlier reading, that the plan-time tuple may stand for the generator as
/// a stated residual, was the last surviving instance of the fallback this
/// module removed everywhere else. It cannot survive its own argument: a
/// generator that silently drifted onto a reader's family deflates the true
/// count by one, and scoring the plan tuple hides exactly that. A caller that
/// wants the generator's family to hold up the floor must observe it, which is
/// the same price every reader pays.
///
/// Outcomes for seats that are neither on [`RosterPlan::admitted`] nor the
/// generator are ignored here: a witness-rejected seat was never on the ballot,
/// so its review cannot hold up a floor it was excluded from.
/// The drift record for one seat, if the jury that sat diverged from the roster
/// that was read — on **any** of the three axes that can diverge.
///
/// All three are independent, and only the first was ever checked:
///
/// 1. The **endpoint tuple** moved (a different family or URL answered). This
///    is the one that changes the floor.
/// 2. The **model pin** moved inside one family (`gpt-5.6-sol ~> gpt-5.6-terra`).
///    The tuples are identical, so axis 1 sees nothing — and this is the switch
///    the field actually produces, three times in two days on one seat.
/// 3. A human **re-pointed the seat after dispatch**, whether or not that
///    changed anything measurable. The rescue is itself the fact: a jury that
///    needed hands to produce a verdict is not the jury the roster describes.
fn seat_drift(seat: &AdmittedSeat, outcome: &SeatOutcome) -> Option<SeatDrift> {
    let endpoint_moved = outcome
        .realized_endpoint
        .as_ref()
        .is_some_and(|realized| realized != &seat.endpoint);
    let model_moved = outcome
        .realized_model
        .as_ref()
        .is_some_and(|realized| Some(realized) != seat.model.as_ref());
    if !endpoint_moved && !model_moved && !outcome.switched_after_dispatch {
        return None;
    }
    Some(SeatDrift {
        seat_id: seat.seat_id.clone(),
        specified: seat.endpoint.clone(),
        realized: outcome.realized_endpoint.clone(),
        specified_model: seat.model.clone(),
        realized_model: outcome.realized_model.clone(),
        human_switch: outcome.switched_after_dispatch,
    })
}

#[must_use]
pub fn jury_integrity(plan: &RosterPlan, outcomes: &[SeatOutcome]) -> JuryIntegrity {
    let by_seat: BTreeMap<&str, &SeatOutcome> =
        outcomes.iter().map(|o| (o.seat_id.as_str(), o)).collect();

    let mut non_delivering: Vec<SeatNonDelivery> = Vec::new();
    let mut drift: Vec<SeatDrift> = Vec::new();
    let mut unobserved: Vec<String> = Vec::new();

    // The generator's family, taken from its observation when the caller
    // supplied one and from the plan otherwise (see the ceiling above).
    let generator_outcome = by_seat.get(plan.generator.seat_id.as_str()).copied();
    if let Some(outcome) = generator_outcome {
        drift.extend(seat_drift(&plan.generator, outcome));
    }
    let mut families: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    match generator_outcome.and_then(|o| o.realized_endpoint.as_ref()) {
        Some(realized) => {
            families.insert(realized.family.clone());
        }
        // The generator is not a special case. Falling back to `plan.generator
        // .endpoint` here would score the seat that *proposes* on an endpoint
        // nobody watched — the very move this function refuses for every
        // admitted seat two dozen lines below. Absence of an observation is
        // absence of an observation wherever it occurs, so the generator's
        // family is withheld and the seat is recorded as unobserved.
        None => unobserved.push(plan.generator.seat_id.clone()),
    }

    for seat in &plan.admitted {
        let Some(outcome) = by_seat.get(seat.seat_id.as_str()) else {
            non_delivering.push(SeatNonDelivery {
                seat_id: seat.seat_id.clone(),
                delivery: SeatDelivery::NotDispatched,
            });
            continue;
        };
        if !outcome.delivery.delivered() {
            non_delivering.push(SeatNonDelivery {
                seat_id: seat.seat_id.clone(),
                delivery: outcome.delivery,
            });
            continue;
        }
        // The record first, and unconditionally: a hand-switched seat leaves a
        // trace whether or not anyone wrote down where it landed.
        drift.extend(seat_drift(seat, outcome));

        // Then the score. Diversity is a property of what answered, and an
        // unobserved endpoint is not a quiet "same as rostered" — it is a seat
        // whose family nobody can vouch for, so it holds up no part of the
        // floor.
        let Some(realized) = &outcome.realized_endpoint else {
            unobserved.push(seat.seat_id.clone());
            continue;
        };
        families.insert(realized.family.clone());
    }

    let delivered_families = families.len();
    let floor_met =
        !plan.requirement.required || delivered_families >= plan.requirement.min_distinct_families;

    JuryIntegrity {
        required_families: plan.requirement.min_distinct_families,
        delivered_families,
        floor_met,
        non_delivering,
        drift,
        unobserved,
    }
}

/// The committee's verdict together with the integrity of the jury that
/// produced it — the two facts a caller must never read apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitteeFold {
    /// The aggregate verdict, already gated on jury integrity.
    pub verdict: CommitteeVerdict,
    /// The delivered jury the verdict was folded from.
    pub integrity: JuryIntegrity,
}

/// Fold seat outcomes into a verdict **that a hollowed-out jury cannot
/// confirm** — the integrity-aware door callers should use.
///
/// The order of the three rules is the whole design:
///
/// 1. **A red is still decisive**, whoever raised it, delivered roster or not.
///    A broken jury never launders a refutation into a pass; fail-closed beats
///    tidiness.
/// 2. Otherwise the plain conjunctive door runs over the **delivered** outcomes
///    only, so a non-delivering seat neither confirms nor is quietly skipped.
/// 3. **A jury that is not [`JuryIntegrity::is_intact`] may not certify.** A
///    `Confirmed` from rule 2 is downgraded to [`CommitteeVerdict::Inconclusive`]
///    whenever the delivered floor is unmet, a roster seat carried no review, a
///    seat's endpoint went unobserved, or a hand re-pointed a pane mid-round.
///    `Inconclusive` is the honest verdict there: not "clean," not "refuted,"
///    *no jury we can stand behind*.
///
/// Rule 3 is what the loop needs to stop reporting CLEAN-and-CLEAN over a jury
/// that was, structurally, one family and one reader. It is also what makes
/// `is_intact` a **control** rather than a caveat: before it was wired here the
/// predicate recorded the compromise faithfully and nothing read it, so a fold
/// rescued by hand mid-review still certified while carrying its own admission
/// that it had been. A flag no decision consults is documentation.
///
/// The asymmetry is deliberate and matches the seat-level rule: a compromised
/// jury may still **refute** — rule 1 runs first, and fail-closed beats tidiness
/// — but it may never **certify**.
#[must_use]
pub fn fold_committee(plan: &RosterPlan, outcomes: &[SeatOutcome]) -> CommitteeFold {
    let integrity = jury_integrity(plan, outcomes);

    // Rule 1 — any red, from any seat, refutes. Checked before integrity so a
    // broken jury cannot bury a concrete refutation under "inconclusive."
    if outcomes
        .iter()
        .any(|o| o.verdict == SeatVerdict::Refuted || o.falsifier_red)
    {
        return CommitteeFold {
            verdict: CommitteeVerdict::Refuted,
            integrity,
        };
    }

    // Rule 2 — the plain door, over the seats that actually reviewed.
    let delivered: Vec<SeatOutcome> = outcomes
        .iter()
        .filter(|o| o.delivery.delivered())
        .cloned()
        .collect();
    let door = committee_verdict(&delivered);

    // Rule 3 — and the jury has to have been a jury. A door that says
    // `Confirmed` over a roster that lost a seat, an observation or an axis is
    // certifying on behalf of a body that did not sit as described.
    let verdict = if door == CommitteeVerdict::Confirmed && !integrity.is_intact() {
        CommitteeVerdict::Inconclusive
    } else {
        door
    };

    CommitteeFold { verdict, integrity }
}

/// Basename of the **machine-readable** roster a committee writes beside its
/// prose `roster.md`, and the file the `cs reconcile --check` witness lint
/// reads.
///
/// # Why a second file, when `roster.md` already exists
///
/// `roster.md` is written for a human and holds the two witness tables as
/// prose. A gate cannot refuse prose. For a while that was the whole gap: the
/// witnesses in this module were exercised only by their own unit tests, which
/// all passed while changing nothing, because **no production caller consulted
/// them** — verified by grep on 2026-07-28, zero callers of [`plan_committee`],
/// [`committee_requirement`], [`fold_committee`], [`jury_integrity`],
/// [`sor_may_not_resurrect`] or [`RosterPlan::floor_bearing_seats`] anywhere
/// outside this file. A roster that failed a witness was discouraged by a
/// recipe and contradicted by nothing.
///
/// This file is what makes the witnesses decidable by a tool. The convene step
/// writes it; `cs reconcile --check` refuses the roster that fails.
pub const COMMITTEE_ROSTER_FILE: &str = "roster.json";

/// The formula ids whose molecules convene a committee, and therefore OWE a
/// [`COMMITTEE_ROSTER_FILE`].
///
/// # Why the gate asks the formula and not the directory
///
/// "Is this molecule a committee?" was answered by looking for artefacts a
/// convener had to choose to write: `roster.json`, then the prose `roster.md`,
/// then a seat's `committee-posture.md`. Every one of those makes the gate
/// **opt-in by the party it constrains** — a convener who writes none of them
/// is never inspected, and the honest remaining scope was "the gate cannot
/// refuse what leaves no trace anywhere".
///
/// But it does leave a trace. `cs nucleate` records `formula_id` in the
/// molecule's own `state.json` before any worker touches it, and no convener
/// hand-edits it into being something else. So the question is answered by
/// RESOLUTION — what this molecule *is* — rather than by DECLARATION — what
/// its author chose to write down. A live molecule on this list with no
/// roster is refused whether or not it left any other trace.
///
/// Kept as a list rather than a substring match: a formula named
/// `committee-retrospective` is not a convener, and a gate that guesses from a
/// name is the name-as-axis mistake this module exists to refuse.
pub const CONVENING_FORMULA_IDS: [&str; 1] = ["cross-provider-committee"];

/// The molecule variable a committee writes to pin **which tree** a seat is to
/// review.
///
/// # Why the tree and not the commit
///
/// A commit sha names a point in history; a tree names the *bytes under
/// review*, which is what a reviewer's verdict is actually about. Two commits
/// legitimately carry one tree — a branch tip and main's merge of it are the
/// canonical pair — so pinning the sha collapses a seat for sitting on the
/// merge commit of the very work it reviews. That refusal would itself be an
/// instance of measuring the LABEL instead of the PROPERTY, which is the class
/// this pin exists to close.
pub const REVIEWED_TREE_VAR: &str = "reviewed_tree";

/// What a seat's reviewed-tree pin resolves to against the bases actually
/// available at dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewedTreeResolution {
    /// No pin on this molecule — dispatch is unconstrained, exactly as before.
    NotPinned,
    /// A base carrying the pinned tree was found; cut the worktree from it.
    Resolved {
        /// The git ref / rev to use as the worktree's start point.
        start_point: String,
        /// The full tree id it carries — the pin, resolved.
        tree: String,
    },
    /// The pin itself is not a tree id. Refused rather than ignored: a pin a
    /// reader cannot interpret enforces nothing while looking like it does.
    Unreadable {
        /// The value found on the molecule.
        pin: String,
    },
    /// No available base carries the pinned tree, so the seat cannot be put on
    /// the artefact it was convened to review.
    Unsatisfiable {
        /// The pin, as declared.
        pin: String,
        /// Every `(ref, tree)` pair that was offered and did not match.
        offered: Vec<(String, String)>,
    },
}

/// Resolve a seat's reviewed-tree pin against the bases a dispatcher can
/// actually cut a worktree from.
///
/// # Why this exists
///
/// The committee writes the reviewed head/tree into each seat's contract and
/// `cs tackle` built the seat's worktree from a base that was **not** it: the
/// seat inherited the HEAD of the worktree the operator happened to tackle
/// *from*. The pin was a DECLARATION, the worktree was the RESOLUTION, and
/// nothing reconciled them — the same shape as every other finding in this
/// lineage, arriving in the dispatch machinery rather than in a lint.
///
/// Measured 2026-07-28 on `converge-20260728-7161`: same command, same pin, two
/// dispatch cwds. Tackled from a worktree at `5198a39` the seat got tree
/// `e7c8a521` and collapsed on the mismatch; tackled from main at `95243fb` it
/// got tree `4a25558e` and ran. Worse than a stalled seat, it corrupted
/// artefacts silently — a sibling seat's `verdict.json` DECLARED the reviewed
/// tree it had been pinned to while its worktree sat on another, which is a
/// claimed measurement never made, in the file the release gate reads.
///
/// The refusal is the load-bearing half. Choosing a matching base is a
/// convenience; refusing to launch when none carries the pinned tree is what
/// makes the pin a constraint rather than decoration — the same shape as the
/// incoherent-pair gate that correctly refused an adapter/model mismatch in
/// that very run.
///
/// Prefix matching is deliberate and bounded: a contract written by a human
/// carries an abbreviated tree id (`4a25558e446d`), and a pin shorter than
/// [`MIN_REVIEWED_TREE_PIN`] characters is [`ReviewedTreeResolution::Unreadable`]
/// rather than a wildcard that matches half the repository.
///
/// `candidates` is ordered by preference and consulted in order, so a
/// dispatcher's own first choice wins when several bases carry the same tree —
/// which is the ordinary case, since a merge and its branch tip agree.
///
/// # Examples
///
/// ```
/// use cosmon_core::committee::{resolve_reviewed_tree, ReviewedTreeResolution};
///
/// let bases = vec![
///     ("HEAD".to_string(), "e7c8a521fe29aa11bb22cc33dd44ee55ff667788".to_string()),
///     ("main".to_string(), "4a25558e446dbfe76e2e81a6968285fe1eea3981".to_string()),
/// ];
///
/// // The abbreviated pin from the contract finds main, not the ambient HEAD.
/// assert!(matches!(
///     resolve_reviewed_tree(Some("4a25558e446d"), &bases),
///     ReviewedTreeResolution::Resolved { ref start_point, .. } if start_point == "main",
/// ));
///
/// // And a tree nothing carries refuses, rather than silently reviewing
/// // whatever happened to be checked out.
/// assert!(matches!(
///     resolve_reviewed_tree(Some("deadbeefcafe"), &bases),
///     ReviewedTreeResolution::Unsatisfiable { .. },
/// ));
/// ```
#[must_use]
pub fn resolve_reviewed_tree(
    pin: Option<&str>,
    candidates: &[(String, String)],
) -> ReviewedTreeResolution {
    let Some(pin) = pin.map(str::trim).filter(|p| !p.is_empty()) else {
        return ReviewedTreeResolution::NotPinned;
    };
    let pin = pin.to_ascii_lowercase();
    if pin.len() < MIN_REVIEWED_TREE_PIN
        || pin.len() > 40
        || !pin.chars().all(|c| c.is_ascii_hexdigit())
    {
        return ReviewedTreeResolution::Unreadable { pin };
    }
    for (start_point, tree) in candidates {
        if tree.to_ascii_lowercase().starts_with(&pin) {
            return ReviewedTreeResolution::Resolved {
                start_point: start_point.clone(),
                tree: tree.clone(),
            };
        }
    }
    ReviewedTreeResolution::Unsatisfiable {
        pin,
        offered: candidates.to_vec(),
    }
}

/// The shortest abbreviated tree id a reviewed-tree pin may use.
///
/// Git's own default abbreviation is 7; anything shorter is a prefix that
/// matches by luck, and a pin that matches by luck is not a pin.
pub const MIN_REVIEWED_TREE_PIN: usize = 7;

/// The formulas whose molecules **sit as seats**, and therefore owe the
/// two-file verdict emission ([`read_seat_emission`]).
///
/// The sibling of [`CONVENING_FORMULA_IDS`], one layer down and for the same
/// reason. A verdict lint scoped to artefacts can only judge a seat that
/// *spoke*: a seat emitting nothing leaves nothing to be wrong about, so
/// absence reads as absence-of-a-seat rather than as a seat that said nothing.
/// Measured 2026-07-28 — a missing `verdict.json` exited the lint through a
/// bare `continue` while a malformed one was refused, and the contract's
/// central rule (*a missing verdict is NOT-CLEAN, never CLEAN*) had no code
/// enforcement at all.
///
/// `formula_id` is written by `cs nucleate` before any worker runs, so it says
/// what the molecule IS rather than what its author chose to write down. That
/// is what lets an absent verdict be judged without refusing every ordinary
/// molecule in the tree for not being a seat.
pub const SEAT_FORMULA_IDS: [&str; 1] = ["cmb-verify"];

/// A committee roster as a convener **declares** it — the machine-readable
/// form of the recipe's `roster.md`, and the only thing a gate can refuse.
///
/// Deserialized from [`COMMITTEE_ROSTER_FILE`] in a molecule directory. Every
/// field the dual witness needs is here and nothing else: the stake that sets
/// the diversity floor, the generator the roster is diverse *against*, and the
/// refuter candidates in the order they are to be admitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterSpec {
    /// The effective criticality of the work under review. Sets the stake floor
    /// (`root` → 2 distinct families, `security`/`max` → 3) that
    /// `[provider_bias]` may raise and never lower.
    pub stake: CriticalityLevel,
    /// Whether a committee is required at all — the fleet/spore policy input,
    /// never inferred from criticality. Defaults to `true`, because a file
    /// named `roster.json` exists precisely because a committee is being
    /// convened; declaring `false` is how a convener records "no committee
    /// required here" without deleting the roster.
    #[serde(default = "default_cross_provider")]
    pub cross_provider: bool,
    /// The generator seat — the molecule whose diagnosis, fix and falsifier are
    /// the artefacts under audit.
    pub generator: SeatCandidate,
    /// The refuter candidates, in admission order. Order is load-bearing and
    /// deterministic: on a collision the first claimant is admitted and the
    /// second rejected, so the same file always yields the same roster.
    #[serde(default)]
    pub refuters: Vec<SeatCandidate>,
}

/// A `roster.json` exists because a committee is being convened.
const fn default_cross_provider() -> bool {
    true
}

impl RosterSpec {
    /// Plan this roster against the project's `[provider_bias]` floor.
    ///
    /// A thin composition of [`committee_requirement`] and [`plan_committee`],
    /// named so a caller cannot accidentally derive the requirement one way and
    /// the roster another.
    #[must_use]
    pub fn plan(&self, bias: &ProviderBiasConfig) -> RosterPlan {
        let requirement = committee_requirement(self.stake, bias, self.cross_provider);
        plan_committee(&self.generator, &self.refuters, requirement)
    }

    /// This roster with every seat's `injected` flag **re-derived from what is
    /// on disk**, plus one line per seat whose claim did not survive it.
    ///
    /// # `injected: true` is a claim, exactly like a declared family
    ///
    /// The recipe tells a convener to "flip that seat's
    /// `persona.briefing.injected` to true once the durable file exists". That
    /// makes the load-bearing field of witness (2) a self-declaration — the
    /// same defect the [`Self::resolved`] step closes on witness (1). A roster
    /// that simply types `true` passed.
    ///
    /// `observe` is the injected port (the core stays I/O-free): given a seat
    /// id it returns the [`ObservedDelivery`]
    /// [`AdversarialBriefing::from_durable_injection`] needs — the *parsed*
    /// [`COMMITTEE_POSTURE_FILE`] in that seat's own molecule directory, and
    /// whether its `briefing.md` carries the pointer at it. `None` means the
    /// seat has no directory yet — a planned seat at convene, before
    /// nucleation.
    ///
    /// # Presence was not content, and this is where that was fixed
    ///
    /// The posture leg used to be `file.exists()`. A seat whose contract file
    /// held `# posture\n` — no version, no hash, no body — was certified as
    /// having received an adversarial contract, which is the gate passing while
    /// the constrained party says something EMPTY. The leg is now a comparison:
    /// the file must parse as a rendered contract, and its header's
    /// `contract-version` and `contract-hash` must be the ones this seat's
    /// roster entry declares.
    ///
    /// # Which party this constrains, and which it does not
    ///
    /// Say it plainly, because a hash matched against a claim by the same
    /// author is self-attestation wearing a checksum.
    ///
    /// - **Constrained: the SEAT.** `roster.json` lives in the *convener's*
    ///   molecule directory; `committee-posture.md` lives in the seat's own,
    ///   where the seat's worker can write. A seat that truncates, empties,
    ///   stubs or swaps its contract now breaks a comparison against a file it
    ///   does not own, and is refused. Under presence-only it was not.
    /// - **NOT constrained: the CONVENER.** It authors both artefacts, so a
    ///   convener that renders a fabricated body under a self-consistent header
    ///   still passes. This check cannot see that, and nothing inside one
    ///   party's own two files ever could.
    /// - **Now checked: that the hash content-addresses the body.** This was
    ///   the last self-attested leg — the hash was compared against the
    ///   roster's copy of itself and never against the prose it claims to
    ///   address. It is now recomputed ([`verify_contract_hash`]), so a seat
    ///   whose declared digest is not its body's digest is refused. The
    ///   justification that previously licensed the omission asserted that
    ///   verification "would refuse every committee convened to date"; measured
    ///   on 2026-08-01 across the 29 live contracts, 20 already verify and the
    ///   9 that do not are digest-shaped fabrications, including one value
    ///   shared verbatim by three different contracts. See
    ///   [`verify_contract_hash`] for the full measurement.
    ///
    /// # `None` is not a pass
    ///
    /// A seat with no directory that claims nothing is genuinely not a
    /// divergence, and is left carrying whatever the roster said so the
    /// ordinary `briefing-not-injected` line fires. But a seat with no
    /// directory that claims `injected: true` is the *strongest* contradiction
    /// this witness can observe: it asserts two files inside a directory that
    /// does not exist. Reading `None` as "leave the claim alone" therefore left
    /// self-attestation intact precisely where delivery was impossible, and is
    /// reported as its own violation.
    #[must_use]
    pub fn with_observed_delivery(
        &self,
        observe: impl Fn(&str) -> Option<ObservedDelivery>,
    ) -> (Self, Vec<String>) {
        let mut out = Vec::new();
        let mut observe_seat = |seat: &SeatCandidate| -> SeatCandidate {
            let mut seat = seat.clone();
            let Some(claimed) = seat.persona.briefing.clone() else {
                return seat;
            };
            let Some(observed) = observe(&seat.seat_id) else {
                // No directory. Nothing was claimed *about a molecule that does
                // not exist* — unless it was, and then the absence is the
                // sharpest contradiction available, not the weakest. `injected:
                // true` asserts two files exist inside a directory that does
                // not; delivery is being claimed exactly where it CANNOT have
                // occurred. Returning the seat unchanged here left that one
                // state self-attested, which is the shape the whole witness
                // exists to refuse. Measured 2026-07-28: a roster declaring
                // `injected: true` for a seat with no directory exited 0.
                if claimed.injected {
                    out.push(format!(
                        "seat '{}' claims `injected: true` but has no molecule \
                         directory at all — there is no `{COMMITTEE_POSTURE_FILE}` \
                         and no briefing.md because there is nowhere for them to \
                         be. Delivery cannot be claimed for a molecule that was \
                         never nucleated: nucleate the seat and let `cs tackle` \
                         write the pointer, or say `injected: false` until it is",
                        seat.seat_id,
                    ));
                    seat.persona.briefing = Some(AdversarialBriefing::from_durable_injection(
                        claimed.version,
                        claimed.contract_hash,
                        false,
                        false,
                    ));
                }
                // A seat claiming nothing is a planned seat at convene: no
                // claim, nothing to contradict, and the ordinary
                // `briefing-not-injected` line is the honest finding.
                return seat;
            };
            // The posture leg is a COMPARISON, not a head-count: the file must
            // parse as a rendered contract AND declare the version and hash
            // this seat's roster entry declares.
            let posture_verdict = match &observed.posture {
                Some(found)
                    if found.version == claimed.version
                        && found.contract_hash == claimed.contract_hash =>
                {
                    // The header matching the roster is an agreement between
                    // two parties. Whether the hash addresses the prose
                    // beneath it is a property of ONE file, and no amount of
                    // agreement between the two could establish it — which is
                    // why it is asked separately, and last.
                    verify_contract_hash(&found.contract_hash, &found.body)
                        .refusal()
                        .map(|refusal| {
                            format!(
                                "`{COMMITTEE_POSTURE_FILE}` declares the contract \
                                 the roster declares, but that contract-hash does \
                                 not hold up: {refusal}"
                            )
                        })
                }
                Some(found) => Some(format!(
                    "`{COMMITTEE_POSTURE_FILE}` is NOT the contract this seat was \
                     rostered under — the file declares contract-version {} / \
                     contract-hash `{}`, the roster declares {} / `{}`",
                    found.version, found.contract_hash, claimed.version, claimed.contract_hash,
                )),
                None if observed.posture_file_exists => Some(format!(
                    "`{COMMITTEE_POSTURE_FILE}` exists but is not a rendered \
                     adversarial contract — it carries no contract-version line, \
                     no contract-hash line, or no body at all. A placeholder at \
                     the contract's path is not the contract: presence is not \
                     content"
                )),
                None => Some(format!("`{COMMITTEE_POSTURE_FILE}` is MISSING")),
            };
            let derived = AdversarialBriefing::from_durable_injection(
                claimed.version,
                claimed.contract_hash.clone(),
                posture_verdict.is_none(),
                observed.pointer,
            );
            if claimed.injected && !derived.injected {
                out.push(format!(
                    "seat '{}' claims `injected: true` but the two-fact test does \
                     not hold on disk: {}; and its briefing.md {} the pointer at \
                     it. Delivery is a fact about files, not a field a convener \
                     sets — flipping the flag is the paper contract this witness \
                     exists to refuse",
                    seat.seat_id,
                    posture_verdict.unwrap_or_else(|| format!(
                        "`{COMMITTEE_POSTURE_FILE}` is the declared contract"
                    )),
                    if observed.pointer {
                        "carries"
                    } else {
                        "does NOT carry"
                    },
                ));
            }
            seat.persona.briefing = Some(derived);
            seat
        };
        let generator = observe_seat(&self.generator);
        let refuters: Vec<SeatCandidate> = self.refuters.iter().map(&mut observe_seat).collect();
        // The closure borrows `out` mutably; end that borrow before `out` is
        // moved into the return value.
        let _ = observe_seat;
        (
            Self {
                stake: self.stake,
                cross_provider: self.cross_provider,
                generator,
                refuters,
            },
            out,
        )
    }

    /// This roster with every seat's family witness **re-derived** from the
    /// project's `[adapters]` inventory, plus one line per seat whose
    /// declaration did not survive the derivation.
    ///
    /// # Why the declared tuple cannot be planned directly
    ///
    /// [`Self::plan`] reads [`SeatCandidate::family`] — a field the convener
    /// *wrote*. Planning it measures whether the roster's own prose is
    /// internally consistent, which is the property next to the one that
    /// matters. A roster could name family `openai` for a seat whose
    /// `[adapters.…]` section sets `base_url = "https://api.anthropic.com"`,
    /// declare a second family it does not have, and pass every witness. That
    /// is the proxy-costume, unopposed.
    ///
    /// So the gate plans **this** spec, not the declared one:
    /// [`FamilyWitness::resolve`] derives each tuple from `base_url` + `model`
    /// exactly as `cs tackle` will at dispatch, and a divergence between what
    /// was claimed and what resolves is reported as its own violation rather
    /// than silently corrected. A seat naming no adapter is reported too — an
    /// unresolvable claim is not a claim a gate has checked.
    ///
    /// # Unresolvable, in the two shapes it really takes
    ///
    /// A seat is refused when its adapter name is a **ghost** — in neither the
    /// in-code registry ([`crate::spawn_seam::built_in_adapter_names`]) nor the
    /// `[adapters]` inventory, so nothing could dispatch it — or when the name
    /// is dispatchable but **unresolvable**
    /// ([`crate::provider_diversity::endpoint_is_derived`]): nothing on the
    /// record other than the seat's own label feeds the derivation.
    ///
    /// Neither test is *"does this adapter have an `[adapters.<name>]`
    /// section?"*, which is the property next to the one that matters and was
    /// what this gate measured until 2026-07-28. `codex` dispatches with no
    /// section — the section is optional and only tunes launch mode — so that
    /// test refused a real, resolvable seat, and in a galaxy whose only
    /// non-generator family is reached through `codex` it refused the sole
    /// provider that would have supplied the diversity the gate exists to
    /// enforce.
    #[must_use]
    pub fn resolved(
        &self,
        adapters: Option<&crate::config::AdaptersConfig>,
    ) -> (Self, Vec<String>) {
        let mut out = Vec::new();
        let mut resolve_seat = |seat: &SeatCandidate| -> SeatCandidate {
            let mut seat = seat.clone();
            let Some(name) = seat.adapter.clone() else {
                out.push(format!(
                    "seat '{}' names no `adapter`, so its declared family \
                     (provider '{}', family '{}') resolves against nothing and is a \
                     SELF-ATTESTATION. A roster may not certify its own \
                     distinctness: name the `[adapters.<name>]` section this seat \
                     sits on so the tuple can be derived from base_url + model",
                    seat.seat_id, seat.family.endpoint.provider, seat.family.endpoint.family,
                ));
                return seat;
            };
            // Two different failures live here, and conflating them is what
            // the first version of this check got wrong.
            //
            // (i) A GHOST — a name nothing in the system answers to. It is in
            //     neither the in-code adapter registry nor the project's TOML
            //     inventory, so no dispatch could ever spawn it and its tuple
            //     would be derived from the seat's own NAME: `declared` equals
            //     `resolved` by construction and the mismatch check below
            //     cannot fire however the seat declares itself. Measured
            //     2026-07-28 with a seat named `ghostseat` declaring family
            //     `ghostseat` and no section anywhere: `cs reconcile --check`
            //     exited 0.
            //
            // (ii) A DISPATCHABLE BUT UNRESOLVABLE name — cosmon can spawn it,
            //     and still nothing on the record can contradict what the seat
            //     says about it (see `endpoint_is_derived`).
            //
            // The registry side of (i) is `spawn_seam::built_in_adapter_names`
            // — the same list `cs tackle` composes its dispatch registry from
            // and `cs adapters list` projects, consulted rather than copied so
            // there is one inventory and not two that drift.
            //
            // The check this replaces asked only *does this adapter have an
            // `[adapters.<name>]` section?*, which is the property NEXT TO the
            // one that matters. `AdaptersConfig` is populated only by
            // `[adapters.*]` TOML, while `codex` (and `claude`, `aider`,
            // `opencode`) dispatch with no section at all — the section is
            // optional and only tunes launch mode. So the gate refused a seat
            // cosmon really dispatches, and in a galaxy whose sole
            // non-generator family is reached through `codex` it refused the
            // one provider that would have supplied the diversity it exists to
            // enforce: no jury could be seated at all (measured 2026-07-28,
            // converge-20260728-7161 round 1, probes/round1-driver/).
            //
            // The remedy the old message prescribed was itself a
            // self-attestation: `codex` has no `base_url` and no `api_key_env`
            // — it shells out to the `codex` CLI — so any section written to
            // satisfy the gate is a fiction nothing verifies against the real
            // dispatch path. The fix is here, not in anyone's config.
            //
            // `adapters: None` (no `[adapters]` table at all) is not an
            // exemption: a built-in name still resolves, and everything else
            // still fails (i).
            let in_registry = crate::spawn_seam::built_in_adapter_names()
                .iter()
                .any(|b| *b == name);
            let in_inventory = adapters.is_some_and(|a| a.entry(&name).is_some());
            if !in_registry && !in_inventory {
                out.push(format!(
                    "seat '{}' names adapter '{name}', which cosmon cannot \
                     dispatch and cannot resolve: it is not a built-in adapter \
                     ({}) and there is no `[adapters.{name}]` section declaring \
                     it. Its tuple would be derived from the seat's own NAME, so \
                     its declared family (provider '{}', family '{}') can never \
                     be contradicted — a name that resolves against nothing is \
                     the same SELF-ATTESTATION as no name at all. Name an \
                     adapter that exists, or write the `[adapters.{name}]` \
                     section this seat really sits on",
                    seat.seat_id,
                    crate::spawn_seam::built_in_adapter_names().join(", "),
                    seat.family.endpoint.provider,
                    seat.family.endpoint.family,
                ));
                return seat;
            }
            if !crate::provider_diversity::endpoint_is_derived(adapters, &name) {
                out.push(format!(
                    "seat '{}' names adapter '{name}', which cosmon can dispatch \
                     but cannot RESOLVE: `[adapters.{name}]` declares neither \
                     `base_url` nor `default_model`, and the name itself belongs \
                     to no vendor lineage cosmon knows (unlike `codex` → openai \
                     or `claude` → anthropic, an adapter such as `aider` or \
                     `ollama` will serve whatever weights it is pointed at). So \
                     its tuple falls through to the seat's own NAME and its \
                     declared family (provider '{}', family '{}') can never be \
                     contradicted — the same SELF-ATTESTATION as naming no \
                     adapter at all. Declare `base_url` and/or `default_model` \
                     on `[adapters.{name}]` so the family is derived from the \
                     endpoint this seat really sits on",
                    seat.seat_id, seat.family.endpoint.provider, seat.family.endpoint.family,
                ));
                return seat;
            }
            let resolved = FamilyWitness::resolve(adapters, &name);
            if resolved.endpoint != seat.family.endpoint {
                out.push(format!(
                    "seat '{}' declares endpoint (provider '{}', base_url '{}', \
                     family '{}') but `[adapters.{name}]` RESOLVES to (provider \
                     '{}', base_url '{}', family '{}') — the roster is measured on \
                     the resolved tuple, never the declared one. Fix the \
                     declaration or point the adapter where the roster says it \
                     points",
                    seat.seat_id,
                    seat.family.endpoint.provider,
                    if seat.family.endpoint.base_url.is_empty() {
                        "<vendor default>"
                    } else {
                        &seat.family.endpoint.base_url
                    },
                    seat.family.endpoint.family,
                    resolved.endpoint.provider,
                    if resolved.endpoint.base_url.is_empty() {
                        "<vendor default>"
                    } else {
                        &resolved.endpoint.base_url
                    },
                    resolved.endpoint.family,
                ));
            }
            seat.family = resolved;
            seat
        };
        let generator = resolve_seat(&self.generator);
        let refuters: Vec<SeatCandidate> = self.refuters.iter().map(&mut resolve_seat).collect();
        // As above: release the mutable borrow of `out` before moving it.
        let _ = resolve_seat;
        (
            Self {
                stake: self.stake,
                cross_provider: self.cross_provider,
                generator,
                refuters,
            },
            out,
        )
    }

    /// Every reason this roster may not be convened, as human-readable lines —
    /// empty exactly when it may.
    ///
    /// The refusal half of [`Self::report`]; see that method for what is
    /// refused, what is merely reported, and why the two are not the same list.
    #[must_use]
    pub fn violations(
        &self,
        bias: &ProviderBiasConfig,
        adapters: Option<&crate::config::AdaptersConfig>,
    ) -> Vec<String> {
        self.report(bias, adapters).refusals
    }

    /// Every seat this roster names, generator first — the membership a reader
    /// auditing from `roster.json` alone would take as the committee.
    pub fn seats(&self) -> impl Iterator<Item = &SeatCandidate> {
        std::iter::once(&self.generator).chain(self.refuters.iter())
    }

    /// Reasons this roster's **membership** does not match who actually sat,
    /// given a way to ask whether a seat's molecule collapsed.
    ///
    /// # Why membership needs its own check
    ///
    /// The witnesses above measure a roster against the config and the seats'
    /// own artefacts. Neither notices that a named seat *never executed*: a
    /// collapsed molecule has no verdict to be incoherent with, no briefing to
    /// be uninjected, and its declared tuple resolves exactly as well as a live
    /// seat's. So a roster could name a collapsed floor-bearer forever and pass
    /// every other line here.
    ///
    /// Measured on `converge-20260728-7161`: the floor-bearing seat collapsed
    /// on a mismatched reviewed tree, was re-nucleated under a new id, and
    /// `roster.json` kept naming the collapsed one. The replacement appeared on
    /// no roster at all. Both facts were true and neither was refusable; the
    /// only record of who really sat was a paragraph in a ledger, which is
    /// precisely the declaration-without-resolution shape this whole lineage is
    /// about — arriving this time in the membership rather than in a field.
    ///
    /// The remedy makes the rebinding atomic in the only sense a file format
    /// can: the roster is REFUSED until it carries both halves. Removing the
    /// collapsed seat without recording the replacement leaves an unrostered
    /// seat (the CLI's second pass refuses that); recording the replacement
    /// without a reason is refused here; recording a reason for a seat that is
    /// still seated is refused here too. There is no ordering of the edit in
    /// which a reader of `roster.json` alone is misled about who carried the
    /// floor.
    ///
    /// `seat_collapsed` is injected rather than read, because this crate is
    /// I/O-free: the CLI answers it from each seat's `state.json`.
    #[must_use]
    pub fn reconvocation_violations(&self, seat_collapsed: &dyn Fn(&str) -> bool) -> Vec<String> {
        let mut out = Vec::new();
        let replacements: Vec<(&str, &str)> = self
            .seats()
            .filter_map(|s| {
                s.replaced_seat_id
                    .as_deref()
                    .map(|old| (s.seat_id.as_str(), old))
            })
            .collect();

        for seat in self.seats() {
            if seat_collapsed(&seat.seat_id) {
                if let Some((by, _)) = replacements.iter().find(|(_, old)| *old == seat.seat_id) {
                    out.push(format!(
                        "seat '{}' COLLAPSED and is still on the roster, while '{by}' \
                         records replacing it. A replaced seat is removed, not kept \
                         beside its replacement: two seats claiming one chair is the \
                         same unreadable membership as none",
                        seat.seat_id,
                    ));
                } else {
                    out.push(format!(
                        "seat '{}' is named on this roster and its molecule COLLAPSED \
                         — it never delivered a verdict, so a reader of `{}` alone \
                         would credit the floor to a seat that did not execute. Either \
                         remove it and give its replacement \
                         `\"replaced_seat_id\": \"{}\"` with a non-empty \
                         `replacement_reason`, or say here why a collapsed seat is \
                         still seated. Prose in a ledger is not a record",
                        seat.seat_id, COMMITTEE_ROSTER_FILE, seat.seat_id,
                    ));
                }
            }
            if let Some(old) = seat.replaced_seat_id.as_deref() {
                if old.trim().is_empty() {
                    out.push(format!(
                        "seat '{}' carries an EMPTY `replaced_seat_id` — a replacement \
                         of nothing. Name the seat it replaced or drop the field",
                        seat.seat_id,
                    ));
                }
                if seat
                    .replacement_reason
                    .as_deref()
                    .is_none_or(|r| r.trim().is_empty())
                {
                    out.push(format!(
                        "seat '{}' records replacing '{old}' with no \
                         `replacement_reason`. A re-convocation must say WHY the \
                         predecessor did not deliver: a jury failure does not consume \
                         a round and a quality refusal does, and an unstated cause is \
                         how the second is laundered into the first",
                        seat.seat_id,
                    ));
                }
                if old == seat.seat_id {
                    out.push(format!(
                        "seat '{}' records replacing ITSELF — a re-convocation seats a \
                         different molecule, so the two ids may never be equal",
                        seat.seat_id,
                    ));
                }
            }
        }
        out
    }

    /// The roster's full reading: what refuses it, and what is loud about it
    /// without refusing it.
    ///
    /// # What counts as a refusal
    ///
    /// - **A witness-rejected seat**, with one exception named below: a persona
    ///   collision, a missing injected briefing, a missing falsification
    ///   artefact. A rejected seat is not a warning to weigh, it is a seat that
    ///   may not sit.
    /// - **An unreachable family floor.** No dispatch could lift this roster to
    ///   the diversity its stake requires, so it cannot be convened as written.
    ///
    /// # What does NOT refuse, and why
    ///
    /// A roster whose floor rests on a single seat
    /// ([`RosterPlan::floor_is_single_point_of_failure`]) is **fragile, not
    /// illegal**. Refusing it would forbid the only roster many galaxies can
    /// actually wire, so it is reported as a loud line the convener must carry
    /// into `roster.md` — the recipe's own rule — rather than as a refusal
    /// here. The distinction is the honest one: the gate refuses what is
    /// invalid and names what is merely brittle.
    ///
    /// # The layer that reports may not overrule the layer that computes
    ///
    /// A [`SeatRejection::FamilyCollision`] on a roster whose floor is still
    /// **reachable** is an advisory, not a refusal. Until 2026-07-28 every
    /// entry of `plan.rejected` became a violation unconditionally, and the
    /// consequence was that the doctrine's own prescribed roster had no
    /// admissible representation anywhere in the system:
    /// [`plan_committee`] models a same-family second reader as admissible
    /// (`floor_met`, `floor_is_single_point_of_failure`, asserted by
    /// `the_prescribed_roster_seats_two_but_only_one_bears_the_floor`) and this
    /// method then refused the very roster the kernel had just called fine.
    /// Measured with a two-jaw pincer: the reader ON the ballot earned a
    /// family-collision refusal, the same reader OFF the ballot but seated
    /// earned an unrostered-seat refusal, and there was no third state.
    ///
    /// The distinction being drawn is *rejected from the ballot* versus
    /// *inadmissible in the tree*. A colliding seat is off the ballot — it is
    /// an echo, its verdict may not carry the floor, and `plan.admitted` has
    /// always said so. Whether the ROSTER is illegal is a different question,
    /// and it is answered by the floor: when the collision is what costs the
    /// roster its diversity, `floor_reachable` is false and the seat's
    /// collision is reported as a refusal beside the floor line that names the
    /// consequence. So a family collision can still refuse — it is not
    /// decoration — but it refuses for costing the floor, never for existing.
    #[must_use]
    pub fn report(
        &self,
        bias: &ProviderBiasConfig,
        adapters: Option<&crate::config::AdaptersConfig>,
    ) -> RosterReport {
        let (resolved, mut out) = self.resolved(adapters);
        let plan = resolved.plan(bias);
        let mut advisories = Vec::new();
        for r in &plan.rejected {
            // The one downgrade, and it is conditioned on the KERNEL's own
            // verdict about the roster, never on the rejection's label alone.
            if matches!(r.reason, SeatRejection::FamilyCollision { .. }) && plan.floor_reachable {
                advisories.push(format!(
                    "seat '{}' is NOT ON THE BALLOT (witness {}, {}): {}. The roster \
                     still reaches its floor of {} without it, so it may sit as a \
                     non-floor-bearing reader — but it is an echo of the seat it \
                     collides with, and its verdict may never be counted toward the \
                     floor or stand in for a refusing floor-bearing seat",
                    r.seat_id,
                    r.reason.witness_axis(),
                    r.reason.label(),
                    describe_rejection(&r.reason),
                    plan.requirement.min_distinct_families,
                ));
                continue;
            }
            out.push(format!(
                "seat '{}' fails witness {} ({}): {}",
                r.seat_id,
                r.reason.witness_axis(),
                r.reason.label(),
                describe_rejection(&r.reason),
            ));
        }
        // The REACHABLE floor, not the realized one. A roster written at
        // convene has dispatched nothing, so no refuter can yet carry an
        // injected contract and the realized floor is 1 by construction —
        // measuring it here would make this line fire on every correctly
        // shaped roster, which is a gate that always fails. What is refused
        // is a roster no dispatch could ever lift to the floor; a roster that
        // merely has not been dispatched yet is reported by its own delivery
        // lines above, which say the true thing about it.
        if !plan.floor_reachable {
            out.push(format!(
                "the roster reaches {} distinct provider family/families, below the \
                 required floor of {} — no dispatch can lift it, so the committee \
                 cannot be convened as written (stake {:?}, generator '{}' + {} \
                 refuter(s) on distinct endpoints). Widen it or point the colliding \
                 seats at distinct providers",
                plan.reachable_families(),
                plan.requirement.min_distinct_families,
                self.stake,
                plan.generator.seat_id,
                plan.admitted.len()
                    + plan
                        .rejected
                        .iter()
                        .filter(|r| r.reason.is_delivery())
                        .count(),
            ));
        }
        // Fragile-but-legal, said once and always: a floor carried by a single
        // seat is the shape one provider refusal vacates.
        if plan.floor_is_single_point_of_failure() {
            advisories.push(format!(
                "the roster's floor rests entirely on {:?} — a single refusal there \
                 vacates the jury and the round is INCONCLUSIVE, not CLEAN. Legal, \
                 and it must be carried into `roster.md` rather than discovered \
                 mid-round",
                plan.floor_bearing_seats(),
            ));
        }
        RosterReport {
            refusals: out,
            advisories,
        }
    }
}

/// A roster's full reading: what refuses it, and what is loud about it without
/// refusing it.
///
/// # Why two lists and not one
///
/// A gate that refuses everything it notices forbids the only roster many
/// galaxies can wire; a gate that reports everything it refuses enforces
/// nothing. The two lists keep the honest distinction the ADR-153 lint is built
/// on — *the gate refuses what is invalid and names what is merely brittle* —
/// and put it in the type rather than in a caller's discipline.
///
/// Only [`Self::refusals`] may decide an exit status. [`Self::advisories`] are
/// printed in full, always: an advisory nobody prints is silence with extra
/// steps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RosterReport {
    /// Reasons the roster may not be convened. Non-empty means refused.
    pub refusals: Vec<String>,
    /// True and load-bearing statements about a roster that is nonetheless
    /// legal — the non-floor-bearing readers and the single-point-of-failure
    /// floor.
    pub advisories: Vec<String>,
}

/// Render a [`SeatRejection`] as the sentence a convener needs in order to fix
/// it — which seat it collided with, or which artefact is missing.
///
/// The label alone (`family-collision`) names the class; this names the
/// instance. A gate that only prints the class sends its reader back to the
/// source to find out what happened.
#[must_use]
pub fn describe_rejection(reason: &SeatRejection) -> String {
    match reason {
        SeatRejection::FamilyCollision {
            endpoint,
            collides_with,
        } => format!(
            "it resolves to the same endpoint as '{collides_with}' \
             (provider '{}', base_url '{}', family '{}') — two seats on one \
             endpoint are an echo, not two witnesses. Distinctness is measured \
             on the RESOLVED endpoint, never the adapter name",
            endpoint.provider,
            if endpoint.base_url.is_empty() {
                "<vendor default>"
            } else {
                &endpoint.base_url
            },
            endpoint.family,
        ),
        SeatRejection::PersonaCollision {
            role_id,
            collides_with,
        } => format!(
            "it plays role_id '{role_id}', already claimed by '{collides_with}' — \
             the same posture wearing two provider hats is one witness, not two"
        ),
        SeatRejection::BriefingNotInjected => format!(
            "its adversarial briefing contract is absent, the wrong version, or \
             declared-but-not-delivered. Delivery is the two-fact test: the durable \
             `{COMMITTEE_POSTURE_FILE}` must exist in the seat's molecule directory \
             AND its briefing.md must carry the pointer at it (`cs tackle` and \
             `cs evolve` both re-establish that pointer)"
        ),
        SeatRejection::FalsificationArtifactMissing => {
            "it shipped no falsification-attempt artefact — no evidence it tried to \
             BREAK the fix rather than read it. A refuter that only read is a reader"
                .to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderRequirementSet;

    /// A fully free endpoint tuple — all three ADR-147 axes independent.
    ///
    /// The convenience [`endpoint`] below pins `provider == family` and an
    /// empty `base_url`, which makes most fixtures readable but cannot express
    /// the states the invariant is actually about: two seats distinct in
    /// `base_url` yet sharing a family, or a mid-round swap that moves only the
    /// URL. A generator that cannot emit the failing case makes its suite green
    /// for the wrong reason, so the free constructor is the primitive and the
    /// pinned one is derived from it.
    fn endpoint_at(provider: &str, base_url: &str, family: &str) -> EndpointTuple {
        EndpointTuple {
            provider: provider.into(),
            base_url: base_url.into(),
            family: family.into(),
        }
    }

    fn endpoint(provider: &str, family: &str) -> EndpointTuple {
        endpoint_at(provider, "", family)
    }

    /// The contract prose every delivery fixture below is built over.
    ///
    /// It is a named constant rather than a literal at each site because the
    /// hash and the body are now bound to each other: a fixture that means
    /// "delivered" must declare the digest of *this* text, and a fixture that
    /// means "forged" is exactly one that does not.
    const CONTRACT_BODY: &str = "Audit the artefacts. The generator's confidence is not evidence.";

    fn briefing() -> AdversarialBriefing {
        AdversarialBriefing {
            version: ADVERSARIAL_BRIEFING_VERSION,
            contract_hash: committee_contract_hash(CONTRACT_BODY),
            injected: true,
        }
    }

    /// What the observing port reports for a seat whose directory really holds
    /// the contract [`briefing`] declares, pointer and all.
    ///
    /// It is spelled out as a helper rather than as `(true, true)` because the
    /// posture leg is no longer a boolean: the witness compares the file's
    /// header against the roster's declaration, so a fixture that means
    /// "delivered" has to say *which contract* was delivered.
    fn delivered() -> ObservedDelivery {
        let claimed = briefing();
        ObservedDelivery {
            posture_file_exists: true,
            posture: Some(PostureContract {
                version: claimed.version,
                contract_hash: claimed.contract_hash,
                body: CONTRACT_BODY.into(),
            }),
            pointer: true,
        }
    }

    /// A seat candidate on an arbitrary endpoint tuple.
    ///
    /// [`seat`] wraps this with `provider == family` and no `base_url`; use
    /// this one whenever the state under test needs the axes to move apart.
    fn seat_at(id: &str, role: SeatRole, endpoint: EndpointTuple, role_id: &str) -> SeatCandidate {
        SeatCandidate {
            seat_id: id.into(),
            role,
            // The plan-level fixtures exercise `plan_committee`, which is fed
            // already-resolved tuples. Resolution itself is exercised by the
            // `resolved`/`violations` tests, which name real adapters.
            adapter: Some(id.into()),
            family: FamilyWitness {
                endpoint,
                model: None,
            },
            persona: PersonaWitness {
                role_id: role_id.into(),
                briefing: Some(briefing()),
                falsification_artifact: Some("falsification-attempt.md".into()),
            },
            replaced_seat_id: None,
            replacement_reason: None,
        }
    }

    fn seat(id: &str, role: SeatRole, family: &str, role_id: &str) -> SeatCandidate {
        seat_at(id, role, endpoint(family, family), role_id)
    }

    /// A seat candidate pinned to a **named model**, as the real roster pins
    /// them. Without this the sibling-switch case cannot be expressed at all:
    /// the endpoint tuple stops at family, so `sol` and `terra` are the same
    /// fixture.
    fn seat_pinned(
        id: &str,
        role: SeatRole,
        endpoint: EndpointTuple,
        model: &str,
        role_id: &str,
    ) -> SeatCandidate {
        let mut candidate = seat_at(id, role, endpoint, role_id);
        candidate.family.model = Some(model.into());
        candidate
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

    /// A delivering seat whose realized endpoint was **never observed** — a
    /// review with an unattested family. Fine for the plain door, which reads
    /// opinions only; never enough to carry a diversity floor.
    fn outcome(id: &str, verdict: SeatVerdict, red: bool) -> SeatOutcome {
        SeatOutcome {
            seat_id: id.into(),
            verdict,
            falsifier_red: red,
            delivery: SeatDelivery::Delivered,
            realized_endpoint: None,
            realized_model: None,
            switched_after_dispatch: false,
        }
    }

    /// A seat whose provider refused the work mid-review — the recurring field
    /// event, now three times in two days on one seat.
    fn refused(id: &str) -> SeatOutcome {
        SeatOutcome {
            delivery: SeatDelivery::ProviderRefusal,
            ..outcome(id, SeatVerdict::Inconclusive, false)
        }
    }

    /// The expected drift record for a plain tuple divergence — no model pin on
    /// either side, no human in the loop.
    fn tuple_drift(seat_id: &str, specified: EndpointTuple, realized: EndpointTuple) -> SeatDrift {
        SeatDrift {
            seat_id: seat_id.into(),
            specified,
            realized: Some(realized),
            specified_model: None,
            realized_model: None,
            human_switch: false,
        }
    }

    /// A delivering seat whose realized endpoint **was** observed — the only
    /// shape that may be credited with a family.
    fn observed(id: &str, verdict: SeatVerdict, red: bool, realized: EndpointTuple) -> SeatOutcome {
        SeatOutcome {
            realized_endpoint: Some(realized),
            ..outcome(id, verdict, red)
        }
    }

    /// The generator, **observed** on the endpoint it was rostered against.
    ///
    /// Nearly every fold fixture needs this, and that is the point. The
    /// generator earns its reference family the same way a reader does — by
    /// having been watched — so a test that means *"the generator ran as
    /// specified"* now has to say so instead of leaning on a fallback that
    /// scored the plan. Omit it deliberately to express the opposite case.
    fn generator_ran(plan: &RosterPlan) -> SeatOutcome {
        observed(
            &plan.generator.seat_id,
            SeatVerdict::Confirmed,
            false,
            plan.generator.endpoint.clone(),
        )
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

    // ── jury integrity: the delivered roster, not the planned one ────────
    //
    // These reconstruct converge-20260727-a302 as a roster: the fix molecule
    // was tackled under the galaxy-default `claude` adapter, so the generator
    // resolves to the anthropic family; the formula's own claude seat then
    // collides with it and is rejected; the codex seat is left carrying the
    // entire diversity floor alone — and its provider refused the work
    // mid-review, twice in two days.

    /// The `converge-clean-room` roster, exactly as the formula prescribed it.
    fn converge_clean_room_roster() -> RosterPlan {
        let generator = seat(
            "fix-molecule",
            SeatRole::Generator,
            "anthropic",
            "generator",
        );
        let refuters = vec![
            seat("review-claude", SeatRole::Refuter, "anthropic", "refuter-a"),
            seat("review-codex-sol", SeatRole::Refuter, "openai", "refuter-b"),
        ];
        plan_committee(&generator, &refuters, root_req())
    }

    #[test]
    fn the_prescribed_roster_seats_two_but_only_one_bears_the_floor() {
        let plan = converge_clean_room_roster();

        // The claude seat is not on the ballot at all: same resolved endpoint
        // as the generator it audits.
        assert_eq!(plan.admissible_seat_ids(), vec!["review-codex-sol"]);
        assert!(matches!(
            plan.rejected.first().map(|r| &r.reason),
            Some(SeatRejection::FamilyCollision { .. })
        ));

        // The floor is met — and rests entirely on one seat. A formula that
        // names two seats has produced one family plus one non-floor-bearing
        // reader, and nothing in the seat *count* says so.
        assert!(plan.floor_met);
        assert_eq!(plan.floor_bearing_seats(), vec!["review-codex-sol"]);
        assert!(plan.floor_is_single_point_of_failure());
    }

    #[test]
    fn a_roster_sitting_exactly_at_a_floor_of_three_has_no_slack_either() {
        // Two load-bearing seats, not one — and either refusing still vacates
        // the floor. A `len() == 1` test would call this roster safe; it is
        // not, and at risk security/release this is the shape the generator
        // collision would eat one of the three seats from.
        let generator = seat(
            "fix-molecule",
            SeatRole::Generator,
            "anthropic",
            "generator",
        );
        let refuters = vec![
            seat("review-codex", SeatRole::Refuter, "openai", "refuter-a"),
            seat("review-local", SeatRole::Refuter, "qwen", "refuter-b"),
        ];
        let plan = plan_committee(
            &generator,
            &refuters,
            CommitteeRequirement {
                required: true,
                min_distinct_families: 3,
            },
        );
        assert!(plan.floor_met);
        assert_eq!(
            plan.floor_bearing_seats(),
            vec!["review-codex", "review-local"]
        );
        assert!(plan.floor_is_single_point_of_failure());

        // And the fold agrees: one refusal drops it below three.
        let outcomes = vec![
            generator_ran(&plan),
            refused("review-codex"),
            observed(
                "review-local",
                SeatVerdict::Confirmed,
                false,
                endpoint("qwen", "qwen"),
            ),
        ];
        let fold = fold_committee(&plan, &outcomes);
        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
        assert_eq!(fold.integrity.delivered_families, 2);
    }

    #[test]
    fn a_refused_floor_bearing_seat_cannot_be_folded_as_a_clean_jury() {
        let plan = converge_clean_room_roster();

        // The round as it actually ran: the codex seat's provider refused the
        // work mid-review; the claude seat read the fix and found nothing.
        let outcomes = vec![
            generator_ran(&plan),
            refused("review-codex-sol"),
            observed(
                "review-claude",
                SeatVerdict::Confirmed,
                false,
                endpoint("anthropic", "anthropic"),
            ),
        ];

        let fold = fold_committee(&plan, &outcomes);

        // The load-bearing assertion: no jury, no confirmation. The surviving
        // confirmation comes from a seat that shares the generator's endpoint,
        // so it is an echo, and an echo may not sign off.
        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
        assert!(!fold.integrity.floor_met);
        assert_eq!(fold.integrity.delivered_families, 1);
        assert_eq!(fold.integrity.required_families, 2);

        // And the refusal is *recorded*, which is the whole difference from a
        // human quietly rescuing the seat from inside the pane.
        assert_eq!(
            fold.integrity.non_delivering,
            vec![SeatNonDelivery {
                seat_id: "review-codex-sol".into(),
                delivery: SeatDelivery::ProviderRefusal,
            }]
        );
        assert_eq!(
            fold.integrity.non_delivering[0].delivery.label(),
            "provider-refusal"
        );
    }

    #[test]
    fn a_symmetric_roster_survives_one_refusal_where_the_fragile_one_does_not() {
        // The fix the fragility finding argues for: seats chosen so that BOTH
        // differ from the generator's family. Now no single seat bears the
        // floor, and one provider refusal does not vacate the jury.
        let generator = seat(
            "fix-molecule",
            SeatRole::Generator,
            "anthropic",
            "generator",
        );
        let refuters = vec![
            seat("review-codex", SeatRole::Refuter, "openai", "refuter-a"),
            seat("review-local", SeatRole::Refuter, "qwen", "refuter-b"),
        ];
        let plan = plan_committee(&generator, &refuters, root_req());

        assert_eq!(plan.admitted.len(), 2);
        assert!(plan.floor_bearing_seats().is_empty());
        assert!(!plan.floor_is_single_point_of_failure());

        let outcomes = vec![
            generator_ran(&plan),
            refused("review-codex"),
            observed(
                "review-local",
                SeatVerdict::Confirmed,
                false,
                endpoint("qwen", "qwen"),
            ),
        ];

        let fold = fold_committee(&plan, &outcomes);
        // What survives the refusal is the FLOOR: two families still answered,
        // where the fragile roster is left with one. That is the property the
        // symmetric roster buys, and it is a different property from a clean
        // certification.
        assert!(fold.integrity.floor_met);
        assert_eq!(fold.integrity.delivered_families, 2);
        // Survivable is not the same as unremarked, and unremarked is not the
        // same as clean: the refusal is on the record, so the jury is not
        // intact, so the round may not be certified. A seat short is a seat
        // short whatever the floor says.
        assert!(!fold.integrity.is_intact());
        assert_eq!(fold.integrity.non_delivering.len(), 1);
        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
    }

    #[test]
    fn a_realized_endpoint_that_collapses_onto_a_peer_vacates_the_floor() {
        // The in-pane model switch: the seat still reads `openai` on the
        // roster, but what answered resolved into the generator's family. The
        // floor is a claim about what answered.
        let generator = seat(
            "fix-molecule",
            SeatRole::Generator,
            "anthropic",
            "generator",
        );
        let refuters = vec![seat(
            "review-codex",
            SeatRole::Refuter,
            "openai",
            "refuter-a",
        )];
        let plan = plan_committee(&generator, &refuters, root_req());
        assert!(plan.floor_met);

        let outcomes = vec![observed(
            "review-codex",
            SeatVerdict::Confirmed,
            false,
            endpoint("anthropic", "anthropic"),
        )];

        let fold = fold_committee(&plan, &outcomes);
        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
        assert_eq!(fold.integrity.delivered_families, 1);
        // The divergence is surfaced, not inferred: specified and realized are
        // both on the record so an operator can see which one they were shown.
        assert_eq!(
            fold.integrity.drift,
            vec![tuple_drift(
                "review-codex",
                endpoint("openai", "openai"),
                endpoint("anthropic", "anthropic"),
            )]
        );
    }

    // ── unknown is not a synonym for compliant ──────────────────────────
    //
    // The realized endpoint has three states, not two: observed-and-equal,
    // observed-and-different, and NOT OBSERVED. Collapsing the third into the
    // first is how a seat nobody watched gets credited with a family.

    #[test]
    fn an_unobserved_realized_endpoint_is_not_evidence_of_a_distinct_family() {
        // One admitted seat, specified `openai`, delivering a confirmation —
        // and nobody recorded which endpoint answered. On the specified tuple
        // the floor of two reads as met; on what was actually attested, the
        // jury is the generator's family and nothing else.
        let generator = seat(
            "fix-molecule",
            SeatRole::Generator,
            "anthropic",
            "generator",
        );
        let refuters = vec![seat(
            "review-codex",
            SeatRole::Refuter,
            "openai",
            "refuter-a",
        )];
        let plan = plan_committee(&generator, &refuters, root_req());
        assert!(
            plan.floor_met,
            "the PLANNED roster is legal — that is the trap"
        );

        let outcomes = vec![
            generator_ran(&plan),
            outcome("review-codex", SeatVerdict::Confirmed, false),
        ];
        let fold = fold_committee(&plan, &outcomes);

        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
        assert_eq!(fold.integrity.delivered_families, 1);
        assert!(!fold.integrity.floor_met);
        // Recorded as its own disposition, not smuggled into either of the
        // other two lists: the seat DID deliver, and it did NOT drift.
        assert_eq!(fold.integrity.unobserved, vec!["review-codex".to_string()]);
        assert!(fold.integrity.non_delivering.is_empty());
        assert!(fold.integrity.drift.is_empty());
        assert!(!fold.integrity.is_intact());
    }

    #[test]
    fn an_unobserved_seat_may_still_refute_it_just_may_not_certify() {
        // The asymmetry is deliberate. Withholding a diversity credit from an
        // unattested seat must not also silence its finding — that would be
        // fail-open in the one direction that matters.
        let plan = converge_clean_room_roster();
        let outcomes = vec![
            generator_ran(&plan),
            outcome("review-codex-sol", SeatVerdict::Refuted, false),
        ];
        let fold = fold_committee(&plan, &outcomes);
        assert_eq!(fold.verdict, CommitteeVerdict::Refuted);
        assert_eq!(
            fold.integrity.unobserved,
            vec!["review-codex-sol".to_string()]
        );
    }

    #[test]
    fn an_unobserved_seat_costs_the_floor_nothing_and_the_certification_everything() {
        // The withheld family credit only bites the FLOOR when it was
        // load-bearing: here `review-local` attests its family and carries the
        // floor by itself, so `review-codex` being unattested leaves the count
        // untouched. It does not leave the verdict untouched. A round in which
        // nobody wrote down what one of the seats ran on is a round we cannot
        // certify, whatever the arithmetic says.
        let generator = seat(
            "fix-molecule",
            SeatRole::Generator,
            "anthropic",
            "generator",
        );
        let refuters = vec![
            seat("review-codex", SeatRole::Refuter, "openai", "refuter-a"),
            seat("review-local", SeatRole::Refuter, "qwen", "refuter-b"),
        ];
        let plan = plan_committee(&generator, &refuters, root_req());

        let outcomes = vec![
            generator_ran(&plan),
            outcome("review-codex", SeatVerdict::Confirmed, false),
            observed(
                "review-local",
                SeatVerdict::Confirmed,
                false,
                endpoint("qwen", "qwen"),
            ),
        ];
        let fold = fold_committee(&plan, &outcomes);

        assert!(fold.integrity.floor_met);
        assert_eq!(fold.integrity.delivered_families, 2);
        assert_eq!(fold.integrity.unobserved, vec!["review-codex".to_string()]);
        assert!(!fold.integrity.is_intact());
        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
    }

    #[test]
    fn an_observed_endpoint_equal_to_the_rostered_one_is_not_the_same_fact_as_an_unobserved_one() {
        // The whole distinction in one comparison: same seat, same verdict, the
        // only difference being whether anyone looked.
        let generator = seat(
            "fix-molecule",
            SeatRole::Generator,
            "anthropic",
            "generator",
        );
        let refuters = vec![seat(
            "review-codex",
            SeatRole::Refuter,
            "openai",
            "refuter-a",
        )];
        let plan = plan_committee(&generator, &refuters, root_req());

        let looked = fold_committee(
            &plan,
            &[
                generator_ran(&plan),
                observed(
                    "review-codex",
                    SeatVerdict::Confirmed,
                    false,
                    endpoint("openai", "openai"),
                ),
            ],
        );
        assert_eq!(looked.verdict, CommitteeVerdict::Confirmed);
        assert!(looked.integrity.is_intact());

        let did_not_look = fold_committee(
            &plan,
            &[
                generator_ran(&plan),
                outcome("review-codex", SeatVerdict::Confirmed, false),
            ],
        );
        assert_eq!(did_not_look.verdict, CommitteeVerdict::Inconclusive);
    }

    #[test]
    fn an_observed_generator_that_drifted_onto_its_reader_vacates_the_floor() {
        // The generator has a realized endpoint too. When the caller supplies
        // one, it is used and its divergence recorded — otherwise the floor's
        // guarantee would rest on the one seat nobody is allowed to question.
        let generator = seat(
            "fix-molecule",
            SeatRole::Generator,
            "anthropic",
            "generator",
        );
        let refuters = vec![seat(
            "review-codex",
            SeatRole::Refuter,
            "openai",
            "refuter-a",
        )];
        let plan = plan_committee(&generator, &refuters, root_req());

        let outcomes = vec![
            // The generator actually ran on the reader's family.
            observed(
                "fix-molecule",
                SeatVerdict::Confirmed,
                false,
                endpoint("openai", "openai"),
            ),
            observed(
                "review-codex",
                SeatVerdict::Confirmed,
                false,
                endpoint("openai", "openai"),
            ),
        ];
        let fold = fold_committee(&plan, &outcomes);

        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
        assert_eq!(fold.integrity.delivered_families, 1);
        assert_eq!(
            fold.integrity.drift,
            vec![tuple_drift(
                "fix-molecule",
                endpoint("anthropic", "anthropic"),
                endpoint("openai", "openai"),
            )]
        );
    }

    // ── the three ADR-147 axes move independently ───────────────────────

    #[test]
    fn two_seats_distinct_in_base_url_but_sharing_a_family_count_as_one_axis() {
        // A state the pinned fixture could not express at all: both seats are
        // admitted (their TUPLES differ, so neither is a family collision at
        // plan time) yet they answer from one family, so the delivered floor
        // sees a single axis. Distinctness of tuples is not distinctness of
        // error modes — the floor counts the latter.
        let generator = seat_at(
            "fix-molecule",
            SeatRole::Generator,
            endpoint("anthropic", "anthropic"),
            "generator",
        );
        let refuters = vec![
            seat_at(
                "review-direct",
                SeatRole::Refuter,
                endpoint_at("openai", "https://api.openai.com", "openai"),
                "refuter-a",
            ),
            seat_at(
                "review-gateway",
                SeatRole::Refuter,
                endpoint_at("openai", "https://gateway.internal/openai", "openai"),
                "refuter-b",
            ),
        ];
        let plan = plan_committee(&generator, &refuters, root_req());
        assert_eq!(
            plan.admitted.len(),
            2,
            "distinct tuples are both admissible"
        );

        let outcomes = vec![
            generator_ran(&plan),
            observed(
                "review-direct",
                SeatVerdict::Confirmed,
                false,
                endpoint_at("openai", "https://api.openai.com", "openai"),
            ),
            observed(
                "review-gateway",
                SeatVerdict::Confirmed,
                false,
                endpoint_at("openai", "https://gateway.internal/openai", "openai"),
            ),
        ];
        let fold = fold_committee(&plan, &outcomes);

        // Floor of two: anthropic + openai. Met — but by ONE reader family
        // wearing two URLs, which is what the count must say.
        assert_eq!(fold.integrity.delivered_families, 2);
        assert_eq!(fold.verdict, CommitteeVerdict::Confirmed);
    }

    #[test]
    fn a_mid_round_swap_that_moves_only_the_base_url_is_still_recorded_drift() {
        // The in-pane rescue does not always change the family. A seat
        // re-pointed at a proxy in front of the same vendor keeps its family
        // and its floor credit — and must still leave a trace, because the
        // family label is derived config, not an attested fact (§8b).
        let generator = seat(
            "fix-molecule",
            SeatRole::Generator,
            "anthropic",
            "generator",
        );
        let refuters = vec![seat_at(
            "review-codex",
            SeatRole::Refuter,
            endpoint_at("openai", "https://api.openai.com", "openai"),
            "refuter-a",
        )];
        let plan = plan_committee(&generator, &refuters, root_req());

        let outcomes = vec![
            generator_ran(&plan),
            observed(
                "review-codex",
                SeatVerdict::Confirmed,
                false,
                endpoint_at("openai", "https://gateway.internal/openai", "openai"),
            ),
        ];
        let fold = fold_committee(&plan, &outcomes);

        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
        assert_eq!(fold.integrity.delivered_families, 2);
        assert_eq!(
            fold.integrity.drift,
            vec![tuple_drift(
                "review-codex",
                endpoint_at("openai", "https://api.openai.com", "openai"),
                endpoint_at("openai", "https://gateway.internal/openai", "openai"),
            )]
        );
        // The floor survives — recomputed on what answered, it still spans two
        // families — and the jury is still NOT intact. Those are different
        // questions: "did the round have enough independent readers" and "is
        // the roster a true description of who sat". Failing the second is what
        // costs the round its certification: the count is fine, the record is
        // not, and the verdict follows the record.
        assert!(fold.integrity.floor_met);
        assert!(!fold.integrity.is_intact());
    }

    // ── the rescue that the tuple cannot see ────────────────────────────
    //
    // The measured case, three times in two days on one seat: `gpt-5.6-sol`
    // stalls on a provider guardrail mid-review and the operator re-points the
    // pane at `gpt-5.6-terra`. Same provider, same base_url, same family — so
    // the endpoint tuple is IDENTICAL and a drift check that only compares
    // tuples records nothing at all. Three out of three is not an edge case;
    // it is what this seat does.

    /// The `converge-clean-room` codex seat as it is actually rostered: pinned
    /// to a named model, not just to a family.
    fn pinned_codex_roster() -> RosterPlan {
        let generator = seat_pinned(
            "fix-molecule",
            SeatRole::Generator,
            endpoint("anthropic", "anthropic"),
            "claude-opus-5",
            "generator",
        );
        let refuters = vec![seat_pinned(
            "review-codex",
            SeatRole::Refuter,
            endpoint("openai", "openai"),
            "gpt-5.6-sol",
            "refuter-a",
        )];
        plan_committee(&generator, &refuters, root_req())
    }

    #[test]
    fn an_in_family_sibling_switch_is_recorded_even_though_the_tuple_never_moves() {
        let plan = pinned_codex_roster();

        let outcomes = vec![
            generator_ran(&plan),
            SeatOutcome {
                realized_model: Some("gpt-5.6-terra".into()),
                switched_after_dispatch: true,
                ..observed(
                    "review-codex",
                    SeatVerdict::Confirmed,
                    false,
                    // Identical to the rostered tuple — this is the whole point.
                    endpoint("openai", "openai"),
                )
            },
        ];
        let fold = fold_committee(&plan, &outcomes);

        // The tuple did not move, so the FLOOR is untouched: diversity is a
        // family property and the family is unchanged. The verdict is another
        // matter — a jury that needed a hand mid-round may not certify.
        assert!(fold.integrity.floor_met);
        assert_eq!(fold.integrity.delivered_families, 2);
        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);

        // And yet the switch is on the record — which is what a tuple-only
        // comparison could never produce.
        assert_eq!(
            fold.integrity.drift,
            vec![SeatDrift {
                seat_id: "review-codex".into(),
                specified: endpoint("openai", "openai"),
                realized: Some(endpoint("openai", "openai")),
                specified_model: Some("gpt-5.6-sol".into()),
                realized_model: Some("gpt-5.6-terra".into()),
                human_switch: true,
            }]
        );
        assert_eq!(
            fold.integrity.hand_rescues(),
            vec![
                "review-codex: gpt-5.6-sol ~> gpt-5.6-terra (switched by hand after dispatch)"
                    .to_string()
            ]
        );

        // The load-bearing consequence: a jury that needed a hand does not get
        // to call itself intact, however clean its verdict reads.
        assert!(!fold.integrity.is_intact());
    }

    #[test]
    fn a_hand_rescue_is_recorded_even_when_nobody_wrote_down_where_it_landed() {
        // The operator switches the pane but records neither endpoint nor
        // model. Two facts survive: the rescue happened, and the seat's family
        // is now unattested. Neither may be silently dropped.
        let plan = pinned_codex_roster();

        let outcomes = vec![
            generator_ran(&plan),
            SeatOutcome {
                switched_after_dispatch: true,
                ..outcome("review-codex", SeatVerdict::Confirmed, false)
            },
        ];
        let fold = fold_committee(&plan, &outcomes);

        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
        assert_eq!(fold.integrity.unobserved, vec!["review-codex".to_string()]);
        assert_eq!(
            fold.integrity.hand_rescues(),
            vec![
                "review-codex: gpt-5.6-sol ~> unobserved (switched by hand after dispatch)"
                    .to_string()
            ]
        );
        assert_eq!(fold.integrity.drift.len(), 1);
        assert_eq!(fold.integrity.drift[0].realized, None);
    }

    #[test]
    fn a_seat_left_alone_reports_no_rescue() {
        // The negative case, so `hand_rescues` empty means something. A clean
        // round has an empty rescue list and an intact jury.
        let plan = pinned_codex_roster();
        let outcomes = vec![
            generator_ran(&plan),
            SeatOutcome {
                realized_model: Some("gpt-5.6-sol".into()),
                ..observed(
                    "review-codex",
                    SeatVerdict::Confirmed,
                    false,
                    endpoint("openai", "openai"),
                )
            },
        ];
        let fold = fold_committee(&plan, &outcomes);

        assert_eq!(fold.verdict, CommitteeVerdict::Confirmed);
        assert!(fold.integrity.drift.is_empty());
        assert!(fold.integrity.hand_rescues().is_empty());
        assert!(fold.integrity.is_intact());
    }

    #[test]
    fn a_switch_back_to_the_rostered_pin_is_still_a_rescue() {
        // The rescue and the difference are independent facts. An operator who
        // re-points a seat at the model it was already pinned to has still
        // reached into the pane, and the jury still needed a hand.
        let plan = pinned_codex_roster();
        let outcomes = vec![
            generator_ran(&plan),
            SeatOutcome {
                realized_model: Some("gpt-5.6-sol".into()),
                switched_after_dispatch: true,
                ..observed(
                    "review-codex",
                    SeatVerdict::Confirmed,
                    false,
                    endpoint("openai", "openai"),
                )
            },
        ];
        let fold = fold_committee(&plan, &outcomes);

        assert_eq!(fold.integrity.drift.len(), 1);
        assert!(fold.integrity.drift[0].human_switch);
        assert!(!fold.integrity.is_intact());
        // Nothing measurable moved, so the floor is untouched — and the round
        // still cannot be certified, because the hand in the pane is itself the
        // fact that makes the roster an untrue description of the jury.
        assert!(fold.integrity.floor_met);
        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
    }

    #[test]
    fn three_rescues_in_one_round_are_three_records_not_one_shrug() {
        // Three occurrences is the measured rate on this seat. A round that
        // needed three hands must read as three hands.
        let generator = seat_pinned(
            "fix-molecule",
            SeatRole::Generator,
            endpoint("anthropic", "anthropic"),
            "claude-opus-5",
            "generator",
        );
        let refuters = vec![
            seat_pinned(
                "review-codex",
                SeatRole::Refuter,
                endpoint("openai", "openai"),
                "gpt-5.6-sol",
                "refuter-a",
            ),
            seat_pinned(
                "review-local",
                SeatRole::Refuter,
                endpoint("qwen", "qwen"),
                "qwen3-max",
                "refuter-b",
            ),
        ];
        let plan = plan_committee(&generator, &refuters, root_req());

        let switched = |id: &str, family: &str, to: &str| SeatOutcome {
            realized_model: Some(to.into()),
            switched_after_dispatch: true,
            ..observed(id, SeatVerdict::Confirmed, false, endpoint(family, family))
        };
        let outcomes = vec![
            switched("fix-molecule", "anthropic", "claude-opus-5-thinking"),
            switched("review-codex", "openai", "gpt-5.6-terra"),
            switched("review-local", "qwen", "qwen3-max-thinking"),
        ];
        let fold = fold_committee(&plan, &outcomes);

        assert_eq!(fold.integrity.hand_rescues().len(), 3);
        assert!(!fold.integrity.is_intact());
        // Every family still answered, so the floor is met — and "floor met"
        // was never the same claim as "this jury is what the roster says".
        // Three hands in one round is the second claim failing, and the door
        // now reads the second claim too.
        assert!(fold.integrity.floor_met);
        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
    }

    #[test]
    fn a_hollow_jury_still_never_launders_a_refutation() {
        // Rule ordering: integrity gating must not turn a concrete red into a
        // softer "inconclusive". Fail-closed beats tidiness.
        let plan = converge_clean_room_roster();
        let outcomes = vec![
            SeatOutcome {
                verdict: SeatVerdict::Refuted,
                falsifier_red: true,
                ..refused("review-codex-sol")
            },
            outcome("review-claude", SeatVerdict::Confirmed, false),
        ];
        let fold = fold_committee(&plan, &outcomes);
        assert_eq!(fold.verdict, CommitteeVerdict::Refuted);
        assert!(!fold.integrity.floor_met);
    }

    #[test]
    fn an_undispatched_roster_seat_is_a_non_delivery_not_a_silent_absence() {
        let plan = converge_clean_room_roster();
        // No outcome at all for the floor-bearing seat.
        let outcomes = vec![outcome("review-claude", SeatVerdict::Confirmed, false)];
        let fold = fold_committee(&plan, &outcomes);
        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
        assert_eq!(
            fold.integrity.non_delivering,
            vec![SeatNonDelivery {
                seat_id: "review-codex-sol".into(),
                delivery: SeatDelivery::NotDispatched,
            }]
        );
    }

    #[test]
    fn the_plain_door_alone_is_what_confirmed_the_hollow_jury() {
        // The regression this whole module change exists for, stated as a test
        // so it cannot come back: `committee_verdict` over the same outcomes,
        // with the roster dropped, reads the round as a clean pass. It is not
        // wrong — it is under-informed, and that is precisely why callers must
        // fold through `fold_committee` and never this door directly.
        let outcomes = vec![outcome("review-claude", SeatVerdict::Confirmed, false)];
        assert_eq!(committee_verdict(&outcomes), CommitteeVerdict::Confirmed);

        let plan = converge_clean_room_roster();
        assert_eq!(
            fold_committee(&plan, &outcomes).verdict,
            CommitteeVerdict::Inconclusive
        );
    }

    #[test]
    fn a_seat_outcome_predating_the_delivery_axis_still_deserialises() {
        // Back-compat: an older record asserts a verdict, and a verdict implies
        // the seat ran — so the absent field defaults to `Delivered`.
        let legacy = r#"{"seat_id":"s1","verdict":"confirmed","falsifier_red":false}"#;
        let parsed: SeatOutcome =
            serde_json::from_str(legacy).expect("legacy seat outcome must still parse");
        assert_eq!(parsed.delivery, SeatDelivery::Delivered);
        // The two defaults point in OPPOSITE directions, and deliberately so.
        // `delivery` defaults open because a stored verdict implies the seat
        // ran; `realized_endpoint` defaults closed because nothing about a
        // stored verdict implies anyone observed which endpoint produced it.
        // So a legacy record reviews, but never certifies a family.
        assert!(parsed.realized_endpoint.is_none());

        // And that is visible in the fold: a roster whose ONLY admitted seat
        // reports through a legacy record folds inconclusive, because the seat
        // that was supposed to hold the floor never attested its family.
        let generator = seat(
            "fix-molecule",
            SeatRole::Generator,
            "anthropic",
            "generator",
        );
        let refuters = vec![seat(
            "review-codex",
            SeatRole::Refuter,
            "openai",
            "refuter-a",
        )];
        let plan = plan_committee(&generator, &refuters, root_req());
        let legacy_seat =
            r#"{"seat_id":"review-codex","verdict":"confirmed","falsifier_red":false}"#;
        let seated: SeatOutcome =
            serde_json::from_str(legacy_seat).expect("legacy seat outcome must still parse");
        let fold = fold_committee(&plan, &[generator_ran(&plan), seated]);
        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
        assert_eq!(fold.integrity.unobserved, vec!["review-codex".to_string()]);
    }

    // ── the generator is not a special case ─────────────────────────────

    #[test]
    fn an_unobserved_generator_carries_no_part_of_the_floor_either() {
        // The last surviving instance of the fallback: the seat that *proposes*
        // was scored from the plan whenever nobody watched it, by the same
        // reasoning this module refuses for every reader. Here the generator
        // delivered and its realized endpoint was never observed; the single
        // reader attested `openai`. On the plan tuple the floor of two reads as
        // met (anthropic + openai) and the round certifies. On what was
        // actually attested there is one family and no jury.
        let generator = seat(
            "fix-molecule",
            SeatRole::Generator,
            "anthropic",
            "generator",
        );
        let refuters = vec![seat(
            "review-codex",
            SeatRole::Refuter,
            "openai",
            "refuter-a",
        )];
        let plan = plan_committee(&generator, &refuters, root_req());
        assert!(
            plan.floor_met,
            "the PLANNED roster is legal — that is the trap"
        );

        let outcomes = vec![
            // Delivered, and nobody wrote down what answered.
            outcome("fix-molecule", SeatVerdict::Confirmed, false),
            observed(
                "review-codex",
                SeatVerdict::Confirmed,
                false,
                endpoint("openai", "openai"),
            ),
        ];
        let fold = fold_committee(&plan, &outcomes);

        assert_eq!(
            fold.integrity.delivered_families, 1,
            "an unwatched generator contributes no family"
        );
        assert!(!fold.integrity.floor_met);
        assert_eq!(fold.integrity.unobserved, vec!["fix-molecule".to_string()]);
        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
    }

    #[test]
    fn a_generator_with_no_outcome_at_all_is_unobserved_not_assumed_compliant() {
        // The other half of the same rule. A caller that supplies no
        // observation for the generator has told us nothing about which
        // endpoint produced the artefact under review — which is the identical
        // epistemic state as an outcome with no realized endpoint, and must
        // read the same way. Silence is not a certificate.
        let plan = converge_clean_room_roster();
        let outcomes = vec![observed(
            "review-codex-sol",
            SeatVerdict::Confirmed,
            false,
            endpoint("openai", "openai"),
        )];
        let fold = fold_committee(&plan, &outcomes);

        assert_eq!(fold.integrity.delivered_families, 1);
        assert_eq!(fold.integrity.unobserved, vec!["fix-molecule".to_string()]);
        assert!(!fold.integrity.floor_met);
        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
    }

    // ── the integrity flag is a control, not a caveat ────────────────────

    #[test]
    fn a_jury_that_records_its_own_compromise_cannot_certify_through_the_real_door() {
        // The §8z case, asserted where it matters: on `fold_committee`, the
        // call a caller actually makes. Every ingredient of a clean round is
        // present — both seats delivered, both endpoints observed, both
        // families attested, the floor met with slack — except that a human
        // reached into a pane mid-review. The old door read only the floor, so
        // this folded to CLEAN while carrying, in the same struct, the record
        // that it had been rescued. A caveat the operator cannot act on is not
        // a control.
        let plan = pinned_codex_roster();
        let outcomes = vec![
            generator_ran(&plan),
            SeatOutcome {
                realized_model: Some("gpt-5.6-terra".into()),
                switched_after_dispatch: true,
                ..observed(
                    "review-codex",
                    SeatVerdict::Confirmed,
                    false,
                    endpoint("openai", "openai"),
                )
            },
        ];

        let fold = fold_committee(&plan, &outcomes);

        // Everything the plain door looks at says CLEAN…
        assert!(fold.integrity.floor_met);
        assert_eq!(fold.integrity.delivered_families, 2);
        assert!(fold.integrity.non_delivering.is_empty());
        assert!(fold.integrity.unobserved.is_empty());
        assert_eq!(
            committee_verdict(&outcomes),
            CommitteeVerdict::Confirmed,
            "the under-informed door still says clean — that is why it may not be the last word"
        );
        // …and the jury says it was compromised, so the real door does not.
        assert!(!fold.integrity.is_intact());
        assert_ne!(
            fold.verdict,
            CommitteeVerdict::Confirmed,
            "a fold that records its own compromise must never certify"
        );
        assert_eq!(fold.verdict, CommitteeVerdict::Inconclusive);
    }

    #[test]
    fn a_compromised_jury_may_still_refute_through_the_same_door() {
        // The asymmetry, on the rescue axis rather than the non-delivery one:
        // withholding certification from a hand-rescued jury must not also
        // silence what it found. Gating both directions would be fail-open in
        // the one that matters.
        let plan = pinned_codex_roster();
        let outcomes = vec![
            generator_ran(&plan),
            SeatOutcome {
                switched_after_dispatch: true,
                ..observed(
                    "review-codex",
                    SeatVerdict::Refuted,
                    false,
                    endpoint("openai", "openai"),
                )
            },
        ];
        let fold = fold_committee(&plan, &outcomes);

        assert!(!fold.integrity.is_intact());
        assert_eq!(fold.verdict, CommitteeVerdict::Refuted);
    }

    // ── The gate's own falsifiers ────────────────────────────────────────
    //
    // `RosterSpec::violations` is what `cs reconcile --check` calls, and until
    // now it had NO test of its own: every predicate below it was covered and
    // the composed refusal was not. These are written so that reverting the
    // fix they pin turns each one red.

    /// A project `[adapters]` inventory, as the gate reads it.
    fn adapters(entries: &[(&str, Option<&str>, Option<&str>)]) -> crate::config::AdaptersConfig {
        let mut cfg = crate::config::AdaptersConfig::default();
        for (name, base_url, model) in entries {
            cfg.entries.insert(
                (*name).to_string(),
                crate::config::AdapterEntry {
                    base_url: base_url.map(str::to_string),
                    default_model: model.map(str::to_string),
                    ..crate::config::AdapterEntry::default()
                },
            );
        }
        cfg
    }

    /// A root-stake roster: one generator, one refuter, both fully delivered.
    fn roster(generator: SeatCandidate, refuter: SeatCandidate) -> RosterSpec {
        RosterSpec {
            stake: CriticalityLevel::Root,
            cross_provider: true,
            generator,
            refuters: vec![refuter],
        }
    }

    /// A seat that declares a full tuple and names the adapter it claims to
    /// sit on. The declaration is written out in full — provider, base_url AND
    /// family — because that is what the gate compares: a base_url the roster
    /// does not name is a base_url no reader of the roster can check.
    fn declaring(
        id: &str,
        role: SeatRole,
        adapter: &str,
        provider: &str,
        base_url: &str,
        family: &str,
    ) -> SeatCandidate {
        let mut s = seat_at(id, role, endpoint_at(provider, base_url, family), id);
        s.adapter = Some(adapter.into());
        s
    }

    /// [`declaring`], but the seat claims NO delivery — the honest shape of a
    /// planned seat at convene, before anything is nucleated. Needed to show
    /// that the no-directory refusal fires on the CLAIM and not on the absence.
    fn undelivered(
        id: &str,
        role: SeatRole,
        adapter: &str,
        provider: &str,
        base_url: &str,
        family: &str,
    ) -> SeatCandidate {
        let mut s = declaring(id, role, adapter, provider, base_url, family);
        if let Some(b) = s.persona.briefing.as_mut() {
            b.injected = false;
        }
        s
    }

    const ANTHROPIC: &str = "https://api.anthropic.com";
    const OPENAI: &str = "https://api.openai.com";

    /// **A1 falsifier.** A roster may not certify its own distinctness.
    ///
    /// The refuter declares family `openai` while `[adapters.costume]` points
    /// at `api.anthropic.com` — the same family as the generator, wearing a
    /// different label. Before the resolution step this roster passed every
    /// witness, because the only thing consulted was the declaration. Delete
    /// the `resolved()` call from `violations()` and this test goes red twice:
    /// no divergence line, and no floor refusal either.
    #[test]
    fn a_declared_family_that_does_not_resolve_is_refused() {
        let ad = adapters(&[
            ("author", Some("https://api.anthropic.com"), None),
            // The costume: an "openai" seat fronting Anthropic.
            ("costume", Some("https://api.anthropic.com"), None),
        ]);
        let spec = roster(
            declaring(
                "gen",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            // Declared as OpenAI; `[adapters.costume]` says otherwise.
            declaring(
                "ref",
                SeatRole::Refuter,
                "costume",
                "openai",
                OPENAI,
                "openai",
            ),
        );

        let v = spec.violations(&ProviderBiasConfig::default(), Some(&ad));
        assert!(
            v.iter().any(|m| m.contains("RESOLVES to")),
            "the divergence between the declared and resolved tuple must be \
             named; got {v:?}"
        );
        assert!(
            v.iter().any(|m| m.contains("cannot be convened")),
            "once resolved, both seats are anthropic — the roster reaches ONE \
             family and must be refused; got {v:?}"
        );
    }

    /// The other direction: a roster whose declarations *do* survive
    /// resolution, on genuinely distinct adapters, is clean. Without this the
    /// test above would pass on a gate that refuses everything.
    #[test]
    fn a_roster_that_resolves_as_declared_is_accepted() {
        let ad = adapters(&[
            ("author", Some("https://api.anthropic.com"), None),
            ("skeptic", Some("https://api.openai.com"), None),
        ]);
        let spec = roster(
            declaring(
                "gen",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            declaring(
                "ref",
                SeatRole::Refuter,
                "skeptic",
                "openai",
                OPENAI,
                "openai",
            ),
        );

        assert_eq!(
            spec.violations(&ProviderBiasConfig::default(), Some(&ad)),
            Vec::<String>::new(),
        );
    }

    // ── F-A — the layer that REPORTS overruled the layer that COMPUTES ──

    /// **The falsifier.** The doctrine's prescribed roster — a generator, one
    /// cross-family floor-bearing reader, and one same-family non-floor-bearing
    /// reader — had no admissible representation anywhere in the system.
    ///
    /// [`plan_committee`] calls that roster fine (`floor_met`,
    /// `floor_is_single_point_of_failure`, asserted by
    /// `the_prescribed_roster_seats_two_but_only_one_bears_the_floor`) and
    /// `violations` then refused it, because every entry of `plan.rejected`
    /// became a violation unconditionally. Measured with both jaws of the
    /// pincer, controls included: the reader ON the ballot earned a
    /// family-collision refusal; the same reader OFF the ballot but seated
    /// earned the unrostered-seat refusal; there was no third state.
    #[test]
    fn the_prescribed_non_floor_bearing_reader_is_reported_not_refused() {
        let ad = adapters(&[
            ("author", Some("https://api.anthropic.com"), None),
            ("skeptic", Some("https://api.openai.com"), None),
            // The second reader: the SAME family as the generator it audits.
            ("echo", Some("https://api.anthropic.com"), None),
        ]);
        let mut spec = roster(
            declaring(
                "fix-molecule",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            declaring(
                "review-codex",
                SeatRole::Refuter,
                "skeptic",
                "openai",
                OPENAI,
                "openai",
            ),
        );
        spec.refuters.push(declaring(
            "review-claude",
            SeatRole::Refuter,
            "echo",
            "anthropic",
            ANTHROPIC,
            "anthropic",
        ));

        let report = spec.report(&ProviderBiasConfig::default(), Some(&ad));
        assert_eq!(
            report.refusals,
            Vec::<String>::new(),
            "the doctrine's own prescription must have an admissible \
             representation; it was refused on the seat the kernel had just \
             modelled as admissibly non-floor-bearing"
        );
        assert!(
            report
                .advisories
                .iter()
                .any(|a| a.contains("review-claude") && a.contains("NOT ON THE BALLOT")),
            "downgrading the refusal may not silence it — the echo must still \
             be named, and named as off the ballot; got {:?}",
            report.advisories
        );
        assert!(
            report
                .advisories
                .iter()
                .any(|a| a.contains("rests entirely on") && a.contains("review-codex")),
            "and the single-point-of-failure floor must be carried too; got {:?}",
            report.advisories
        );
    }

    /// **R2.** The off-ballot advisory must READ its witness axis off the
    /// rejection it is describing, not carry a hardcoded label that happens to
    /// agree with it.
    ///
    /// The sibling branch three lines below already prints
    /// `r.reason.witness_axis()`; this one printed the literal `witness 1`. The
    /// two therefore agreed only for as long as [`SeatRejection::FamilyCollision`]
    /// kept the axis it had on the day the literal was typed — a LABEL standing
    /// where a PROPERTY was in scope, which is this lineage's own defect class.
    ///
    /// The falsifier is a mutation rather than an input, because the branch is
    /// guarded on `FamilyCollision` and no witness-2 rejection can reach it:
    /// move `FamilyCollision`'s axis in
    /// [`SeatRejection::witness_axis`] and the hardcoded form diverges from the
    /// sibling on the same input, while the computed form follows it.
    #[test]
    fn the_off_ballot_advisory_reads_its_axis_off_the_rejection() {
        let ad = adapters(&[
            ("author", Some("https://api.anthropic.com"), None),
            ("skeptic", Some("https://api.openai.com"), None),
            ("echo", Some("https://api.anthropic.com"), None),
        ]);
        let mut spec = roster(
            declaring(
                "fix-molecule",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            declaring(
                "review-codex",
                SeatRole::Refuter,
                "skeptic",
                "openai",
                OPENAI,
                "openai",
            ),
        );
        spec.refuters.push(declaring(
            "review-claude",
            SeatRole::Refuter,
            "echo",
            "anthropic",
            ANTHROPIC,
            "anthropic",
        ));

        let bias = ProviderBiasConfig::default();
        let plan = spec.resolved(Some(&ad)).0.plan(&bias);
        let axis = plan
            .rejected
            .iter()
            .find(|r| r.seat_id == "review-claude")
            .map(|r| r.reason.witness_axis())
            .expect("the echo seat is rejected off the ballot");

        let report = spec.report(&bias, Some(&ad));
        let advisory = report
            .advisories
            .iter()
            .find(|a| a.contains("NOT ON THE BALLOT"))
            .expect("the echo must be named as off the ballot");
        assert!(
            advisory.contains(&format!("witness {axis}")),
            "the advisory must name the axis the rejection actually carries \
             ({axis}); got {advisory}"
        );

        // The same literal was also missing the backslash continuations every
        // neighbouring literal uses, so it emitted the source indentation as
        // runs of embedded spaces. Prose the convener pastes into `roster.md`
        // may not carry the shape of the file it was written in.
        assert!(
            !advisory.contains("  "),
            "a refusal line may not carry source indentation into its prose; \
             got {advisory}"
        );
    }

    /// **The counterweight.** A family collision that COSTS the floor still
    /// refuses. Without this the downgrade above would be decoration: a gate
    /// that cannot fail.
    ///
    /// Same two seats as the doctrine's roster with the cross-family reader
    /// removed — so the only reader is the echo, the floor is unreachable, and
    /// the collision is refused beside the floor line that names what it cost.
    #[test]
    fn a_family_collision_that_costs_the_floor_is_still_a_refusal() {
        let ad = adapters(&[
            ("author", Some("https://api.anthropic.com"), None),
            ("echo", Some("https://api.anthropic.com"), None),
        ]);
        let spec = roster(
            declaring(
                "fix-molecule",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            declaring(
                "review-claude",
                SeatRole::Refuter,
                "echo",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
        );

        let report = spec.report(&ProviderBiasConfig::default(), Some(&ad));
        assert!(
            report
                .refusals
                .iter()
                .any(|v| v.contains("review-claude") && v.contains("family-collision")),
            "a collision that leaves no other reader must still refuse; got {:?}",
            report.refusals
        );
        assert!(
            report
                .refusals
                .iter()
                .any(|v| v.contains("cannot be convened")),
            "and the floor line must name the consequence; got {:?}",
            report.refusals
        );
        assert!(
            report.advisories.is_empty(),
            "a refused roster has nothing to be merely brittle about; got {:?}",
            report.advisories
        );
    }

    // ── F4 — the pin was prose and the worktree was the resolution ──

    fn bases() -> Vec<(String, String)> {
        vec![
            (
                "HEAD".to_string(),
                "e7c8a521fe29aa11bb22cc33dd44ee55ff667788".to_string(),
            ),
            (
                "main".to_string(),
                "4a25558e446dbfe76e2e81a6968285fe1eea3981".to_string(),
            ),
        ]
    }

    /// **The falsifier.** The measured incident: the seat was pinned to
    /// `4a25558e446d` and, dispatched from a worktree at another head, was
    /// handed `e7c8a521`. Nothing reconciled the two — so the resolver must
    /// pick the base that CARRIES the pin, not the ambient one.
    #[test]
    fn the_pin_chooses_the_base_rather_than_the_ambient_head() {
        assert_eq!(
            resolve_reviewed_tree(Some("4a25558e446d"), &bases()),
            ReviewedTreeResolution::Resolved {
                start_point: "main".into(),
                tree: "4a25558e446dbfe76e2e81a6968285fe1eea3981".into(),
            },
        );
        // Full-length spelling, and case, resolve identically.
        assert!(matches!(
            resolve_reviewed_tree(Some("4A25558E446DBFE76E2E81A6968285FE1EEA3981"), &bases()),
            ReviewedTreeResolution::Resolved { .. }
        ));
    }

    /// The load-bearing half: when NO available base carries the pinned tree,
    /// the answer is a refusal — never "review whatever is checked out".
    #[test]
    fn a_pin_no_base_carries_is_unsatisfiable() {
        let r = resolve_reviewed_tree(Some("deadbeefcafe1234"), &bases());
        match r {
            ReviewedTreeResolution::Unsatisfiable { pin, offered } => {
                assert_eq!(pin, "deadbeefcafe1234");
                assert_eq!(
                    offered.len(),
                    2,
                    "the refusal must carry what WAS offered, or its reader \
                     cannot tell a stale checkout from a wrong pin"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// **The counterweight for the whole gate.** A molecule with no pin is
    /// unconstrained — the overwhelming majority of dispatches, and a gate that
    /// touched them would be an outage rather than a control.
    #[test]
    fn an_unpinned_molecule_is_not_constrained() {
        assert_eq!(
            resolve_reviewed_tree(None, &bases()),
            ReviewedTreeResolution::NotPinned
        );
        // Blank and whitespace are absence, not a pin nothing satisfies — the
        // same empty-is-not-a-declaration rule the family axis learned.
        assert_eq!(
            resolve_reviewed_tree(Some("   "), &bases()),
            ReviewedTreeResolution::NotPinned
        );
    }

    /// A pin that is not a tree id is refused, not ignored. Ignoring it is how
    /// a typo becomes an unconstrained dispatch that still LOOKS constrained.
    #[test]
    fn an_unreadable_pin_is_refused_rather_than_ignored() {
        for bad in ["4a255", "not-a-tree", "zzzzzzzz", &"a".repeat(41)] {
            assert!(
                matches!(
                    resolve_reviewed_tree(Some(bad), &bases()),
                    ReviewedTreeResolution::Unreadable { .. }
                ),
                "'{bad}' must be refused as unreadable"
            );
        }
    }

    /// Order is preference, not luck: when two bases carry one tree — a branch
    /// tip and main's merge of it, the ordinary case — the dispatcher's own
    /// first choice wins.
    #[test]
    fn the_first_matching_base_wins() {
        let both = vec![
            ("HEAD".to_string(), "4a25558e446dbfe7".to_string()),
            ("main".to_string(), "4a25558e446dbfe7".to_string()),
        ];
        assert!(matches!(
            resolve_reviewed_tree(Some("4a25558e"), &both),
            ReviewedTreeResolution::Resolved { ref start_point, .. } if start_point == "HEAD",
        ));
    }

    // ── F7 — a re-convocation rebound the seats and not the roster ──

    /// **The falsifier.** The round as it really ran: the floor-bearing seat
    /// collapsed, a replacement was nucleated and sat, and `roster.json` kept
    /// naming the collapsed one as floor-bearing. Every other line in this file
    /// stayed green, because a collapsed molecule has no artefact to be wrong
    /// about.
    #[test]
    fn a_roster_still_naming_a_collapsed_seat_is_refused() {
        let spec = roster(
            declaring(
                "fix-molecule",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            declaring(
                "cmbverify-3035",
                SeatRole::Refuter,
                "skeptic",
                "openai",
                OPENAI,
                "openai",
            ),
        );

        let collapsed = |id: &str| id == "cmbverify-3035";
        let v = spec.reconvocation_violations(&collapsed);
        assert!(
            v.iter()
                .any(|m| m.contains("cmbverify-3035") && m.contains("COLLAPSED")),
            "a roster crediting the floor to a seat that never executed must be \
             refused; got {v:?}"
        );
        assert!(
            v.iter().any(|m| m.contains("replaced_seat_id")),
            "and the refusal must say what to write instead; got {v:?}"
        );
    }

    /// **The counterweight.** The properly rebound roster — the collapsed seat
    /// removed, its replacement carrying both halves of the record — is clean.
    /// Without this the check above would be an outage: a gate that refuses
    /// every re-convocation there is.
    #[test]
    fn a_rebound_roster_naming_the_replacement_is_clean() {
        let mut replacement = declaring(
            "cmbverify-f022",
            SeatRole::Refuter,
            "skeptic",
            "openai",
            OPENAI,
            "openai",
        );
        replacement.replaced_seat_id = Some("cmbverify-3035".into());
        replacement.replacement_reason =
            Some("collapsed on a reviewed-tree mismatch; reached no conclusion".into());
        let spec = roster(
            declaring(
                "fix-molecule",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            replacement,
        );

        let collapsed = |id: &str| id == "cmbverify-3035";
        assert_eq!(
            spec.reconvocation_violations(&collapsed),
            Vec::<String>::new(),
            "the rebound roster is the state this check exists to make reachable"
        );
        // And a roster with no re-convocation at all is untouched.
        let untouched = |_: &str| false;
        assert_eq!(
            spec.reconvocation_violations(&untouched),
            Vec::<String>::new(),
        );
    }

    /// Half a record is not a record. A replacement that names its predecessor
    /// without saying why is refused — that blank is where a quality refusal
    /// gets laundered into a jury failure, which does not consume a round.
    #[test]
    fn a_replacement_with_no_stated_reason_is_refused() {
        let mut replacement = declaring(
            "cmbverify-f022",
            SeatRole::Refuter,
            "skeptic",
            "openai",
            OPENAI,
            "openai",
        );
        replacement.replaced_seat_id = Some("cmbverify-3035".into());
        replacement.replacement_reason = Some("   ".into());
        let spec = roster(
            declaring(
                "fix-molecule",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            replacement,
        );

        let v = spec.reconvocation_violations(&|_: &str| false);
        assert!(
            v.iter().any(|m| m.contains("replacement_reason")),
            "an unstated cause must be refused; got {v:?}"
        );
    }

    /// **R3, the falsifier.** A seat naming ITSELF as the seat it replaced is
    /// refused: a re-convocation seats a different molecule, so a self-naming
    /// replacement is a round consumed with nothing rebound.
    #[test]
    fn a_seat_replacing_itself_is_refused() {
        let v = self_replacement_violations("cmbverify-3035");
        assert!(
            v.iter()
                .any(|m| m.contains("cmbverify-3035") && m.contains("replacing ITSELF")),
            "a seat may not name itself as its own predecessor; got {v:?}"
        );
    }

    /// **R3, the counterweight.** The paired case the clause above shipped
    /// without: a replacement naming a DIFFERENT predecessor draws no
    /// self-replacement refusal. A gate proven only to fail is indistinguishable
    /// from an outage — which is the standard the round-3 fix set for every
    /// other finding it closed, and did not meet for this one.
    #[test]
    fn a_replacement_naming_a_different_predecessor_is_not_refused_as_a_self_replacement() {
        let v = self_replacement_violations("cmbverify-3035-old");
        assert!(
            !v.iter().any(|m| m.contains("replacing ITSELF")),
            "a legitimate re-convocation must pass the self-replacement clause; \
             got {v:?}"
        );
        assert_eq!(
            v,
            Vec::<String>::new(),
            "and it must draw no other re-convocation refusal either"
        );
    }

    /// A roster whose refuter `cmbverify-3035` records replacing
    /// `replaced_seat_id`, with a stated reason and nothing collapsed — so the
    /// ONLY clause that can speak is the self-replacement one, and the two
    /// cases above differ in exactly one byte-string.
    fn self_replacement_violations(replaced_seat_id: &str) -> Vec<String> {
        let mut replacement = declaring(
            "cmbverify-3035",
            SeatRole::Refuter,
            "skeptic",
            "openai",
            OPENAI,
            "openai",
        );
        replacement.replaced_seat_id = Some(replaced_seat_id.into());
        replacement.replacement_reason = Some("collapsed on a tree mismatch".into());
        let spec = roster(
            declaring(
                "fix-molecule",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            replacement,
        );
        spec.reconvocation_violations(&|_: &str| false)
    }

    /// Keeping the collapsed seat *beside* its replacement is refused too —
    /// otherwise the rebinding is satisfiable by adding a line and deleting
    /// nothing, and two seats claim one chair.
    #[test]
    fn a_replaced_seat_may_not_stay_beside_its_replacement() {
        let mut replacement = declaring(
            "cmbverify-f022",
            SeatRole::Refuter,
            "skeptic",
            "openai",
            OPENAI,
            "openai",
        );
        replacement.replaced_seat_id = Some("cmbverify-3035".into());
        replacement.replacement_reason = Some("collapsed on a tree mismatch".into());
        let mut spec = roster(
            declaring(
                "fix-molecule",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            declaring(
                "cmbverify-3035",
                SeatRole::Refuter,
                "skeptic",
                "openai",
                OPENAI,
                "openai",
            ),
        );
        spec.refuters.push(replacement);

        let v = spec.reconvocation_violations(&|id: &str| id == "cmbverify-3035");
        assert!(
            v.iter()
                .any(|m| m.contains("still on the roster") && m.contains("cmbverify-f022")),
            "a replaced seat that stays seated must be refused, and the refusal \
             must name who replaced it; got {v:?}"
        );
    }

    /// A seat naming no adapter is a self-attestation, and is named as one
    /// rather than skipped — otherwise the resolution check above is opt-out
    /// by deleting one field.
    #[test]
    fn a_seat_naming_no_adapter_is_a_self_attestation() {
        let mut spec = roster(
            declaring(
                "gen",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            declaring(
                "ref",
                SeatRole::Refuter,
                "skeptic",
                "openai",
                OPENAI,
                "openai",
            ),
        );
        spec.refuters[0].adapter = None;

        let v = spec.violations(&ProviderBiasConfig::default(), None);
        assert!(
            v.iter()
                .any(|m| m.contains("SELF-ATTESTATION") && m.contains("'ref'")),
            "got {v:?}"
        );
    }

    /// **A3 falsifier.** `injected: true` is a claim about two files, and the
    /// gate checks the files.
    ///
    /// The recipe used to tell the convener to "flip that seat's
    /// `persona.briefing.injected` to true once the durable file exists" — so
    /// the load-bearing field of witness (2) was set by the same hand the
    /// witness exists to audit, exactly like the declared family before A1.
    /// Drop the `with_observed_delivery` call from the lint and this goes red.
    #[test]
    fn a_claimed_injection_that_is_not_on_disk_is_refused() {
        let spec = roster(
            declaring(
                "gen",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            declaring(
                "ref",
                SeatRole::Refuter,
                "skeptic",
                "openai",
                OPENAI,
                "openai",
            ),
        );
        // Both seats claim delivery (the `seat` fixture sets `injected: true`).
        assert!(spec.refuters[0]
            .persona
            .briefing
            .as_ref()
            .is_some_and(|b| b.injected));

        // Disk says the durable posture file was never written.
        let (observed, said) =
            spec.with_observed_delivery(|id| (id == "ref").then(ObservedDelivery::default));
        assert!(
            said.iter()
                .any(|m| m.contains("'ref'") && m.contains("is MISSING")),
            "the divergence must be named; got {said:?}"
        );
        // And the derived flag — not the claimed one — is what gets planned.
        assert!(observed.refuters.iter().all(|s| s
            .persona
            .briefing
            .as_ref()
            .is_some_and(|b| !b.injected)));

        // R3-2. A seat with no directory that CLAIMS delivery is the strongest
        // contradiction available, not the weakest: it asserts two files inside
        // a directory that does not exist. This assertion used to read
        // `assert_eq!(quiet, [])` — it pinned the defect rather than the
        // property, which is why the hole survived the A3 fix that this very
        // test was written to prove.
        let (derived, loud) = spec.with_observed_delivery(|_| None);
        assert!(
            loud.iter()
                .any(|m| m.contains("'ref'") && m.contains("has no molecule directory")),
            "delivery claimed where it cannot have occurred must be named; got {loud:?}"
        );
        assert!(
            derived.refuters.iter().all(|s| s
                .persona
                .briefing
                .as_ref()
                .is_some_and(|b| !b.injected)),
            "…and the claim must not survive into what gets planned"
        );

        // The honest convene, and the proof this refusal can be avoided: a seat
        // with no directory that CLAIMS NOTHING has contradicted nothing. A
        // molecule that does not exist is not an accusation.
        let planned = roster(
            declaring(
                "gen",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            undelivered(
                "ref",
                SeatRole::Refuter,
                "skeptic",
                "openai",
                OPENAI,
                "openai",
            ),
        );
        let (_, quiet) = planned.with_observed_delivery(|id| (id != "ref").then(delivered));
        assert!(
            !quiet.iter().any(|m| m.contains("'ref'")),
            "a planned seat claiming no delivery must not be refused for its \
             absence — that would refuse every honest convene; got {quiet:?}"
        );
    }

    /// A rendered contract reads back as itself, and a placeholder does not
    /// read back at all.
    ///
    /// The round trip is the counterweight: a parser that returned `None` for
    /// everything would satisfy the stub cases below while refusing every real
    /// committee, which is an outage wearing a control's clothes.
    #[test]
    fn a_rendered_contract_parses_and_a_placeholder_does_not() {
        let body = "Audit the artefacts. The generator's confidence is not evidence.";
        let parsed = parse_committee_posture(&render_committee_posture(
            ADVERSARIAL_BRIEFING_VERSION,
            "blake3:deadbeef",
            body,
        ))
        .expect("a rendered contract must read back");
        assert_eq!(parsed.version, ADVERSARIAL_BRIEFING_VERSION);
        assert_eq!(parsed.contract_hash, "blake3:deadbeef");
        assert_eq!(parsed.body, body);

        // A body carrying its own `---` rule must not truncate the parse: the
        // body is what follows the LAST rule.
        let sectioned = "First clause.\n\n---\n\nSecond clause.";
        assert_eq!(
            parse_committee_posture(&render_committee_posture(
                ADVERSARIAL_BRIEFING_VERSION,
                "blake3:deadbeef",
                sectioned,
            ))
            .map(|c| c.body),
            Some("Second clause.".to_string()),
            "a rule inside the body may not silently shorten the contract; the \
             parse must still find A body — and this is the documented limit: \
             what it returns is the last section, not the whole prose"
        );

        for stub in [
            "# posture\n",
            "",
            // Header present, body empty — a contract that says nothing.
            &render_committee_posture(ADVERSARIAL_BRIEFING_VERSION, "blake3:deadbeef", "   "),
            // Body present, hash absent — nothing to match a roster against.
            "# Committee posture\n\n- **contract-version:** 1\n\n---\n\nBe adversarial.\n",
        ] {
            assert_eq!(
                parse_committee_posture(stub),
                None,
                "a placeholder is not a contract; got a parse for:\n{stub}"
            );
        }
    }

    /// **The digest falsifier, in both directions.**
    ///
    /// A gate proven only to fail is indistinguishable from an outage — which
    /// is precisely the fear the removed justification appealed to — so the
    /// passing direction is asserted here beside the refusing one, on the same
    /// body.
    #[test]
    fn a_contract_hash_must_be_the_digest_of_the_body_beneath_it() {
        let body = "Try to make the falsifier — or a sharper one — go red.";

        // PASSES: the hash a convener is told to write verifies.
        let honest = committee_contract_hash(body);
        assert!(
            matches!(
                verify_contract_hash(&honest, body),
                ContractHashVerdict::Verified {
                    algorithm: "blake3"
                }
            ),
            "the hash `committee_contract_hash` computes must verify, or no \
             convener can ever pass this gate; got {:?}",
            verify_contract_hash(&honest, body),
        );
        // …and it verifies through the render/parse round trip, which is the
        // path the witness actually reads — a digest that only holds over the
        // in-memory string would be measuring the property next to the one
        // that matters.
        let parsed = parse_committee_posture(&render_committee_posture(
            ADVERSARIAL_BRIEFING_VERSION,
            &honest,
            body,
        ))
        .expect("a rendered contract must read back");
        assert_eq!(
            verify_contract_hash(&parsed.contract_hash, &parsed.body).refusal(),
            None,
            "the digest must survive the round trip the gate reads through"
        );

        // PASSES: sha256, and the legacy bare-hex shape. Both are the live
        // corpus's dominant forms — 19 of the 20 hashes that verify are not
        // blake3 — so a blake3-only verifier would be the outage.
        let sha_hex = {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(format!("{body}\n").as_bytes()))
        };
        for shape in [
            format!("sha256:{sha_hex}"),
            sha_hex.clone(),
            // A compound prefix, as one live contract carries.
            format!("blake3-substitute:sha256:{sha_hex}"),
        ] {
            assert_eq!(
                verify_contract_hash(&shape, body).refusal(),
                None,
                "a real digest under a supported algorithm must verify: {shape}"
            );
        }

        // REFUSED: the body swapped underneath an honest hash.
        assert!(
            matches!(
                verify_contract_hash(&honest, "Be agreeable."),
                ContractHashVerdict::Forged { .. }
            ),
            "a hash that does not address the prose beneath it is forged"
        );

        // REFUSED: the two shapes the live corpus actually contains — a
        // digest-shaped value that digests nothing (three different contracts
        // shared one such blake3 value), and an algorithm named at the wrong
        // width (`sha256:` + 32 hex, half a sha256).
        for (declared, expectation) in [
            (
                "blake3:7bf518807da36fb368daf21b6bfcbf26979bd717e06a4ad7d7a7add03d21a1d6",
                "is NOT the blake3 digest",
            ),
            ("sha256:5bfb76111c0eeb33a932dd75108e87a4", "64"),
        ] {
            let verdict = verify_contract_hash(declared, body);
            assert!(
                matches!(verdict, ContractHashVerdict::Forged { .. }),
                "{declared} must be refused as forged; got {verdict:?}"
            );
            assert!(
                verdict.refusal().is_some_and(|r| r.contains(expectation)),
                "the refusal must say WHY; got {verdict:?}"
            );
        }

        // REFUSED: an opaque label, and an algorithm nothing here can compute.
        // Both are unverifiable rather than forged — the reader is sent to
        // restate the hash, not to hunt for tampering.
        for declared in ["contract-v1-stable", "blake2b-256:cafe"] {
            assert!(
                matches!(
                    verify_contract_hash(declared, body),
                    ContractHashVerdict::Unverifiable { .. }
                ),
                "`{declared}` cannot be checked and must not be waved through"
            );
        }
    }

    /// The digest leg is load-bearing on the witness, not merely computed:
    /// a seat whose file and roster agree on a forged hash still fails
    /// delivery.
    ///
    /// This is the direction that matters, because agreement between the two
    /// parties is exactly what the old check measured — and it is what a
    /// convener authoring both artefacts gets for free.
    #[test]
    fn a_forged_contract_hash_fails_delivery_even_when_roster_and_file_agree() {
        let spec = roster(
            declaring(
                "gen",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            declaring(
                "ref",
                SeatRole::Refuter,
                "skeptic",
                "openai",
                OPENAI,
                "openai",
            ),
        );

        // Counterweight first: the honest fixture passes the whole witness, so
        // a refusal below cannot be the gate refusing everything.
        let (honest, quiet) = spec.with_observed_delivery(|_| Some(delivered()));
        assert!(
            quiet.is_empty()
                && honest.refuters.iter().all(|s| s
                    .persona
                    .briefing
                    .as_ref()
                    .is_some_and(|b| b.injected)),
            "an honest delivery must still pass; got {quiet:?}"
        );

        // Now the same delivery with a hash the roster and the file BOTH
        // declare and the body does not support. Only the refuter's roster
        // entry moves, so the generator stays honest and any refusal below is
        // attributable to the seat under test.
        let forged_hash = committee_contract_hash("a contract nobody was seated under");
        let mut forged_spec = spec.clone();
        forged_spec.refuters[0].persona.briefing = Some(AdversarialBriefing {
            version: ADVERSARIAL_BRIEFING_VERSION,
            contract_hash: forged_hash.clone(),
            injected: true,
        });
        let forged_file = ObservedDelivery {
            posture_file_exists: true,
            posture: Some(PostureContract {
                version: ADVERSARIAL_BRIEFING_VERSION,
                // The file declares exactly what the roster declares — the
                // comparison the old witness made passes — over a body that
                // digest does not address.
                contract_hash: forged_hash.clone(),
                body: CONTRACT_BODY.into(),
            }),
            pointer: true,
        };
        let (observed, said) = forged_spec.with_observed_delivery(|id| {
            Some(if id == "ref" {
                forged_file.clone()
            } else {
                delivered()
            })
        });
        assert!(
            said.iter()
                .any(|m| m.contains("'ref'") && m.contains("contract-hash")),
            "a forged digest must be refused by name even though the file and \
             the roster agree; got {said:?}"
        );
        assert!(
            observed.refuters.iter().all(|s| s
                .persona
                .briefing
                .as_ref()
                .is_some_and(|b| !b.injected)),
            "the digest leg must be load-bearing on `injected`, not advisory"
        );
    }

    /// **The content falsifier.** Delivery used to be `file.exists()`, so a
    /// seat whose entire contract was `# posture\n` was certified as having
    /// received one — the gate passing while the constrained party says
    /// something EMPTY.
    ///
    /// Both refusable shapes are pinned: a file that is not a contract, and a
    /// well-formed contract that is not THIS seat's. The second is the one that
    /// binds a party other than the roster's author — the file lives in the
    /// seat's directory, the declaration in the convener's.
    #[test]
    fn a_posture_file_that_is_not_the_declared_contract_is_refused() {
        let spec = roster(
            declaring(
                "gen",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            declaring(
                "ref",
                SeatRole::Refuter,
                "skeptic",
                "openai",
                OPENAI,
                "openai",
            ),
        );

        // A file at the path that is not a contract.
        let stub = ObservedDelivery {
            posture_file_exists: true,
            posture: None,
            pointer: true,
        };
        let (observed, said) = spec.with_observed_delivery(|id| {
            Some(if id == "ref" {
                stub.clone()
            } else {
                delivered()
            })
        });
        assert!(
            said.iter()
                .any(|m| m.contains("'ref'") && m.contains("presence is not content")),
            "a placeholder must be named as one; got {said:?}"
        );
        assert!(observed.refuters.iter().all(|s| s
            .persona
            .briefing
            .as_ref()
            .is_some_and(|b| !b.injected)));

        // A well-formed contract that is not the one the roster declares.
        let other = ObservedDelivery {
            posture_file_exists: true,
            posture: Some(PostureContract {
                version: ADVERSARIAL_BRIEFING_VERSION,
                contract_hash: "blake3:some-other-contract".into(),
                body: "Be agreeable.".into(),
            }),
            pointer: true,
        };
        let (_, said) = spec.with_observed_delivery(|id| {
            Some(if id == "ref" {
                other.clone()
            } else {
                delivered()
            })
        });
        assert!(
            said.iter().any(|m| m.contains("'ref'")
                && m.contains("blake3:some-other-contract")
                && m.contains(&committee_contract_hash(CONTRACT_BODY))),
            "the refusal must name both the file's contract and the roster's; \
             got {said:?}"
        );
    }

    /// The other direction: when both facts hold on disk, the claim stands and
    /// the seat is admitted. Without this, the test above passes on a lint that
    /// refuses every roster.
    #[test]
    fn an_injection_that_is_on_disk_is_accepted() {
        let ad = adapters(&[
            ("author", Some(ANTHROPIC), None),
            ("skeptic", Some(OPENAI), None),
        ]);
        let spec = roster(
            declaring(
                "gen",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            declaring(
                "ref",
                SeatRole::Refuter,
                "skeptic",
                "openai",
                OPENAI,
                "openai",
            ),
        );
        let (observed, said) = spec.with_observed_delivery(|_| Some(delivered()));
        assert_eq!(said, Vec::<String>::new());
        assert_eq!(
            observed.violations(&ProviderBiasConfig::default(), Some(&ad)),
            Vec::<String>::new(),
        );
    }

    /// **A2 falsifier, direction 1.** A convene-shaped roster — distinct
    /// declared families, nothing dispatched, so `injected: false` — must be
    /// told it has a contract to deliver and NOT that its floor is unmeetable.
    ///
    /// Count the floor over admitted seats alone and this test goes red: at
    /// convene nothing is admitted, so the roster reads as one family and the
    /// refusal fires on every correctly shaped committee that ever convenes.
    #[test]
    fn a_convene_shaped_roster_owes_a_contract_not_a_wider_floor() {
        let ad = adapters(&[
            ("author", Some("https://api.anthropic.com"), None),
            ("skeptic", Some("https://api.openai.com"), None),
        ]);
        let mut spec = roster(
            declaring(
                "gen",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            declaring(
                "ref",
                SeatRole::Refuter,
                "skeptic",
                "openai",
                OPENAI,
                "openai",
            ),
        );
        // Convene: the seat is planned, no `cs tackle` has run, so no durable
        // contract exists to point at yet.
        spec.refuters[0].persona.briefing = None;

        let v = spec.violations(&ProviderBiasConfig::default(), Some(&ad));
        assert!(
            v.iter().any(|m| m.contains("briefing-not-injected")),
            "the undelivered contract is the true finding; got {v:?}"
        );
        assert!(
            !v.iter().any(|m| m.contains("cannot be convened")),
            "the floor is REACHABLE — the second family is on the roster and \
             merely undispatched. Refusing it here is a bar no convene step can \
             clear; got {v:?}"
        );
    }

    /// **A2 falsifier, direction 2.** A genuinely single-family roster still
    /// fails the floor, dispatched or not. Without this, direction 1 could be
    /// satisfied by deleting the floor line altogether — a gate that cannot
    /// fail rather than one that always does.
    #[test]
    fn a_single_family_roster_fails_the_floor_even_undispatched() {
        let ad = adapters(&[
            ("author", Some("https://api.anthropic.com"), None),
            ("echo", Some("https://api.anthropic.com"), None),
        ]);
        let mut spec = roster(
            declaring(
                "gen",
                SeatRole::Generator,
                "author",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
            declaring(
                "ref",
                SeatRole::Refuter,
                "echo",
                "anthropic",
                ANTHROPIC,
                "anthropic",
            ),
        );
        spec.refuters[0].persona.briefing = None;

        let v = spec.violations(&ProviderBiasConfig::default(), Some(&ad));
        assert!(
            v.iter().any(|m| m.contains("cannot be convened")),
            "one family is one family whether or not it is dispatched; got {v:?}"
        );
    }

    /// The table, both rows of both polarities, asserted as the *inversion* it
    /// is: the SAME cmb-verify word lands on opposite absolute verdicts.
    #[test]
    fn the_same_word_inverts_across_the_two_polarities() {
        use MechanismPolarity::{Defect, Fix};
        use SeatVerdict::{Confirmed, Inconclusive, Refuted};
        assert_eq!(
            map_through_polarity(Fix, Confirmed),
            ConvergeVerdict::Clean,
            "a confirmed FIX holds — nothing found"
        );
        assert_eq!(
            map_through_polarity(Defect, Confirmed),
            ConvergeVerdict::Findings,
            "a confirmed DEFECT reproduces — this is the row that was missing \
             from every table, and reading it as CLEAN files a reproduced defect \
             as clean"
        );
        assert_eq!(
            map_through_polarity(Fix, Refuted),
            ConvergeVerdict::Findings
        );
        assert_eq!(
            map_through_polarity(Defect, Refuted),
            ConvergeVerdict::Clean
        );
        for p in [Fix, Defect] {
            assert_eq!(
                map_through_polarity(p, Inconclusive),
                ConvergeVerdict::Inconclusive,
                "no verdict was reachable, under either polarity"
            );
        }
    }

    /// An unrecognised polarity fails closed exactly like an absent one. A
    /// typo that silently resolved to a default would be the assumption this
    /// field exists to forbid, arriving through the parser instead.
    #[test]
    fn an_unrecognised_polarity_is_not_guessed_into_one() {
        assert_eq!(
            MechanismPolarity::parse(" FIX "),
            Some(MechanismPolarity::Fix)
        );
        assert_eq!(MechanismPolarity::parse("fixed"), None);
        assert_eq!(MechanismPolarity::parse("bug"), None);
        assert_eq!(MechanismPolarity::parse(""), None);
    }

    /// The absolute door is out of scope for the polarity rule, and the parser
    /// is what decides that: `CLEAN` is not a [`SeatVerdict`].
    #[test]
    fn the_absolute_vocabulary_does_not_parse_as_the_relative_door() {
        assert_eq!(
            SeatVerdict::parse("CONFIRMED"),
            Some(SeatVerdict::Confirmed)
        );
        for absolute in ["CLEAN", "FINDINGS", "PASS", "BLOCKED", "INCONCLUSIVE "] {
            let parsed = SeatVerdict::parse(absolute);
            if absolute.trim() == "INCONCLUSIVE" {
                // The one word both doors share — and it maps to itself under
                // either polarity, so the overlap is harmless.
                assert_eq!(parsed, Some(SeatVerdict::Inconclusive));
            } else {
                assert_eq!(
                    parsed, None,
                    "{absolute} is the ABSOLUTE door and must not drag a \
                     polarity requirement onto artefacts it does not govern"
                );
            }
        }
    }

    /// The report line is read for its verdict, not its count.
    #[test]
    fn the_report_line_drops_the_count_and_refuses_anything_else() {
        assert_eq!(
            ConvergeVerdict::from_report_line("VERDICT: FINDINGS (3)"),
            Some(ConvergeVerdict::Findings)
        );
        assert_eq!(
            ConvergeVerdict::from_report_line("  VERDICT: CLEAN  "),
            Some(ConvergeVerdict::Clean)
        );
        assert_eq!(
            ConvergeVerdict::from_report_line("# Referee report"),
            None,
            "a line that is not shaped `VERDICT: <word>` yields no verdict — \
             which the reader treats as a missing report, never as clean"
        );
        assert_eq!(
            ConvergeVerdict::from_report_line("VERDICT: probably fine"),
            None
        );
    }

    /// A round is CLEAN only when every seat is readable AND clean, and an
    /// empty jury is never clean.
    #[test]
    fn a_round_fails_closed_on_an_unreadable_or_empty_jury() {
        let clean_seat = |id: &str| SeatEmission {
            seat_id: id.to_owned(),
            mechanism_polarity: Some(MechanismPolarity::Fix),
            verdict: Some(SeatVerdict::Confirmed),
            reported: Some(ConvergeVerdict::Clean),
        };
        assert!(read_converge_round(&[clean_seat("a"), clean_seat("b")]).clean);

        assert!(
            !read_converge_round(&[]).clean,
            "an empty seat set is NOT clean — there was nobody to find anything, \
             which is the case that used to silently pass"
        );

        let bare = SeatEmission {
            mechanism_polarity: None,
            ..clean_seat("b")
        };
        let round = read_converge_round(&[clean_seat("a"), bare]);
        assert!(!round.clean, "one unreadable seat vacates the round");
        assert_eq!(
            round.refusals,
            vec![("b".to_owned(), SeatReadingRefusal::MissingPolarity)]
        );
    }

    /// A seat that never spoke the relative door is read from its report line
    /// alone — but a seat with no report line at all is refused, because the
    /// rule is an affirmative verdict in BOTH files.
    #[test]
    fn one_file_is_not_both_files() {
        let report_only = SeatEmission {
            seat_id: "a".into(),
            mechanism_polarity: None,
            verdict: None,
            reported: Some(ConvergeVerdict::Clean),
        };
        assert_eq!(
            read_seat_emission(&report_only),
            Ok(ConvergeVerdict::Clean),
            "no relative door was spoken, so there is nothing a polarity would \
             disambiguate"
        );
        let verdict_only = SeatEmission {
            reported: None,
            verdict: Some(SeatVerdict::Confirmed),
            mechanism_polarity: Some(MechanismPolarity::Fix),
            ..report_only
        };
        assert_eq!(
            read_seat_emission(&verdict_only),
            Err(SeatReadingRefusal::NoReport)
        );
    }
}
