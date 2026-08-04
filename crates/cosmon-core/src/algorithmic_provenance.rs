// SPDX-License-Identifier: AGPL-3.0-only

//! **task-20260729-7dd4** — the algorithmic-provenance record: what a
//! conclusion-producing node must say about *the method that produced it*,
//! beyond the name of the model.
//!
//! # Why a model id is not provenance
//!
//! [`crate::event_v2::EventV2::ModelObserved`] already pins the **identity** of
//! the method: the concrete id an adapter reported running
//! (`crate::model_realization`). That settles *which* method, and the honesty
//! invariant there is structural — silence is expressed by not emitting.
//!
//! It does not settle whether the method is **reliable** or **reproducible**,
//! which is the other half of what an opposable artefact needs. A hosted
//! model's version label is a string its vendor asserts and may re-point in
//! silence; nothing in the id says at what temperature it decoded, under what
//! quantization, or over what prompt — and in an adversarial setting the prompt
//! is partly written by the adversary, so the prompt *is* part of the
//! algorithm. A chain of custody that names only the model has pinned the
//! signature and left the function body unstated.
//!
//! The requirement was raised by the sporarium crisis-spore bench (its
//! `crisis-spore-seal-property-register.md` §2.6 and
//! `crisis-spore-legal-regulatory-register.md` §3.1), which recorded it and
//! explicitly refused to patch around it locally. This module is the runtime's
//! answer: the field is grown here, once, rather than reinvented per bench.
//!
//! # The counter-intuitive inversion, made computable
//!
//! A self-hosted fallback model — the path a safety review treats as the risky
//! one — is *more* verifiable than a frontier hosted model, from the algorithmic
//! -integrity angle alone: its weights are hashable, its decoding is pinnable,
//! its run is replayable. The model one trusts most for alignment is the one
//! one can prove least about for admissibility.
//!
//! That inversion is not a remark in a doc here; it is what
//! [`AlgorithmicProvenance::is_algorithm_replayable`] computes, and
//! `tests::the_self_hosted_fallback_outranks_the_hosted_frontier_model` is
//! the executable statement of it.
//!
//! # Never fabricate, always declare
//!
//! Every field of the record is a [`Disclosure`]: either the runtime
//! **observed** the value, or it names, on the wire, the reason it could not.
//! There is deliberately no "absent" spelling that a reader could mistake for
//! "nobody bothered" — the doctrine the bench applied to itself ("either the
//! runtime grows the field, or it is said, in writing, that it does not have
//! it") is enforced by the type. The one field with no undisclosed arm is
//! [`WeightsProvenance`]: a node either pins a weights digest or declares
//! `hosted_unverifiable`, and both of those are statements. Not answering is
//! not among the options.
//!
//! # Universality
//!
//! This record is scoped to *any* node that produces a conclusion, not to
//! fallback events. Fallback-specific facts (what triggered it, which
//! transition, which attempt) stay on the fallback events where they belong;
//! this subset rides the realized-model observation, which every dispatch
//! emits. See `docs/adr/169-algorithmic-provenance-rides-the-realized-model-observation.md`.

use serde::{Deserialize, Serialize};

/// Why a provenance field could not be filled in.
///
/// A reason, never an apology: the reader of a chain of custody needs to tell
/// "the vendor does not expose this" from "this deployment was not configured
/// to record it", because only the second is fixable by the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UndisclosedReason {
    /// The model runs behind a hosted API that does not expose the fact. No
    /// configuration change reaches it; only changing provider does.
    HostedProvider,
    /// The runtime *could* record it, but this dispatch did not set it — the
    /// value in force is the provider's own unstated default. An operator can
    /// fix this by pinning the parameter explicitly.
    NotSetByCosmon,
    /// The adapter's side-channel is silent on this fact (the session-log
    /// adapters report a model id and nothing else about the decode).
    AdapterSilent,
}

impl UndisclosedReason {
    /// A compact, stable tag for tables and machine consumers.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::HostedProvider => "hosted_provider",
            Self::NotSetByCosmon => "not_set_by_cosmon",
            Self::AdapterSilent => "adapter_silent",
        }
    }
}

/// A provenance field the runtime either observed or explicitly declined to
/// claim.
///
/// The alternative — `Option<T>` — spells the negative case as *nothing*, and
/// nothing is exactly what an opposable record may not contain: a reader cannot
/// distinguish an unset field from a field the writer never had. `Undisclosed`
/// carries a reason, so the absence is itself a recorded statement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disclosure<T> {
    /// The runtime observed this value at the seam that produced the
    /// conclusion.
    Observed(T),
    /// The runtime does not have this value, and says why.
    Undisclosed(UndisclosedReason),
}

impl<T> Disclosure<T> {
    /// The observed value, or `None` when the field was declared undisclosed.
    ///
    /// Reading the negative case as `None` is fine *here*, at the point of use;
    /// what the type prevents is storing it that way on the wire.
    #[must_use]
    pub fn observed(&self) -> Option<&T> {
        match self {
            Self::Observed(v) => Some(v),
            Self::Undisclosed(_) => None,
        }
    }

    /// True when the runtime actually observed the value.
    #[must_use]
    pub fn is_observed(&self) -> bool {
        matches!(self, Self::Observed(_))
    }
}

/// What is known about the weights that produced a conclusion.
///
/// There is no third arm and no `Option`: every conclusion-producing node
/// answers this question, and `hosted_unverifiable` is an answer. A hosted
/// model's *version label* is not a substitute — it is a string the vendor
/// asserts, and it can be re-pointed at different weights without the label
/// changing, which is precisely the silent drift a chain of custody exists to
/// exclude.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum WeightsProvenance {
    /// The weights are pinned by content digest — the artefact can be
    /// re-obtained and checked byte for byte.
    Pinned {
        /// Digest algorithm name, lowercase (`sha256`, `blake3`).
        algorithm: String,
        /// Lowercase hex digest of the weights artefact.
        digest: String,
    },
    /// The weights sit behind a hosted API and cannot be hashed from here.
    ///
    /// This is the honest declaration a frontier hosted model must carry, and
    /// it is what makes the inversion visible: the self-hosted fallback beside
    /// it can carry [`Self::Pinned`].
    HostedUnverifiable {
        /// The provider or endpoint whose assertion the record rests on, so a
        /// reader knows *whose* word is load-bearing.
        asserted_by: String,
    },
}

impl WeightsProvenance {
    /// True only when the weights are pinned by digest — the one arm under
    /// which the artefact can be independently re-obtained.
    #[must_use]
    pub fn is_verifiable(&self) -> bool {
        matches!(self, Self::Pinned { .. })
    }
}

/// The decoding parameters in force for a conclusion.
///
/// Each is a [`Disclosure`] rather than an `Option` for the reason given on
/// that type: a hosted default that cosmon never set is a different fact from a
/// temperature of zero, and a record that spells both as "missing" cannot be
/// relied on downstream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecodingParams {
    /// Sampling temperature.
    pub temperature: Disclosure<f64>,
    /// Nucleus-sampling cutoff.
    pub top_p: Disclosure<f64>,
    /// The RNG seed. Without it, `temperature > 0` is unreplayable even with
    /// identical weights and prompt.
    pub seed: Disclosure<i64>,
}

impl DecodingParams {
    /// The record for a dispatch that set no decoding parameter at all — the
    /// honest shape for an adapter riding the provider's unstated defaults.
    #[must_use]
    pub fn undisclosed(reason: UndisclosedReason) -> Self {
        Self {
            temperature: Disclosure::Undisclosed(reason),
            top_p: Disclosure::Undisclosed(reason),
            seed: Disclosure::Undisclosed(reason),
        }
    }

    /// True when the decode is pinned tightly enough to be re-run: every
    /// parameter observed, **or** a temperature of exactly zero with a
    /// disclosed `top_p` (a greedy decode needs no seed).
    ///
    /// The greedy exemption is not a courtesy — at `temperature == 0.0` the
    /// sampler is deterministic, so demanding a seed there would mark a
    /// genuinely replayable run as unreplayable.
    #[must_use]
    pub fn is_replayable(&self) -> bool {
        // `<= 0.0` rather than `== 0.0`: temperature is non-negative, so the
        // only value this admits beyond zero is a nonsensical one, and an exact
        // float comparison here would be the more fragile spelling.
        let greedy = self.temperature.observed().is_some_and(|t| *t <= 0.0);
        self.temperature.is_observed()
            && self.top_p.is_observed()
            && (greedy || self.seed.is_observed())
    }
}

/// The digest of the prompt context that produced a conclusion.
///
/// The prompt is part of the algorithm, and in a crisis it is partly written by
/// the adversary — so a record that pins weights and decoding but not the
/// prompt has pinned everything except the input the attacker controls.
///
/// A **digest**, never the bytes: the prompt of an incident response carries
/// the incident, and a chain of custody must be publishable without
/// re-disclosing what it is about.
pub type PromptContextDigest = Disclosure<ContextDigest>;

/// A content digest of the prompt context — algorithm plus lowercase hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextDigest {
    /// Digest algorithm name, lowercase (`sha256`).
    pub algorithm: String,
    /// Lowercase hex digest.
    pub digest: String,
}

impl ContextDigest {
    /// SHA-256 over the given bytes, which is what the provider adapters use
    /// for the serialized request context.
    #[must_use]
    pub fn sha256(bytes: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        Self {
            algorithm: "sha256".to_owned(),
            digest: format!("{:x}", Sha256::digest(bytes)),
        }
    }
}

/// Whether the procedure that produced a conclusion can be re-run, and how.
///
/// [`Self::Undetermined`] is a real answer, not a hole: it says the runtime did
/// not evaluate replayability for this node, which a reader must be able to
/// tell from "evaluated and found unreplayable".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Reproducibility {
    /// The run can be reproduced; `procedure` states how (the command, the
    /// pinned artefacts, the ordering constraints).
    Replayable {
        /// How to re-run it, in one sentence a human can follow.
        procedure: String,
    },
    /// The run cannot be reproduced, and the reason is recorded.
    NotReplayable {
        /// What makes it unreplayable (hosted weights, no seed, live tool I/O).
        reason: String,
    },
    /// The runtime did not evaluate replayability for this node.
    Undetermined,
}

/// The algorithmic-provenance subset carried by every node that produces a
/// conclusion.
///
/// Universal by construction: it rides the realized-model observation, which is
/// emitted for every dispatch, so it is not a property of the fallback path.
/// See the module docs for the doctrine and the ADR for the placement decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AlgorithmicProvenance {
    /// The weights that ran — pinned by digest, or declared unverifiable.
    pub weights: WeightsProvenance,
    /// The quantization in force (`fp16`, `q4_K_M`, …), or why it is unknown.
    pub quantization: Disclosure<String>,
    /// Temperature / top-p / seed.
    pub decoding: DecodingParams,
    /// Digest of the prompt context — the adversary-writable part of the
    /// algorithm.
    pub prompt_context: PromptContextDigest,
    /// Whether the procedure can be re-run.
    pub reproducibility: Reproducibility,
}

impl AlgorithmicProvenance {
    /// The provenance of a conclusion produced behind a hosted API that
    /// discloses none of this — the honest floor for a frontier adapter.
    ///
    /// `asserted_by` names whose word the weights rest on (the provider or
    /// endpoint). The prompt digest is *not* filled in here: a caller that
    /// holds the request bytes should call [`Self::with_prompt_context`], and a
    /// caller that does not must not invent one.
    #[must_use]
    pub fn hosted_unverifiable(asserted_by: impl Into<String>) -> Self {
        Self {
            weights: WeightsProvenance::HostedUnverifiable {
                asserted_by: asserted_by.into(),
            },
            quantization: Disclosure::Undisclosed(UndisclosedReason::HostedProvider),
            decoding: DecodingParams::undisclosed(UndisclosedReason::NotSetByCosmon),
            prompt_context: Disclosure::Undisclosed(UndisclosedReason::AdapterSilent),
            reproducibility: Reproducibility::NotReplayable {
                reason: "hosted weights cannot be pinned by digest".to_owned(),
            },
        }
    }

    /// The provenance floor for a **session-log adapter** (`claude`, `codex`):
    /// a subprocess whose only side-channel is a transcript naming a model id.
    ///
    /// Distinct from [`Self::hosted_unverifiable`] in the *reason* it gives,
    /// which is the whole point of recording reasons: an in-process provider
    /// could pin a temperature and chooses not to
    /// ([`UndisclosedReason::NotSetByCosmon`], fixable by an operator), whereas
    /// nothing an operator sets makes a session log report the decode
    /// ([`UndisclosedReason::AdapterSilent`], fixable only by changing adapter).
    /// Collapsing the two would tell an auditor to go fix something unfixable.
    #[must_use]
    pub fn adapter_silent(asserted_by: impl Into<String>) -> Self {
        Self {
            weights: WeightsProvenance::HostedUnverifiable {
                asserted_by: asserted_by.into(),
            },
            quantization: Disclosure::Undisclosed(UndisclosedReason::AdapterSilent),
            decoding: DecodingParams::undisclosed(UndisclosedReason::AdapterSilent),
            prompt_context: Disclosure::Undisclosed(UndisclosedReason::AdapterSilent),
            reproducibility: Reproducibility::NotReplayable {
                reason: "the adapter's session log reports a model id and nothing else \
                         about the decode"
                    .to_owned(),
            },
        }
    }

    /// Attach the digest of the prompt context that produced the conclusion.
    ///
    /// Takes already-computed bytes rather than a prompt object: this crate is
    /// I/O-free and the serialization of a request is the adapter's business.
    #[must_use]
    pub fn with_prompt_context(mut self, bytes: &[u8]) -> Self {
        self.prompt_context = Disclosure::Observed(ContextDigest::sha256(bytes));
        self
    }

    /// Replace the decoding parameters (an adapter that pins them).
    #[must_use]
    pub fn with_decoding(mut self, decoding: DecodingParams) -> Self {
        self.decoding = decoding;
        self
    }

    /// Replace the weights provenance (a self-hosted adapter that hashes them).
    #[must_use]
    pub fn with_weights(mut self, weights: WeightsProvenance) -> Self {
        self.weights = weights;
        self
    }

    /// Replace the quantization disclosure.
    #[must_use]
    pub fn with_quantization(mut self, quantization: Disclosure<String>) -> Self {
        self.quantization = quantization;
        self
    }

    /// Replace the reproducibility verdict.
    #[must_use]
    pub fn with_reproducibility(mut self, reproducibility: Reproducibility) -> Self {
        self.reproducibility = reproducibility;
        self
    }

    /// Whether the *algorithm* — not the answer — could be re-run to the same
    /// output by a third party holding this record.
    ///
    /// Requires all three legs: pinned weights, a replayable decode, and an
    /// observed prompt digest. This is the predicate that inverts the intuition
    /// about which model is the safe one to have used.
    ///
    /// It is deliberately independent of [`Self::reproducibility`], which is
    /// what the *emitter claimed*: an audit compares the two, and a claim of
    /// `Replayable` over a record that fails this check is exactly the
    /// discrepancy worth surfacing.
    #[must_use]
    pub fn is_algorithm_replayable(&self) -> bool {
        self.weights.is_verifiable()
            && self.decoding.is_replayable()
            && self.prompt_context.is_observed()
    }

    /// One line for an operator: the computed verdict, and the gaps that
    /// produced it.
    ///
    /// The gaps are named rather than counted because they are not
    /// interchangeable — a missing seed is a flag away, a missing weights hash
    /// means changing provider — and a reader deciding whether an artefact is
    /// opposable needs to know which kind they have.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.is_algorithm_replayable() {
            return "algorithm replayable — weights, decode and prompt all pinned".to_owned();
        }
        let gaps = self.undisclosed_fields();
        if gaps.is_empty() {
            // Every field disclosed, yet not replayable: the decode is pinned
            // but sampled without a seed, or the weights are hosted. The
            // enumeration above would say nothing, so name the verdict alone.
            return "algorithm not replayable".to_owned();
        }
        format!(
            "algorithm not replayable — undisclosed: {}",
            gaps.join(", ")
        )
    }

    /// The fields this record does **not** carry, as stable slugs — what an
    /// audit reads to decide whether the gap is fixable here or requires a
    /// different provider.
    #[must_use]
    pub fn undisclosed_fields(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.weights.is_verifiable() {
            out.push("weights_hash");
        }
        if !self.quantization.is_observed() {
            out.push("quantization");
        }
        if !self.decoding.temperature.is_observed() {
            out.push("temperature");
        }
        if !self.decoding.top_p.is_observed() {
            out.push("top_p");
        }
        if !self.decoding.seed.is_observed() {
            out.push("seed");
        }
        if !self.prompt_context.is_observed() {
            out.push("prompt_context_hash");
        }
        if matches!(self.reproducibility, Reproducibility::Undetermined) {
            out.push("reproducibility");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The record a self-hosted model can produce: weights hashed, decode
    /// pinned, prompt digested.
    fn self_hosted() -> AlgorithmicProvenance {
        AlgorithmicProvenance::hosted_unverifiable("ollama@localhost")
            .with_weights(WeightsProvenance::Pinned {
                algorithm: "sha256".to_owned(),
                digest: "ab".repeat(32),
            })
            .with_quantization(Disclosure::Observed("q4_K_M".to_owned()))
            .with_decoding(DecodingParams {
                temperature: Disclosure::Observed(0.7),
                top_p: Disclosure::Observed(0.95),
                seed: Disclosure::Observed(42),
            })
            .with_prompt_context(b"the incident prompt")
            .with_reproducibility(Reproducibility::Replayable {
                procedure: "re-run against the pinned weights with the recorded seed".to_owned(),
            })
    }

    /// The inversion the bench asked not to lose: the *fallback* model — the
    /// path a safety review treats as risky — is the one whose algorithm can be
    /// re-run, and the frontier hosted model is not.
    #[test]
    fn the_self_hosted_fallback_outranks_the_hosted_frontier_model() {
        let frontier = AlgorithmicProvenance::hosted_unverifiable("api.anthropic.com")
            .with_prompt_context(b"the incident prompt");

        assert!(
            self_hosted().is_algorithm_replayable(),
            "pinned weights + pinned decode + digested prompt is replayable"
        );
        assert!(
            !frontier.is_algorithm_replayable(),
            "a hosted frontier model cannot be replayed however trusted it is"
        );
    }

    #[test]
    fn a_hosted_record_names_every_field_it_lacks() {
        let hosted = AlgorithmicProvenance::hosted_unverifiable("api.openai.com");
        assert_eq!(
            hosted.undisclosed_fields(),
            vec![
                "weights_hash",
                "quantization",
                "temperature",
                "top_p",
                "seed",
                "prompt_context_hash",
            ],
            "the gaps are enumerated, never silent"
        );
        // `reproducibility` is absent from the list: the hosted floor states a
        // verdict (`NotReplayable`), which is a disclosure, not a hole.
        assert!(matches!(
            hosted.reproducibility,
            Reproducibility::NotReplayable { .. }
        ));
    }

    #[test]
    fn a_fully_disclosed_record_has_no_gaps() {
        assert!(self_hosted().undisclosed_fields().is_empty());
    }

    /// A greedy decode is deterministic, so it is replayable without a seed —
    /// demanding one would mark a genuinely reproducible run as not.
    #[test]
    fn greedy_decoding_is_replayable_without_a_seed() {
        let greedy = DecodingParams {
            temperature: Disclosure::Observed(0.0),
            top_p: Disclosure::Observed(1.0),
            seed: Disclosure::Undisclosed(UndisclosedReason::NotSetByCosmon),
        };
        assert!(greedy.is_replayable());

        let sampled = DecodingParams {
            temperature: Disclosure::Observed(0.7),
            ..greedy
        };
        assert!(
            !sampled.is_replayable(),
            "a sampled decode without a seed cannot be re-run"
        );
    }

    /// The claim and the computed verdict are separate axes on purpose: an
    /// emitter that claims `Replayable` over an unverifiable record must remain
    /// detectable.
    #[test]
    fn a_false_replayability_claim_is_detectable() {
        let lying = AlgorithmicProvenance::hosted_unverifiable("api.example.com")
            .with_reproducibility(Reproducibility::Replayable {
                procedure: "trust us".to_owned(),
            });
        assert!(matches!(
            lying.reproducibility,
            Reproducibility::Replayable { .. }
        ));
        assert!(
            !lying.is_algorithm_replayable(),
            "the computed verdict must not inherit the emitter's claim"
        );
    }

    /// The prompt is digested, never carried: an incident-response chain of
    /// custody has to be publishable without re-disclosing the incident.
    #[test]
    fn the_prompt_is_carried_as_a_digest_not_as_bytes() {
        let secret = b"exfiltrated credentials for host db-01";
        let p = AlgorithmicProvenance::hosted_unverifiable("api.example.com")
            .with_prompt_context(secret);
        let wire = serde_json::to_string(&p).unwrap();
        assert!(
            !wire.contains("db-01"),
            "prompt bytes must not reach the wire"
        );
        assert!(wire.contains(&ContextDigest::sha256(secret).digest));
    }

    #[test]
    fn the_prompt_digest_is_stable_and_discriminating() {
        assert_eq!(ContextDigest::sha256(b"a"), ContextDigest::sha256(b"a"));
        assert_ne!(ContextDigest::sha256(b"a"), ContextDigest::sha256(b"b"));
        assert_eq!(ContextDigest::sha256(b"a").algorithm, "sha256");
    }

    /// The whole record round-trips through JSON — it rides an event, so a
    /// reader on the other side of `events.jsonl` must reconstruct it exactly.
    #[test]
    fn the_record_round_trips_through_json() {
        let p = self_hosted();
        let wire = serde_json::to_string(&p).unwrap();
        assert_eq!(
            serde_json::from_str::<AlgorithmicProvenance>(&wire).unwrap(),
            p
        );
    }

    /// An undisclosed field is a *statement with a reason* on the wire, not an
    /// omitted key — that is the whole argument for `Disclosure` over `Option`.
    #[test]
    fn an_undisclosed_field_states_its_reason_on_the_wire() {
        let wire = serde_json::to_string(&AlgorithmicProvenance::hosted_unverifiable("x")).unwrap();
        assert!(wire.contains("undisclosed"));
        assert!(wire.contains("hosted_provider"));
        assert!(wire.contains("not_set_by_cosmon"));
    }

    #[test]
    fn the_summary_names_the_verdict_and_the_gaps() {
        assert_eq!(
            self_hosted().summary(),
            "algorithm replayable — weights, decode and prompt all pinned"
        );
        let hosted = AlgorithmicProvenance::hosted_unverifiable("api.example.com").summary();
        assert!(hosted.starts_with("algorithm not replayable — undisclosed: weights_hash"));
        assert!(hosted.contains("prompt_context_hash"));
    }

    /// The session-log floor and the hosted floor differ in the *reason* they
    /// give, which is what tells an auditor whether the gap is theirs to fix.
    #[test]
    fn a_silent_adapter_and_a_hosted_api_give_different_reasons() {
        let silent = AlgorithmicProvenance::adapter_silent("claude");
        let hosted = AlgorithmicProvenance::hosted_unverifiable("api.anthropic.com");
        assert_eq!(
            silent.decoding.temperature,
            Disclosure::Undisclosed(UndisclosedReason::AdapterSilent),
        );
        assert_eq!(
            hosted.decoding.temperature,
            Disclosure::Undisclosed(UndisclosedReason::NotSetByCosmon),
            "an in-process provider could pin a temperature and did not — that \
             is an operator-fixable gap, unlike a silent session log"
        );
        assert_eq!(silent.undisclosed_fields(), hosted.undisclosed_fields());
    }

    #[test]
    fn undisclosed_reason_tags_are_stable() {
        assert_eq!(UndisclosedReason::HostedProvider.tag(), "hosted_provider");
        assert_eq!(UndisclosedReason::NotSetByCosmon.tag(), "not_set_by_cosmon");
        assert_eq!(UndisclosedReason::AdapterSilent.tag(), "adapter_silent");
    }
}
