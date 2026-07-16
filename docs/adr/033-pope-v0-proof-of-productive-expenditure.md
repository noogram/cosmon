# ADR-033 — PoPE v0: Proof of Productive Expenditure

| Field | Value |
|-------|-------|
| Status | Accepted |
| Date | 2026-04-13 |
| Origin | delib-20260413-fb7b (trajectory deliberation, P4) |
| Falsifier | Tampered receipt rejected by day 25 OR collapse |
| Depends on | ADR-011 (Content-Identity Principle) |

## Context

Every molecule in cosmon consumes cognitive resources (tokens, compute, cost).
The system tracks this via `EnergyRecord` (energy.rs). But tracking is not
verification — nothing prevents a molecule from claiming arbitrary consumption,
and nothing proves to a downstream observer that the claimed work actually
occurred.

**PoPE** (Proof of Productive Expenditure) closes this gap: a cryptographic
receipt binds a molecule's claimed resource consumption to verifiable evidence.
A downstream verifier molecule — or the External Witness (P_external) — can
accept or reject the claim.

### The adversary's constraint (delib-fb7b §1)

> "PoPE 'provider-signed receipts' assumes Anthropic/OpenAI sign receipts.
> They don't. PoPE v0 must specify what to do when the artifact is a screenshot
> of a usage dashboard."

Providers today expose usage data via:
1. **API response headers** — `x-request-id`, token counts in response JSON
2. **Usage dashboards** — web UIs with daily/monthly aggregates
3. **Billing invoices** — monthly PDFs

None of these are cryptographically signed. PoPE v0 must work without
provider signatures while being extensible to provider signatures when they
arrive.

## Decision

### Three attestation levels

PoPE v0 defines three attestation levels, each with different trust properties:

| Level | Name | Evidence | Trust model | v0 support |
|-------|------|----------|-------------|------------|
| L0 | Self-attested | Worker's own `EnergyRecord` | Trust the worker | Yes |
| L1 | Operator-attested | Operator cross-references provider dashboard/invoice | Trust the operator | Yes |
| L2 | Provider-signed | Cryptographic signature from the API provider | Trust the provider's key | Spec only |

**L0** is what exists today — the worker reports its own consumption. It is
trivially forgeable by the worker but still useful: it creates a verifiable
paper trail that can be audited after the fact.

**L1** is the adversary-demanded level: the operator (External Witness, per
P_external) independently verifies the worker's claims against provider
evidence. The evidence is a screenshot, invoice, or API usage export. The
operator signs the receipt with their own key (HMAC-SHA256 over the receipt
payload). This is the "screenshot of a usage dashboard" answer.

**L2** is the target state: the API provider includes a signed receipt in
the response. PoPE v0 specifies the type structure but does not implement
verification because no provider offers this today.

### Receipt structure

A `Receipt` binds:
- **Who**: molecule ID + worker ID
- **What**: input tokens, output tokens, cost, model
- **When**: timestamp
- **Evidence hash**: SHA-256 of the evidence artifact (API response JSON,
  screenshot PNG, invoice PDF)
- **Attestation level**: L0 / L1 / L2
- **Signature**: HMAC-SHA256 over the canonical payload

The canonical payload is the receipt fields serialized as sorted-key JSON
(deterministic), then hashed with SHA-256. The signature is HMAC-SHA256
of that hash using the signer's key.

### Verification

A `ReceiptVerifier` checks:
1. Recompute the canonical payload hash from the receipt fields
2. Verify the HMAC signature against the expected key
3. If evidence hash is present, verify the evidence artifact matches
4. Reject if any field has been tampered with

**The falsifier**: construct a valid receipt, tamper with one field (e.g.
inflate token count), re-serialize. The verifier MUST reject it.

### Certificate Transparency (future)

Per the adversary's recommendation (CT, RFC 6962), PoPE v1 should append
receipts to an append-only log with Merkle consistency proofs. This enables
third parties to audit the full expenditure history without trusting any
single party. v0 does not implement this but the receipt structure is
designed to be CT-compatible (content-addressed, append-only).

## Consequences

- New module `cosmon-core::pope` with pure domain types (zero I/O)
- `Receipt` type is serde-serializable and content-addressable
- `ReceiptVerifier` trait allows pluggable verification backends
- HMAC-based verification is the v0 default (operator's key)
- L1 attestation answers the adversary's "screenshot" concern: the operator
  is the bridge between unstructured provider evidence and structured receipts
- L2 is forward-compatible: when providers add signing, the receipt type
  already has the `ProviderSigned` variant
- Integration with `EnergyRecord`: a receipt wraps an energy record with
  cryptographic binding
