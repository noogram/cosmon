// SPDX-License-Identifier: AGPL-3.0-only

//! The seal-verification contract (ADR-140 D4, N4).
//!
//! Per ADR-139 D3 a spore's TLA+ seal must gate [`expand`](mod@super::expand) and
//! **fail closed**. But the machine running a germination may not have a JRE /
//! TLC available (the workshop prototype's seal has never been TLC-checked for
//! exactly this reason). This module makes the three states **honest and
//! visible** rather than silently passing:
//!
//! | seal state | meaning | gate behaviour |
//! |------------|---------|----------------|
//! | **absent** | no `[spore.seal]` block | germinate, report `seal: none` |
//! | **present + checked** | `.tla` proof verified (this run or a cached BLAKE3 of a prior pass) | germinate, report `seal: verified <hash>` |
//! | **present + unchecked** | `.tla` present, TLC unavailable or not yet run | germinate ONLY under `--allow-unchecked-seal`, report `seal: present, NOT verified`; default is to **refuse** |
//!
//! # The honesty invariant
//!
//! **A germination never claims a seal is verified when it is not.** A
//! present-but-unverifiable seal does not silently degrade to "good"; it forces
//! the operator to either provide TLC or explicitly opt into the risk with
//! `--allow-unchecked-seal`. The default is fail-closed: a sealed spore on a
//! JRE-less machine refuses to germinate rather than germinate while implying a
//! proof it never ran. A seal whose proof TLC actually **rejected** is worse
//! still: it refuses **unconditionally**, the opt-in flag does not apply, because
//! a broken proof is a known-unsafe generator, not an unknown one.
//!
//! Encoded as a test: [`gate`] emits the word `verified` for, and only for,
//! [`SealStatus::Verified`].
//!
//! # The verdict cache
//!
//! A verified check is cached: the verdict is keyed by `BLAKE3(spore.tla ||
//! spore.cfg)` (the **content of the proof**, [`proof_hash`]), so re-germinating
//! a spore whose seal bytes are unchanged reuses the prior pass without
//! re-running TLC. Any edit to the proof changes the hash and therefore
//! invalidates the cached verdict - a cache miss re-runs TLC. Only a *pass* is
//! cached: a failure on the same bytes will re-fail deterministically, and not
//! caching it leaves room for a transient (e.g. an out-of-memory TLC run) to be
//! retried.
//!
//! # Zero-I/O core
//!
//! The decision logic ([`gate`]), the status taxonomy ([`SealStatus`]), the
//! proof hash ([`proof_hash`]), and the orchestration ([`verify_seal`]) are
//! pure. The two I/O seams are traits:
//!
//! * [`TlcRunner`] - detects a JRE/TLC and runs it against the seal files;
//! * [`SealVerdictCache`] - persists the `proof_hash -> passed` verdict.
//!
//! Filesystem- and process-backed implementations live in the shell
//! (`cs spore run`, N5); this module ships an in-memory cache
//! ([`InMemorySealVerdictCache`]) and a scriptable fake runner ([`FakeTlcRunner`])
//! for tests and dry-runs.

use std::path::Path;
use std::sync::Mutex;

use std::collections::BTreeMap;

use crate::error::CosmonError;

use super::Seal;

// ---------------------------------------------------------------------------
// Proof hash
// ---------------------------------------------------------------------------

/// Compute the verdict cache key for a seal: `BLAKE3(spore.tla || spore.cfg)`.
///
/// The hash is over the **content of the proof** - the `.tla` module bytes
/// followed by the `.cfg` config bytes (if any). This is what makes the cache
/// honest about edits: changing a single byte of the proof or its config yields
/// a different hash, so the cached "passed" verdict for the old bytes is never
/// reused for the new ones.
///
/// Returned as lowercase hex (64 chars).
#[must_use]
pub fn proof_hash(module_bytes: &[u8], config_bytes: Option<&[u8]>) -> String {
    // Concatenate `.tla || .cfg` and hash with the same BLAKE3 primitive the
    // rest of cosmon uses (no new hashing primitive - ADR-140 D5 / ADR-043).
    let mut buf = Vec::with_capacity(module_bytes.len() + config_bytes.map_or(0, <[u8]>::len));
    buf.extend_from_slice(module_bytes);
    if let Some(cfg) = config_bytes {
        buf.extend_from_slice(cfg);
    }
    cosmon_hash::Hash::of_bytes(&buf).to_hex()
}

// ---------------------------------------------------------------------------
// Status taxonomy
// ---------------------------------------------------------------------------

/// The outcome of attempting to verify a spore's seal.
///
/// This is the honest three-state contract of ADR-140 D4, plus the
/// proof-rejected case that the prose folds into "present + unchecked" but which
/// the gate treats more strictly (it refuses unconditionally).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SealStatus {
    /// No `[spore.seal]` block - the spore declares no proof to check.
    Absent,
    /// The proof is verified, either by a TLC run this invocation or by a cached
    /// prior pass over byte-identical proof content.
    Verified {
        /// `BLAKE3(spore.tla || spore.cfg)` of the verified proof (hex).
        proof_hash: String,
        /// Whether the verdict came from the cache (`true`) or a fresh TLC run
        /// (`false`). Reporting-only; the gate treats both as verified.
        from_cache: bool,
    },
    /// The seal is present but TLC could not be run (no JRE / TLC on this
    /// machine). Honestly unchecked: the gate refuses by default and germinates
    /// only under the opt-in flag.
    UncheckedToolUnavailable {
        /// `BLAKE3(spore.tla || spore.cfg)` of the unchecked proof (hex).
        proof_hash: String,
    },
    /// The seal is present and TLC **ran and rejected the proof**. A known-unsafe
    /// generator: the gate refuses unconditionally, the opt-in flag does not
    /// apply.
    ProofFailed {
        /// `BLAKE3(spore.tla || spore.cfg)` of the rejected proof (hex).
        proof_hash: String,
        /// A short human description of why TLC rejected the proof.
        detail: String,
    },
}

// ---------------------------------------------------------------------------
// Gate decision
// ---------------------------------------------------------------------------

/// The germination gate's decision after weighing a [`SealStatus`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealGate {
    /// Germination proceeds. `report` is the honest one-line status to print
    /// (`seal: none` / `seal: verified <hash>` / `seal: present, NOT verified`).
    Germinate {
        /// The honest status line.
        report: String,
    },
    /// Germination is refused. `report` explains why and how to proceed.
    Refuse {
        /// The honest refusal line.
        report: String,
    },
}

impl SealGate {
    /// Whether this decision germinates.
    #[must_use]
    pub fn germinates(&self) -> bool {
        matches!(self, SealGate::Germinate { .. })
    }

    /// The honest report line, regardless of outcome.
    #[must_use]
    pub fn report(&self) -> &str {
        match self {
            SealGate::Germinate { report } | SealGate::Refuse { report } => report,
        }
    }
}

/// Short form of a proof hash for human reports (first 12 hex chars).
fn short(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

/// Decide whether germination proceeds, given a verification status and whether
/// the operator opted into running on an unchecked seal.
///
/// This is the single place the honesty invariant lives:
///
/// * [`Absent`](SealStatus::Absent) → germinate, `seal: none`.
/// * [`Verified`](SealStatus::Verified) → germinate, `seal: verified <hash>` -
///   the **only** status that yields a "verified" report.
/// * [`UncheckedToolUnavailable`](SealStatus::UncheckedToolUnavailable) →
///   germinate **only** if `allow_unchecked`, with the honest
///   `seal: present, NOT verified` line; otherwise refuse (fail-closed default).
/// * [`ProofFailed`](SealStatus::ProofFailed) → refuse **unconditionally**; the
///   opt-in flag is ignored because a rejected proof is known-unsafe.
#[must_use]
pub fn gate(status: &SealStatus, allow_unchecked: bool) -> SealGate {
    match status {
        SealStatus::Absent => SealGate::Germinate {
            report: "seal: none".to_string(),
        },
        SealStatus::Verified {
            proof_hash,
            from_cache,
        } => {
            let suffix = if *from_cache { " (cached)" } else { "" };
            SealGate::Germinate {
                report: format!("seal: verified {}{}", short(proof_hash), suffix),
            }
        }
        SealStatus::UncheckedToolUnavailable { proof_hash } => {
            if allow_unchecked {
                SealGate::Germinate {
                    report: format!(
                        "seal: present, NOT verified ({}) - germinating under --allow-unchecked-seal",
                        short(proof_hash)
                    ),
                }
            } else {
                SealGate::Refuse {
                    report: format!(
                        "seal: present, NOT verified ({}) - refusing: no JRE/TLC available. \
                         Install a JRE + TLC, or pass --allow-unchecked-seal to germinate at your own risk.",
                        short(proof_hash)
                    ),
                }
            }
        }
        SealStatus::ProofFailed { proof_hash, detail } => SealGate::Refuse {
            report: format!(
                "seal: FAILED ({}) - refusing: TLC rejected the proof: {detail}",
                short(proof_hash)
            ),
        },
    }
}

// ---------------------------------------------------------------------------
// I/O seams
// ---------------------------------------------------------------------------

/// What a TLC run concluded over a seal's `.tla` module and `.cfg` config.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TlcOutcome {
    /// TLC verified every declared property.
    Passed,
    /// TLC ran but rejected the proof; `detail` is a short reason.
    Failed {
        /// Short human description of the rejection.
        detail: String,
    },
    /// TLC could not be run on this machine (no JRE / TLC located).
    Unavailable,
}

/// The TLC execution seam.
///
/// Implementations detect a JRE + TLC and run it against the seal files. The
/// process-spawning implementation lives in the shell; tests use
/// [`FakeTlcRunner`].
pub trait TlcRunner {
    /// Whether a JRE + TLC are available on this machine.
    ///
    /// [`verify_seal`] consults this before attempting a run so it can produce
    /// the precise [`SealStatus::UncheckedToolUnavailable`] status without a
    /// spurious process spawn.
    fn available(&self) -> bool;

    /// Run TLC against the seal `module` (`.tla`) and optional `config` (`.cfg`).
    ///
    /// Only called when [`available`](TlcRunner::available) returned `true`; an
    /// implementation that nonetheless cannot run returns
    /// [`TlcOutcome::Unavailable`] and the orchestration stays honest (it reports
    /// the seal as unchecked, never as verified).
    fn check(&self, module: &Path, config: Option<&Path>) -> TlcOutcome;
}

/// The seal-verdict cache seam: persists the `proof_hash -> passed` verdict.
///
/// Keyed by [`proof_hash`] (the content of the proof), so an edited proof is a
/// cache miss by construction. The filesystem-backed implementation
/// (`.cosmon/cache/seal/<hash>`) lives in the shell; tests use
/// [`InMemorySealVerdictCache`].
pub trait SealVerdictCache {
    /// Look up the cached verdict for a proof hash. `Some(true)` means a prior
    /// TLC pass over byte-identical proof content; `None` is a miss.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError`] on backend (I/O) failure.
    fn get(&self, proof_hash: &str) -> Result<Option<bool>, CosmonError>;

    /// Record that the proof with this hash passed TLC.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError`] on backend (I/O) failure.
    fn put(&self, proof_hash: &str, passed: bool) -> Result<(), CosmonError>;
}

// ---------------------------------------------------------------------------
// Resolved seal + orchestration
// ---------------------------------------------------------------------------

/// A seal with its proof files read from disk by the shell.
///
/// The shell resolves the [`Seal`]'s relative `module` / `config` paths against
/// the manifest directory and reads their bytes; the core hashes the bytes (for
/// the cache key) and hands the paths to the [`TlcRunner`]. Keeping bytes and
/// paths together lets the core stay pure (it never touches the filesystem)
/// while the verdict cache key tracks the exact proof content.
#[derive(Debug, Clone)]
pub struct ResolvedSeal<'a> {
    /// Path to the `.tla` module (passed to TLC).
    pub module_path: &'a Path,
    /// Path to the `.cfg` config, if the seal declares one (passed to TLC).
    pub config_path: Option<&'a Path>,
    /// Bytes of the `.tla` module (hashed for the cache key).
    pub module_bytes: &'a [u8],
    /// Bytes of the `.cfg` config, if any (hashed for the cache key).
    pub config_bytes: Option<&'a [u8]>,
}

/// Verify a spore's seal into an honest [`SealStatus`], consulting the verdict
/// cache before running TLC and populating it on a fresh pass.
///
/// The algorithm (ADR-140 D4):
///
/// 1. No seal (`resolved` is `None`, the spore declared no `[spore.seal]`) →
///    [`SealStatus::Absent`].
/// 2. Compute `proof_hash` over the proof bytes.
/// 3. Cache hit (`Some(true)`) → [`SealStatus::Verified`] `{ from_cache: true }`
///    **without** running TLC.
/// 4. Cache miss + no TLC available → [`SealStatus::UncheckedToolUnavailable`].
/// 5. Cache miss + TLC available → run TLC:
///    * [`TlcOutcome::Passed`] → store the verdict, return
///      [`SealStatus::Verified`] `{ from_cache: false }`;
///    * [`TlcOutcome::Failed`] → [`SealStatus::ProofFailed`] (not cached);
///    * [`TlcOutcome::Unavailable`] (a runner that flipped after `available`) →
///      [`SealStatus::UncheckedToolUnavailable`].
///
/// `seal` is the parsed [`Seal`] (or `None`); `resolved` carries the bytes and
/// paths. Both being `None` means no seal. If `seal` is `Some` but `resolved`
/// is `None`, the shell failed to read the proof files - treated as
/// unverifiable (honest), never as verified.
///
/// # Errors
///
/// Propagates any [`CosmonError`] raised by the [`SealVerdictCache`].
pub fn verify_seal(
    seal: Option<&Seal>,
    resolved: Option<&ResolvedSeal<'_>>,
    cache: &dyn SealVerdictCache,
    tlc: &dyn TlcRunner,
) -> Result<SealStatus, CosmonError> {
    // No seal block at all: nothing to verify.
    let Some(_seal) = seal else {
        return Ok(SealStatus::Absent);
    };

    // Seal declared but the shell could not read its proof files: honestly
    // unverifiable. We still need a hash for the report; without bytes we cannot
    // compute one, so use a sentinel and report it as unchecked.
    let Some(resolved) = resolved else {
        return Ok(SealStatus::UncheckedToolUnavailable {
            proof_hash: "<proof files unreadable>".to_string(),
        });
    };

    let hash = proof_hash(resolved.module_bytes, resolved.config_bytes);

    // Cache hit: a prior pass over byte-identical proof content. An edit to the
    // proof changed the hash, so this only fires for the exact same bytes.
    if let Some(true) = cache.get(&hash)? {
        return Ok(SealStatus::Verified {
            proof_hash: hash,
            from_cache: true,
        });
    }

    // Cache miss. Run TLC if we can; otherwise honestly report unchecked.
    if !tlc.available() {
        return Ok(SealStatus::UncheckedToolUnavailable { proof_hash: hash });
    }

    match tlc.check(resolved.module_path, resolved.config_path) {
        TlcOutcome::Passed => {
            cache.put(&hash, true)?;
            Ok(SealStatus::Verified {
                proof_hash: hash,
                from_cache: false,
            })
        }
        TlcOutcome::Failed { detail } => Ok(SealStatus::ProofFailed {
            proof_hash: hash,
            detail,
        }),
        TlcOutcome::Unavailable => Ok(SealStatus::UncheckedToolUnavailable { proof_hash: hash }),
    }
}

// ---------------------------------------------------------------------------
// Reference implementations (pure; for tests and dry-runs)
// ---------------------------------------------------------------------------

/// A process-local, in-memory [`SealVerdictCache`].
///
/// Pure (no I/O); suitable for tests, dry-runs, and callers wanting a cache
/// scoped to a single process. The persistent `.cosmon/cache/seal/<hash>`
/// backend lives in the shell.
#[derive(Debug, Default)]
pub struct InMemorySealVerdictCache {
    entries: Mutex<BTreeMap<String, bool>>,
}

impl InMemorySealVerdictCache {
    /// Construct an empty in-memory verdict cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of cached verdicts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().map_or(0, |m| m.len())
    }

    /// Whether the cache holds no verdicts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SealVerdictCache for InMemorySealVerdictCache {
    fn get(&self, proof_hash: &str) -> Result<Option<bool>, CosmonError> {
        let guard = self.entries.lock().map_err(|_| CosmonError::Runtime {
            reason: "seal verdict cache mutex poisoned".to_owned(),
        })?;
        Ok(guard.get(proof_hash).copied())
    }

    fn put(&self, proof_hash: &str, passed: bool) -> Result<(), CosmonError> {
        let mut guard = self.entries.lock().map_err(|_| CosmonError::Runtime {
            reason: "seal verdict cache mutex poisoned".to_owned(),
        })?;
        guard.insert(proof_hash.to_owned(), passed);
        Ok(())
    }
}

/// A scriptable [`TlcRunner`] for tests: a fixed availability bit and a fixed
/// outcome. Records how many times [`check`](TlcRunner::check) ran so a test can
/// assert that a cache hit avoided the run.
#[derive(Debug)]
pub struct FakeTlcRunner {
    available: bool,
    outcome: TlcOutcome,
    runs: Mutex<u32>,
}

impl FakeTlcRunner {
    /// A runner that is available and returns `outcome` on every check.
    #[must_use]
    pub fn available_with(outcome: TlcOutcome) -> Self {
        Self {
            available: true,
            outcome,
            runs: Mutex::new(0),
        }
    }

    /// A runner that reports no JRE/TLC available; its check is never expected
    /// to run.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            available: false,
            outcome: TlcOutcome::Unavailable,
            runs: Mutex::new(0),
        }
    }

    /// How many times [`check`](TlcRunner::check) was invoked.
    #[must_use]
    pub fn run_count(&self) -> u32 {
        self.runs.lock().map_or(0, |g| *g)
    }
}

impl TlcRunner for FakeTlcRunner {
    fn available(&self) -> bool {
        self.available
    }

    fn check(&self, _module: &Path, _config: Option<&Path>) -> TlcOutcome {
        if let Ok(mut g) = self.runs.lock() {
            *g += 1;
        }
        self.outcome.clone()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn seal() -> Seal {
        Seal {
            module: "spore.tla".to_string(),
            config: Some("spore.cfg".to_string()),
            properties: vec!["Termination".to_string()],
        }
    }

    fn resolved<'a>(
        module_path: &'a Path,
        config_path: &'a Path,
        module_bytes: &'a [u8],
        config_bytes: &'a [u8],
    ) -> ResolvedSeal<'a> {
        ResolvedSeal {
            module_path,
            config_path: Some(config_path),
            module_bytes,
            config_bytes: Some(config_bytes),
        }
    }

    // --- proof_hash --------------------------------------------------------

    #[test]
    fn proof_hash_is_stable_and_content_addressed() {
        let a = proof_hash(b"MODULE spore", Some(b"INVARIANT X"));
        let b = proof_hash(b"MODULE spore", Some(b"INVARIANT X"));
        assert_eq!(a, b, "same bytes => same hash");
        assert_eq!(a.len(), 64, "BLAKE3 hex is 64 chars");
    }

    #[test]
    fn editing_the_proof_changes_the_hash() {
        let original = proof_hash(b"MODULE spore", Some(b"INVARIANT X"));
        let edited_module = proof_hash(b"MODULE spore EDITED", Some(b"INVARIANT X"));
        let edited_config = proof_hash(b"MODULE spore", Some(b"INVARIANT Y"));
        assert_ne!(original, edited_module, "editing .tla moves the hash");
        assert_ne!(original, edited_config, "editing .cfg moves the hash");
    }

    // --- gate: the honest three states ------------------------------------

    #[test]
    fn absent_seal_germinates_with_seal_none() {
        let g = gate(&SealStatus::Absent, false);
        assert!(g.germinates());
        assert_eq!(g.report(), "seal: none");
    }

    #[test]
    fn verified_seal_germinates_and_reports_verified() {
        let status = SealStatus::Verified {
            proof_hash: "abcdef0123456789".to_string(),
            from_cache: false,
        };
        let g = gate(&status, false);
        assert!(g.germinates());
        assert!(g.report().starts_with("seal: verified abcdef012345"));
    }

    #[test]
    fn unchecked_seal_refuses_by_default() {
        let status = SealStatus::UncheckedToolUnavailable {
            proof_hash: "deadbeefcafebabe".to_string(),
        };
        let g = gate(&status, false);
        assert!(!g.germinates(), "fail-closed default refuses");
        assert!(g.report().contains("NOT verified"));
        // The refusal must never claim a pass: every "verified" is negated.
        assert!(
            !g.report().contains("verified") || g.report().contains("NOT verified"),
            "must not imply a pass: {}",
            g.report()
        );
    }

    #[test]
    fn unchecked_seal_germinates_under_flag_with_honest_line() {
        let status = SealStatus::UncheckedToolUnavailable {
            proof_hash: "deadbeefcafebabe".to_string(),
        };
        let g = gate(&status, true);
        assert!(g.germinates(), "opt-in flag germinates");
        assert!(
            g.report().contains("NOT verified"),
            "the honest line survives the opt-in: {}",
            g.report()
        );
    }

    #[test]
    fn proof_failure_refuses_even_under_flag() {
        // A rejected proof is known-unsafe: the opt-in flag must NOT rescue it.
        let status = SealStatus::ProofFailed {
            proof_hash: "0011223344556677".to_string(),
            detail: "Invariant Termination violated".to_string(),
        };
        let with_flag = gate(&status, true);
        let without_flag = gate(&status, false);
        assert!(
            !with_flag.germinates(),
            "flag must not rescue a failed proof"
        );
        assert!(!without_flag.germinates());
        assert!(with_flag.report().contains("FAILED"));
    }

    #[test]
    fn only_verified_status_ever_reports_the_word_verified() {
        // The honesty invariant, enforced mechanically: scan every status and
        // every flag setting; "verified" appears in a germinating report iff the
        // status is Verified.
        let hash = "aabbccddeeff0011".to_string();
        let statuses = [
            (SealStatus::Absent, false),
            (
                SealStatus::Verified {
                    proof_hash: hash.clone(),
                    from_cache: true,
                },
                true,
            ),
            (
                SealStatus::UncheckedToolUnavailable {
                    proof_hash: hash.clone(),
                },
                false,
            ),
            (
                SealStatus::ProofFailed {
                    proof_hash: hash.clone(),
                    detail: "x".to_string(),
                },
                false,
            ),
        ];
        for (status, is_verified) in statuses {
            for allow in [false, true] {
                let g = gate(&status, allow);
                // "NOT verified" contains the substring "verified" but never
                // claims a pass; the invariant is about a *germinating* report
                // that asserts verification.
                let claims_pass = g.germinates()
                    && g.report().contains("verified")
                    && !g.report().contains("NOT verified");
                assert_eq!(
                    claims_pass, is_verified,
                    "status {status:?} (allow={allow}) wrongly claims/omits verification"
                );
            }
        }
    }

    // --- verify_seal orchestration ----------------------------------------

    #[test]
    fn no_seal_resolves_to_absent() {
        let cache = InMemorySealVerdictCache::new();
        let tlc = FakeTlcRunner::unavailable();
        let status = verify_seal(None, None, &cache, &tlc).unwrap();
        assert_eq!(status, SealStatus::Absent);
        assert_eq!(tlc.run_count(), 0, "no seal => no TLC run");
    }

    #[test]
    fn present_seal_with_jre_runs_tlc_and_caches() {
        let cache = InMemorySealVerdictCache::new();
        let tlc = FakeTlcRunner::available_with(TlcOutcome::Passed);
        let s = seal();
        let mp = PathBuf::from("spore.tla");
        let cp = PathBuf::from("spore.cfg");
        let r = resolved(&mp, &cp, b"MODULE spore", b"INVARIANT X");
        let expected_hash = proof_hash(b"MODULE spore", Some(b"INVARIANT X"));

        let status = verify_seal(Some(&s), Some(&r), &cache, &tlc).unwrap();
        match &status {
            SealStatus::Verified {
                from_cache,
                proof_hash: ph,
            } => {
                assert!(!from_cache, "first run is a fresh TLC pass");
                assert_eq!(ph, &expected_hash);
            }
            other => panic!("expected Verified, got {other:?}"),
        }
        assert_eq!(tlc.run_count(), 1);
        assert_eq!(cache.len(), 1, "the pass was cached");
    }

    #[test]
    fn cached_pass_skips_the_tlc_run() {
        let cache = InMemorySealVerdictCache::new();
        let tlc = FakeTlcRunner::available_with(TlcOutcome::Passed);
        let s = seal();
        let mp = PathBuf::from("spore.tla");
        let cp = PathBuf::from("spore.cfg");
        let r = resolved(&mp, &cp, b"MODULE spore", b"INVARIANT X");

        // First run populates the cache.
        verify_seal(Some(&s), Some(&r), &cache, &tlc).unwrap();
        assert_eq!(tlc.run_count(), 1);

        // Second run with byte-identical proof hits the cache: no second run.
        let status = verify_seal(Some(&s), Some(&r), &cache, &tlc).unwrap();
        assert!(
            matches!(
                status,
                SealStatus::Verified {
                    from_cache: true,
                    ..
                }
            ),
            "second run is a cache hit"
        );
        assert_eq!(tlc.run_count(), 1, "cache hit avoided a second TLC run");
    }

    #[test]
    fn present_seal_without_jre_is_unchecked() {
        let cache = InMemorySealVerdictCache::new();
        let tlc = FakeTlcRunner::unavailable();
        let s = seal();
        let mp = PathBuf::from("spore.tla");
        let cp = PathBuf::from("spore.cfg");
        let r = resolved(&mp, &cp, b"MODULE spore", b"INVARIANT X");

        let status = verify_seal(Some(&s), Some(&r), &cache, &tlc).unwrap();
        assert!(matches!(
            status,
            SealStatus::UncheckedToolUnavailable { .. }
        ));
        assert_eq!(tlc.run_count(), 0, "no JRE => TLC not invoked");
        // And the gate refuses by default, germinates under the flag.
        assert!(!gate(&status, false).germinates());
        assert!(gate(&status, true).germinates());
    }

    #[test]
    fn edited_proof_invalidates_the_cached_verdict() {
        let cache = InMemorySealVerdictCache::new();
        let tlc = FakeTlcRunner::available_with(TlcOutcome::Passed);
        let s = seal();
        let mp = PathBuf::from("spore.tla");
        let cp = PathBuf::from("spore.cfg");

        // Pass over the original proof, cached.
        let original = resolved(&mp, &cp, b"MODULE spore", b"INVARIANT X");
        verify_seal(Some(&s), Some(&original), &cache, &tlc).unwrap();
        assert_eq!(tlc.run_count(), 1);

        // Edit the proof: a different hash, so the cached verdict does NOT apply
        // and TLC re-runs.
        let edited = resolved(&mp, &cp, b"MODULE spore EDITED", b"INVARIANT X");
        let status = verify_seal(Some(&s), Some(&edited), &cache, &tlc).unwrap();
        assert!(matches!(
            status,
            SealStatus::Verified {
                from_cache: false,
                ..
            }
        ));
        assert_eq!(tlc.run_count(), 2, "edited proof re-ran TLC");
        assert_eq!(cache.len(), 2, "both proof versions are cached separately");
    }

    #[test]
    fn tlc_rejection_yields_proof_failed_and_is_not_cached() {
        let cache = InMemorySealVerdictCache::new();
        let tlc = FakeTlcRunner::available_with(TlcOutcome::Failed {
            detail: "Deadlock reached".to_string(),
        });
        let s = seal();
        let mp = PathBuf::from("spore.tla");
        let cp = PathBuf::from("spore.cfg");
        let r = resolved(&mp, &cp, b"MODULE spore", b"INVARIANT X");

        let status = verify_seal(Some(&s), Some(&r), &cache, &tlc).unwrap();
        assert!(matches!(status, SealStatus::ProofFailed { .. }));
        assert!(cache.is_empty(), "a failure is never cached");
        // The gate refuses unconditionally.
        assert!(!gate(&status, true).germinates());
    }

    #[test]
    fn seal_present_but_proof_files_unreadable_is_unchecked() {
        let cache = InMemorySealVerdictCache::new();
        let tlc = FakeTlcRunner::available_with(TlcOutcome::Passed);
        let s = seal();
        // Shell could not read the proof files: resolved is None.
        let status = verify_seal(Some(&s), None, &cache, &tlc).unwrap();
        assert!(matches!(
            status,
            SealStatus::UncheckedToolUnavailable { .. }
        ));
        assert_eq!(tlc.run_count(), 0);
        assert!(!gate(&status, false).germinates(), "fail-closed");
    }
}
