// SPDX-License-Identifier: AGPL-3.0-only

//! Per-`sub` leaky bucket — clause (c) of the §8j HTTPS+JWT
//! instantiation (ADR-080 §3.3).
//!
//! Mirrors the disk-persisted bucket model from `cosmon-matrix-tick`
//! (the §8j Matrix instantiation), keyed on the JWT `claim.sub`
//! BLAKE3 hash so the on-disk filename never leaks the raw subject.
//!
//! The bucket persists across adapter restarts: a kill-9'd RPP
//! cannot be used to reset a flooder's accounting.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Default bucket capacity (V0 — ADR-080 §3.3): 30 burst.
pub const DEFAULT_CAPACITY: f64 = 30.0;

/// Default leak per hour (V0 — ADR-080 §3.3): 600 / hour ≈ 10 / min.
pub const DEFAULT_LEAK_PER_HOUR: f64 = 600.0;

/// Abstract clock so tests can inject deterministic time.
pub trait Clock: std::fmt::Debug + Send + Sync {
    /// Milliseconds since Unix epoch.
    fn now_ms(&self) -> i64;
}

/// Production clock. Construct it via [`SystemClock::default`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
            Err(_) => 0,
        }
    }
}

/// On-disk bucket record. Kept tiny so the file is cheap to rewrite.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct BucketState {
    level: f64,
    last_updated_ms: i64,
}

/// Persistent leaky-bucket rate limiter, keyed by JWT `claim.sub`.
#[derive(Debug)]
pub struct IngressRateLimiter {
    dir: PathBuf,
    capacity: f64,
    leak_per_ms: f64,
    /// Serialises read-modify-write so concurrent workers don't
    /// double-leak the bucket. The Mutex is fine-grained — a single
    /// adapter process holds the whole structure.
    lock: Mutex<()>,
}

impl IngressRateLimiter {
    /// Construct a limiter rooted at `dir`. Creates the dir on first
    /// write — no-op if it already exists.
    #[must_use]
    pub fn new(dir: PathBuf, capacity: f64, leak_per_hour: f64) -> Self {
        let leak_per_ms = leak_per_hour / (3600.0 * 1000.0);
        Self {
            dir,
            capacity,
            leak_per_ms,
            lock: Mutex::new(()),
        }
    }

    /// Default-tuned limiter for V0 (`capacity=30`, `leak=600/hour`).
    #[must_use]
    pub fn default_in(dir: PathBuf) -> Self {
        Self::new(dir, DEFAULT_CAPACITY, DEFAULT_LEAK_PER_HOUR)
    }

    /// Configured burst capacity (max tokens the bucket can hold).
    #[must_use]
    pub fn capacity(&self) -> f64 {
        self.capacity
    }

    /// Configured leak rate (tokens drained per hour).
    #[must_use]
    pub fn leak_per_hour(&self) -> f64 {
        self.leak_per_ms * 3600.0 * 1000.0
    }

    /// Configured leak rate (tokens drained per minute).
    #[must_use]
    pub fn leak_per_minute(&self) -> f64 {
        self.leak_per_ms * 60.0 * 1000.0
    }

    /// Read-only snapshot of a tenant's bucket — the disk row drained
    /// to `now_ms` plus the configured shape. Pure: no mutation, no
    /// rate-limit consumption (calling `current_state` to inspect must
    /// not itself eat a token).
    ///
    /// Returns `Ok(RateState)` even when the tenant has never made a
    /// request — the absent file decodes to an empty bucket
    /// (`level=0`).
    ///
    /// # Errors
    ///
    /// Returns the underlying IO error on read failure of the bucket
    /// file (a parse failure surfaces as `InvalidData`).
    pub fn current_state(&self, sub_hash: &str, now_ms: i64) -> std::io::Result<RateState> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = self.load(sub_hash)?;
        let drained = drain_level(&state, now_ms, self.leak_per_ms);
        let remaining = (self.capacity - drained).max(0.0);
        // Milliseconds for the bucket to fully drain back to 0 — that
        // is the soonest moment a fresh burst window is wide open.
        let reset_ms = if self.leak_per_ms > 0.0 {
            (drained / self.leak_per_ms).ceil() as i64
        } else {
            // No leak — never reset by leak; pretend "now" so callers
            // don't crash on Duration::from_millis(i64::MAX).
            0
        };
        Ok(RateState {
            capacity: self.capacity,
            leak_per_ms: self.leak_per_ms,
            level: drained,
            remaining,
            reset_at_ms: now_ms.saturating_add(reset_ms),
        })
    }

    /// Atomic check-and-consume. Returns the post-consume retry wait
    /// (0 ms when admitted) — callers inspect for `>0` to reject.
    ///
    /// # Errors
    ///
    /// Returns the underlying IO error on read/write failure of the
    /// bucket file.
    pub fn check_and_consume(&self, sub_hash: &str, now_ms: i64) -> std::io::Result<RateOutcome> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = self.load(sub_hash)?;
        let drained = drain_level(&state, now_ms, self.leak_per_ms);
        if drained + 1.0 > self.capacity {
            let overflow = (drained + 1.0) - self.capacity;
            let retry_ms = if self.leak_per_ms > 0.0 {
                (overflow / self.leak_per_ms).ceil() as i64
            } else {
                24 * 3600 * 1000
            };
            return Ok(RateOutcome::Rejected { retry_ms });
        }
        let new_state = BucketState {
            level: drained + 1.0,
            last_updated_ms: now_ms,
        };
        self.store(sub_hash, &new_state)?;
        Ok(RateOutcome::Admitted)
    }

    fn path_for(&self, sub_hash: &str) -> PathBuf {
        self.dir.join(format!("{}.json", filename_safe(sub_hash)))
    }

    fn load(&self, sub_hash: &str) -> std::io::Result<BucketState> {
        let p = self.path_for(sub_hash);
        if !p.exists() {
            return Ok(BucketState::default());
        }
        let text = std::fs::read_to_string(&p)?;
        serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    fn store(&self, sub_hash: &str, state: &BucketState) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let p = self.path_for(sub_hash);
        let tmp = p.with_extension("json.tmp");
        let bytes = serde_json::to_vec(state)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &p)?;
        Ok(())
    }
}

fn drain_level(state: &BucketState, now_ms: i64, leak_per_ms: f64) -> f64 {
    let dt = (now_ms.saturating_sub(state.last_updated_ms)).max(0);
    (state.level - (dt as f64) * leak_per_ms).max(0.0)
}

fn filename_safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Hash a JWT `sub` into the on-disk filename component (BLAKE3, hex).
#[must_use]
pub fn hash_sub(sub: &str) -> String {
    blake3::hash(sub.as_bytes()).to_hex().to_string()
}

/// Outcome of a single check-and-consume call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RateOutcome {
    /// Admitted — the bucket was incremented and persisted.
    Admitted,
    /// Rejected — bucket would overflow; `retry_ms` is the earliest
    /// wall-clock millisecond the client should retry at.
    Rejected {
        /// Wait this many milliseconds before retrying.
        retry_ms: i64,
    },
}

/// Read-only snapshot of one tenant's bucket — returned by
/// [`IngressRateLimiter::current_state`]. Powers the `/v1/quota` route
/// and the `X-RateLimit-*` response headers. The shape mirrors the
/// leaky-bucket model;
/// it is **not** a per-minute / per-hour counter — the bucket *is* the
/// rate-limit account, and burst capacity is the dominant axis.
#[derive(Clone, Copy, Debug)]
pub struct RateState {
    /// Burst capacity (max tokens the bucket can hold).
    pub capacity: f64,
    /// Leak rate, tokens drained per millisecond.
    pub leak_per_ms: f64,
    /// Current bucket level, drained to the snapshot `now_ms`. Always
    /// in `[0, capacity]`.
    pub level: f64,
    /// Tokens still available before the next request would overflow
    /// (`capacity - level`). Floor for the `X-RateLimit-Remaining`
    /// header.
    pub remaining: f64,
    /// Wall-clock ms since Unix epoch when the bucket would drain back
    /// to 0 (i.e. when burst capacity is fully restored). When
    /// `leak_per_ms == 0` this equals the snapshot `now_ms`.
    pub reset_at_ms: i64,
}

impl RateState {
    /// Floor of `remaining` as a non-negative `i64`, the value to
    /// serialise into the `X-RateLimit-Remaining` header.
    #[must_use]
    pub fn remaining_floor(&self) -> i64 {
        self.remaining.floor().max(0.0) as i64
    }

    /// Floor of `capacity` as a non-negative `i64`, the value to
    /// serialise into the `X-RateLimit-Limit` header.
    #[must_use]
    pub fn capacity_floor(&self) -> i64 {
        self.capacity.floor().max(0.0) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn admits_first_request() {
        let td = TempDir::new().unwrap();
        let lim = IngressRateLimiter::new(td.path().to_path_buf(), 3.0, 60.0);
        assert_eq!(
            lim.check_and_consume("sub-hash", 0).unwrap(),
            RateOutcome::Admitted
        );
    }

    #[test]
    fn capacity_blocks_burst() {
        let td = TempDir::new().unwrap();
        let lim = IngressRateLimiter::new(td.path().to_path_buf(), 2.0, 0.0);
        assert_eq!(
            lim.check_and_consume("h", 0).unwrap(),
            RateOutcome::Admitted
        );
        assert_eq!(
            lim.check_and_consume("h", 0).unwrap(),
            RateOutcome::Admitted
        );
        let outcome = lim.check_and_consume("h", 0).unwrap();
        assert!(matches!(outcome, RateOutcome::Rejected { retry_ms } if retry_ms > 0));
    }

    #[test]
    fn persists_across_instances() {
        let td = TempDir::new().unwrap();
        {
            let a = IngressRateLimiter::new(td.path().to_path_buf(), 1.0, 0.0);
            a.check_and_consume("h", 0).unwrap();
        }
        let b = IngressRateLimiter::new(td.path().to_path_buf(), 1.0, 0.0);
        assert!(matches!(
            b.check_and_consume("h", 0).unwrap(),
            RateOutcome::Rejected { .. }
        ));
    }

    #[test]
    fn leaks_over_time() {
        let td = TempDir::new().unwrap();
        // 3600/hr = 1/sec → 1/1000 per ms
        let lim = IngressRateLimiter::new(td.path().to_path_buf(), 1.0, 3600.0);
        lim.check_and_consume("h", 0).unwrap();
        assert!(matches!(
            lim.check_and_consume("h", 0).unwrap(),
            RateOutcome::Rejected { .. }
        ));
        assert_eq!(
            lim.check_and_consume("h", 2_000).unwrap(),
            RateOutcome::Admitted
        );
    }

    #[test]
    fn current_state_on_empty_bucket() {
        let td = TempDir::new().unwrap();
        let lim = IngressRateLimiter::new(td.path().to_path_buf(), 30.0, 600.0);
        let snap = lim.current_state("nobody", 0).unwrap();
        assert!((snap.capacity - 30.0).abs() < 1e-9);
        assert!((snap.level - 0.0).abs() < 1e-9);
        assert!((snap.remaining - 30.0).abs() < 1e-9);
        assert_eq!(snap.remaining_floor(), 30);
        assert_eq!(snap.capacity_floor(), 30);
        assert_eq!(snap.reset_at_ms, 0);
    }

    #[test]
    fn current_state_after_consumes() {
        let td = TempDir::new().unwrap();
        let lim = IngressRateLimiter::new(td.path().to_path_buf(), 30.0, 0.0);
        for _ in 0..7 {
            lim.check_and_consume("h", 0).unwrap();
        }
        let snap = lim.current_state("h", 0).unwrap();
        assert!((snap.level - 7.0).abs() < 1e-9);
        assert!((snap.remaining - 23.0).abs() < 1e-9);
        assert_eq!(snap.remaining_floor(), 23);
    }

    #[test]
    fn current_state_drains_with_time() {
        let td = TempDir::new().unwrap();
        // 3600/hr → 1 token / sec → 1 / 1000 per ms
        let lim = IngressRateLimiter::new(td.path().to_path_buf(), 30.0, 3600.0);
        for _ in 0..10 {
            lim.check_and_consume("h", 0).unwrap();
        }
        // 5 s later we expect 5 tokens drained → level = 5
        let snap = lim.current_state("h", 5_000).unwrap();
        assert!((snap.level - 5.0).abs() < 1e-9);
        assert!((snap.remaining - 25.0).abs() < 1e-9);
        // reset = 5 s remaining at 1 tok / s → reset_at_ms = 5_000 + 5_000
        assert_eq!(snap.reset_at_ms, 10_000);
    }

    #[test]
    fn current_state_does_not_consume() {
        let td = TempDir::new().unwrap();
        let lim = IngressRateLimiter::new(td.path().to_path_buf(), 30.0, 0.0);
        for _ in 0..5 {
            lim.check_and_consume("h", 0).unwrap();
        }
        // 100 snapshots: level must stay frozen at 5.
        for _ in 0..100 {
            let snap = lim.current_state("h", 0).unwrap();
            assert!((snap.level - 5.0).abs() < 1e-9);
        }
        // And a check_and_consume still admits because we never bumped.
        assert_eq!(
            lim.check_and_consume("h", 0).unwrap(),
            RateOutcome::Admitted
        );
        let snap = lim.current_state("h", 0).unwrap();
        assert!((snap.level - 6.0).abs() < 1e-9);
    }

    #[test]
    fn current_state_reset_at_when_no_leak() {
        let td = TempDir::new().unwrap();
        let lim = IngressRateLimiter::new(td.path().to_path_buf(), 5.0, 0.0);
        lim.check_and_consume("h", 100).unwrap();
        let snap = lim.current_state("h", 1_000).unwrap();
        // No leak → reset_at_ms collapses to now_ms (no eventual drain
        // possible without further leak).
        assert_eq!(snap.reset_at_ms, 1_000);
    }

    #[test]
    fn leak_rate_accessors() {
        let td = TempDir::new().unwrap();
        let lim = IngressRateLimiter::new(td.path().to_path_buf(), 30.0, 600.0);
        assert!((lim.capacity() - 30.0).abs() < 1e-9);
        assert!((lim.leak_per_hour() - 600.0).abs() < 1e-9);
        assert!((lim.leak_per_minute() - 10.0).abs() < 1e-9);
    }
}
