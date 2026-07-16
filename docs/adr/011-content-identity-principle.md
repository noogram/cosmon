# ADR-011: Content-Identity Principle

## Status
Proposed

## Context

Across every system we build and every system we rely on, the same architectural
principle keeps surfacing independently: **an artifact's identity should be a
function of its content, not its location.**

Git identifies objects by SHA-1 of their content. IPFS uses content-based
multihashes. Nix derives store paths from the hash of all build inputs. Bazel
uses content digests for remote caching. Within our own ecosystem, OxyMake
identifies pipeline artifacts by hashing their computation graph, and archive-service
addresses messages by content hash.

This is not coincidence. Content-identity solves a class of problems that
location-based identity cannot:

1. **Cache correctness.** If identity = content, then a cache hit is
   _definitionally_ correct. No invalidation protocol, no staleness window,
   no "did the file at this path change?" question. The lookup key _is_ the
   answer.

2. **Deduplication.** Two producers that independently generate the same output
   produce the same identity. No coordination needed. No dedup protocol. The
   hash _is_ the dedup.

3. **Reproducibility.** Verifying that a rebuild produced the correct result is
   a single comparison: does the output hash match? No diffing, no "close
   enough," no test oracle problem.

4. **Distribution.** Content-addressed artifacts can be fetched from _any_
   source -- local cache, remote peer, CDN -- because the address itself
   verifies integrity. Location is a transport concern, not an identity concern.

5. **Immutability.** Content-identity is inherently immutable: changing the
   content changes the identity. This eliminates an entire category of bugs
   where a "same" artifact silently mutates.

Location-based identity (file paths, URLs, database row IDs) conflates _where_
something is with _what_ it is. This conflation is the root cause of cache
invalidation bugs, stale references, phantom dependencies, and "works on my
machine" failures.

### The three flavors

Not all content-identity works the same way. We observe three distinct flavors,
each with different trade-offs:

| Flavor | Identity derived from | Example | Trade-off |
|--------|----------------------|---------|-----------|
| **Content hash** | Raw bytes of the artifact | Git blob, IPFS block | Purest form; identity changes if a single bit changes |
| **Computation hash** | Hash of inputs + transformation | Nix derivation, OxyMake step, Bazel action | Identity is stable across machines that use different intermediate representations; requires hermetic builds |
| **External reference** | Opaque identifier from an authoritative source | DOI, ISSN, crate version | Delegates identity to an external registry; useful when content is not byte-reproducible |

All three share the core property: identity is determined by _what_ the thing
is, not _where_ it lives.

### The mtime+size fast-path

Pure content hashing is expensive for large files. Every system that adopts
content-identity independently discovers the same optimization: **use mtime +
file size as a probabilistic fast-path to avoid re-hashing unchanged files.**

- Git uses stat(2) data (mtime, size, inode) in its index to skip rehashing.
- OxyMake checks mtime+size before computing content hashes.
- Bazel's local action cache uses mtime-based checks.
- Cargo uses mtime fingerprinting for incremental compilation.

The invariant: if mtime and size are unchanged, the content hash is _assumed_
unchanged. This is not cryptographically sound -- it is an optimization that
trades a negligible false-positive risk for a major performance gain. The full
hash remains the source of truth; the fast-path only decides when to skip
recomputation.

### Why formalize this now

Cosmon orchestrates agents that produce, consume, and cache artifacts: formulas,
molecule state, agent snapshots, build outputs. As the framework grows, every
artifact storage decision will face the same question: how do we identify this
thing?

Without an explicit principle, each subsystem will reinvent its own answer --
some using paths, some using hashes, some using database IDs -- creating
inconsistency that compounds into cache bugs, stale references, and
irreproducible behavior. Formalizing the principle now establishes a shared
vocabulary and a default answer before the inconsistencies accumulate.

## Decision

Adopt the **Content-Identity Principle** as a foundational architectural
constraint for Cosmon and related systems:

> **Identity = f(content), not f(location).**
>
> Every persistent artifact SHOULD be identified by a function of its content.
> Location (path, URL, database row) is a _routing_ concern, not an _identity_
> concern.

### Concrete rules

1. **Default to content-identity.** When designing a new artifact type (formula,
   molecule snapshot, build output, cached result), its primary identifier SHOULD
   be derived from its content using one of the three flavors.

2. **Choose the right flavor:**
   - **Content hash** when the artifact is byte-deterministic (serialized state,
     build outputs, immutable blobs).
   - **Computation hash** when the artifact is the result of a transformation
     and input-determinism matters more than byte-determinism (pipeline steps,
     derived data).
   - **External reference** when the artifact originates from an external
     registry and byte-reproducibility is not guaranteed (third-party crate
     versions, academic papers, API responses).

3. **Implement the mtime+size fast-path** for any content-hashing layer that
   operates on local files. The fast-path MUST be bypassable (e.g.,
   `--force-rehash`) for correctness verification.

4. **Separate identity from location.** Artifact storage MAY use
   content-derived paths (e.g., `objects/ab/cdef1234...` a la Git), but the
   path is a consequence of the identity, not the identity itself. Code MUST
   NOT depend on path structure for correctness.

5. **Document the flavor.** Every artifact type's documentation MUST state which
   content-identity flavor it uses and why.

### What this does NOT require

- **Not all identifiers must be content-based.** Session IDs, agent IDs, and
  other _ephemeral_ or _mutable_ entities are correctly identified by assigned
  names. The principle applies to _persistent artifacts_ -- things that could
  be cached, deduplicated, or verified.

- **Not a specific hash algorithm.** The principle is algorithm-agnostic.
  SHA-256, BLAKE3, or xxHash are all valid choices depending on the security
  vs. performance trade-off.

- **Not a ban on paths or URLs.** Location-based references are fine for
  _routing_ (finding where to fetch something). They are not fine for
  _identification_ (deciding what something is).

## Consequences

### Positive

- **Shared vocabulary.** Teams and agents can discuss artifact identity using
  "content hash," "computation hash," and "external ref" without ambiguity.

- **Cache correctness by construction.** Systems built on content-identity
  eliminate cache invalidation as a bug category.

- **Composability.** Content-addressed artifacts compose naturally: a molecule's
  identity can include the hashes of its steps, creating a Merkle DAG that
  captures the full provenance chain.

- **Auditability.** Content-identity creates a verifiable chain from inputs to
  outputs. Any artifact can be independently verified by recomputing its
  identity.

### Negative

- **Hash computation cost.** Content hashing adds CPU overhead. The mtime+size
  fast-path mitigates this for the common case, but the cold-start cost is real.

- **Content-drift in external references.** External references (DOIs, crate
  versions) can silently change their underlying content (yanked and
  re-published crates, updated papers). Pinning and verification checksums
  are needed at system boundaries.

- **Learning curve.** Developers accustomed to location-based identity (file
  paths, database IDs) need to internalize the distinction between routing and
  identity.

## Prior art

| System | Flavor | Identity mechanism |
|--------|--------|--------------------|
| **Git** | Content hash | SHA-1 of blob/tree/commit content |
| **IPFS** | Content hash | Multihash (CID) of block content |
| **Nix** | Computation hash | Hash of all build inputs (derivation) |
| **Bazel** | Computation hash | Action digest (inputs + command hash) |
| **OxyMake** | Computation hash | Hash of step inputs + transformation |
| **archive-service** | Content hash | SHA-256 of message content |
| **Cargo** | Content hash | Crate checksum in `Cargo.lock` |
| **Docker** | Content hash | Layer digest (SHA-256 of tar content) |

## References

- Wheeler, J.A. (1990). "Information, Physics, Quantum: The Search for Links."
  The informational foundation: identity as information, not location.
- Benet, J. (2014). "IPFS - Content Addressed, Versioned, P2P File System."
  The canonical content-addressing system.
- Dolstra, E. (2006). "The Purely Functional Software Deployment Model."
  Nix's computation-hash approach to reproducible builds.
