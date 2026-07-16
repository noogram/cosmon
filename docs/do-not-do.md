# Do Not Do

This document records design and architectural choices that have been
explicitly rejected after deliberation. Each item captures a rule, the
structural reasons it exists, and references to the deliberations that
produced it.

The goal is not to prohibit exploration but to prevent the reintroduction
of patterns whose failure modes are already understood. Before challenging
an item, read the linked deliberations and propose a successor ADR.

---

## 1. No blockchain / smart contracts in the cosmon-adjacent reciprocity enforcement path

**Rule.** Do not introduce a dependency on a blockchain (public or
proprietary) nor smart contracts in the enforcement path of the
reciprocity / copyleft mechanism of cosmon-adjacent projects.

**Structural reasons.**

1. *Fusion of orthogonal invariants.* The triptych trademark +
   NPL-1.0 §K4/§K5/§K6 + transparency log signed 2-of-3 Ed25519 + OTS
   Bitcoin anchor works by orthogonality (law ⊥ contract ⊥ cryptography)
   with independent failure modes. A smart contract fuses them into a
   single artifact exposed to a single surface. Anti-pattern isomorphic
   to the native-token option rejected 11/11 in delib-6e50.
2. *Rice-undecidability not lifted.* The reciprocity predicates
   (*"same software"*, *"commercial use"*) are undecidable; the contract
   is confined to the syntactic layer, which is already saturated
   off-chain.
3. *Oracle = disguised SPOF.* All security falls back on a signed
   endpoint that already exists as 2-of-3 Ed25519 without on-chain
   finality.
4. *Violation of §K6 private-use carve-out* by construction.
5. *Over-capacity channel = free attack surface* (delib-89dc C1/C8):
   ≤0 useful bits for 10³–10⁴ attack bits; ΔMTTF negative.
6. *GDPR / right-to-be-forgotten tension* vs. on-chain immutability.
7. *Cargo-cult pattern* (Denuvo, FlexLM, HASP, SafeNet) ported onto a
   Web3 substrate.

**References.** delib-6e50 (C1/C6), delib-89dc (C1/C2/C8), delib-e2f0
(option C rejected 11/11), delib-20260415-8fc0 (8/8).
