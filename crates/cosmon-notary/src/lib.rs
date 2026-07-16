// SPDX-License-Identifier: AGPL-3.0-only

//! Notary Protocol v0 — operator-signed content-hash commitments.
//!
//! See [ADR-056](../../../docs/adr/056-notary-protocol-v0.md) for the
//! governing specification. This crate implements the **minimum viable
//! minting surface**: a commitment schema, a canonical byte encoding, a
//! signature trait with an Ed25519 implementation, and local
//! operator-signing. There is no HTTP server, no multi-validator
//! consensus, no zero-knowledge layer, and no token economics —
//! deliberately. Those are phase-2+ concerns; this crate is the
//! primitive the rest of cosmon can rest on.
//!
//! # Honest naming
//!
//! - **Proof-of-Sealed-Presence (`PoSP`).** A signed attestation that a
//!   specific content hash (prompt + briefing seals + formula version
//!   + operator key) existed at a specific moment. Nothing more.
//! - **Proof-of-LLM-Expenditure (`PoLE`).** An economic attestation that
//!   tokens were burned producing the content. Not implemented in v0;
//!   the schema reserves a slot for the `energy_receipt` field that a
//!   future version will populate from `claudion`.
//!
//! Marketing may call the combined artifact "proof-of-cognition"; the
//! code, the types, and the docs in this crate do not.
//!
//! # Why a separate crate
//!
//! Signing is a supply-chain surface. Keeping it isolated from
//! `cosmon-core` means:
//!
//! - `cosmon-core` stays free of cryptographic dependencies (no
//!   `ed25519-dalek` in the type graph used by schedulers, runtimes,
//!   MCP servers).
//! - `cargo deny` can gate changes to `cosmon-notary/Cargo.toml` more
//!   tightly than the rest of the workspace.
//! - Future algorithm swaps (Ed25519 → post-quantum, BLS aggregation)
//!   happen behind the [`signature::Scheme`] trait without rippling.
//!
//! # Phase roadmap
//!
//! | Phase | What this crate does |
//! |-------|----------------------|
//! | 0 (today) | Commitment schema + canonical encoding + Ed25519 local sign/verify. |
//! | 1 | BLS12-381 scheme impl (stub present as [`signature::BlsStub`]); Merkle `parent_commitments`. |
//! | 2 | Remote Noogram validator (HTTP), `NotarizationCertificate` emitted by a second signer. |
//! | 3 | Validator set epochs (sorted-pubkey root), dis-attestation rules, CT-log layer. |
//!
//! # What this crate is *not*
//!
//! - Not a blockchain. There is no consensus, no ordering, no token.
//! - Not a lock. A mint is a trace, not an enforcement point.
//! - Not a PKI. Operator public keys are bring-your-own-filesystem.
//! - Not multi-party. Validators are a phase-2 concern; today the
//!   operator signs their own commitment. The `validator_set_root`
//!   field is present so phase-2 code can land without reshaping the
//!   byte layout.
//!
//! ADR-056 governs. If the ADR and the code disagree, **the ADR
//! wins** — file a bead to fix the code.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod commitment;
pub mod notarization;
pub mod signature;
pub mod verify;

pub use commitment::{Commitment, CommitmentError, CANONICAL_COMMITMENT_VERSION, DOMAIN_SEPARATOR};
pub use notarization::{NotarizationCertificate, NotaryError, Seal};
pub use signature::{Ed25519Scheme, PublicKey, Scheme, Signature, SigningError};
pub use verify::{verify_seal, SealVerifyError};
