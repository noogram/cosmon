// SPDX-License-Identifier: AGPL-3.0-only

//! Convergence evaluator — decides when a patrol with a `[patrol.sunset]`
//! rule has collected enough information and should stop.
//!
//! ## Why this lives outside `tick.rs`
//!
//! The tick loop is a pure decision pass over *schema* + *clock* + *state*.
//! Convergence adds a fourth input — a sample file on disk — whose shape
//! (malformed rows, missing files, partial writes) differs from anything
//! else the scheduler reads. Factoring the sample I/O and the statistical
//! predicates here keeps `tick.rs` small and keeps the convergence rule
//! directly unit-testable without spinning up a whole `Config`.
//!
//! ## Three pure primitives + one tolerant reader
//!
//! The public surface deliberately exposes *primitives*, not a single
//! `evaluate_sunset(...)`:
//!
//! - [`rolling_stdev`] — sample standard deviation over the last `window`
//!   values. Building block for `variance-threshold`.
//! - [`sample_count_predicate`] — "did we collect enough samples?" Building
//!   block for `sample-count`.
//! - [`operator_trigger_predicate`] — "did the operator drop the trigger
//!   file?" Building block for `operator-trigger-only`.
//! - [`read_samples_tolerant`] — the file reader: skips blank lines,
//!   comments (`#…`), and unparseable rows, and accumulates one
//!   [`ConvergenceWarning`] per dropped row so the caller can later emit
//!   them to the scheduler's event log.
//!
//! ## Variance vs stdev — why we expose stdev
//!
//! `Sunset::variance_threshold` is named in variance units (`σ²`). The
//! primitive returns `σ` because stdev has the same units as the data,
//! which makes the value easier to reason about in logs (*"values wobble
//! by ±0.02"*) than variance (*"wobble squared is 0.0004"*). The caller
//! compares `rolling_stdev.powi(2) < variance_threshold` — see
//! [`variance_threshold_predicate`].
//!
//! ## Tolerance, not silence
//!
//! The TSV reader never panics or returns `Result` — a patrol whose data
//! source is temporarily missing should *not* block every other patrol on
//! the same tick. Instead the reader returns `values: []` and a warning
//! describing why, so the scheduler can:
//!
//! 1. Still evaluate `min_samples` / `sample_count` (both will say "not
//!    enough yet"), which keeps the patrol alive.
//! 2. Emit the warning to `events.jsonl` for the operator to see.
//!
//! Think of it as the same discipline as `cargo check` on a file it
//! cannot open: warn, don't fail the whole run.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::environment::Environment;

/// Sample standard deviation (n−1 divisor) over the **last `window`
/// samples** of `values`.
///
/// # Contract
///
/// - Returns `None` when `window < 2` (stdev of fewer than two points
///   is undefined; rolling with a 1-wide window is a meaningless gate).
/// - Returns `None` when `values.len() < window` (not enough samples
///   yet — the caller should keep collecting).
/// - Otherwise returns `Some(σ)` where `σ` is computed over exactly
///   the trailing `window` entries.
///
/// # Numerical notes
///
/// We use the **two-pass** method (mean, then sum of squared residuals),
/// not Welford or the naive `E[X²] - E[X]²`. For a measurement scheduler:
///
/// - `window` is small (tens to hundreds), so the extra pass is free.
/// - Two-pass is numerically stable for the magnitudes patrols log
///   (latencies, counts, percentages) — Welford's incremental update
///   would buy nothing here and cost readability.
/// - `E[X²] - E[X]²` is catastrophically unstable near the convergence
///   regime we care about (small spread around a non-zero mean) and is
///   the single biggest trap in naive variance code. We do not use it.
#[must_use]
pub fn rolling_stdev(window: usize, values: &[f64]) -> Option<f64> {
    if window < 2 || values.len() < window {
        return None;
    }
    // Safe: length checked above, so `values.len() - window` does not wrap.
    let tail = &values[values.len() - window..];

    #[allow(clippy::cast_precision_loss)]
    let n = window as f64;
    let mean: f64 = tail.iter().copied().sum::<f64>() / n;
    let sum_sq: f64 = tail.iter().map(|v| (v - mean).powi(2)).sum();
    // n-1 divisor: sample stdev, not population.
    let variance = sum_sq / (n - 1.0);
    Some(variance.sqrt())
}

/// True iff we have at least `target` samples on hand.
///
/// Wrapping the comparison in a named predicate (rather than inlining
/// `samples >= target` at the call site) matters because the scheduler
/// has *three* places that ask the same question — dispatch gate, sunset
/// gate, and `cs scheduler status` — and they must all agree. A typo
/// like `>` vs `>=` in one of them is a silent half-hour sampling gap
/// that only shows up when the operator asks "why did this sunset so
/// late?".
#[must_use]
pub fn sample_count_predicate(samples: u64, target: u64) -> bool {
    samples >= target
}

/// True iff the operator has dropped a trigger file at `trigger_path`.
///
/// `None` path means "no manual override configured" — always returns
/// `false`, never `true`. This is the correct default: a patrol whose
/// `[patrol.sunset] strategy = "operator-trigger-only"` block forgot
/// to set `trigger_file` can never auto-sunset, only be removed from
/// the config by hand. That's louder than a silent-success default.
pub fn operator_trigger_predicate<E: Environment + ?Sized>(
    env: &E,
    trigger_path: Option<&str>,
) -> bool {
    match trigger_path {
        Some(path) => env.path_exists(path),
        None => false,
    }
}

/// Composite convenience: `variance-threshold` strategy as one call.
///
/// Returns `true` iff **all three** of the following hold:
///
/// 1. `values.len() >= min_samples` — the min-samples floor. Guards
///    against a stuck-sensor series (`[42.0, 42.0, ...]`) sunsetting
///    on the second tick.
/// 2. [`rolling_stdev`] over `window` yields `Some(σ)` (i.e. we have
///    enough tail samples to compute it).
/// 3. `σ² < variance_threshold`.
///
/// This keeps the XOR of "stuck sensor" vs "legitimate convergence" in
/// one place, so the three callers (tick, dispatch, status) see the
/// same answer.
#[must_use]
pub fn variance_threshold_predicate(
    window: usize,
    min_samples: u64,
    variance_threshold: f64,
    values: &[f64],
) -> bool {
    #[allow(clippy::cast_possible_truncation)]
    let have = values.len() as u64;
    if !sample_count_predicate(have, min_samples) {
        return false;
    }
    let Some(sigma) = rolling_stdev(window, values) else {
        return false;
    };
    sigma * sigma < variance_threshold
}

/// A single non-fatal problem the TSV reader found while parsing.
///
/// The scheduler routes these into `events.jsonl` at dispatch time so
/// operators can see "this probe's log file had 7 malformed lines last
/// tick" without the scheduler itself having to decide whether to alert,
/// silence, or dedupe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvergenceWarning {
    /// Path that was being read when the problem occurred. Copied (not
    /// borrowed) because the warning often outlives the call scope —
    /// it ends up in an event record.
    pub path: String,

    /// 1-based line number. `None` means the warning is about the file
    /// as a whole (missing, permission denied) rather than a specific row.
    pub line: Option<usize>,

    /// Machine-readable discriminant.
    pub kind: WarningKind,

    /// Human-readable detail. Suitable for log lines; not parsed.
    pub detail: String,
}

/// Discriminant for [`ConvergenceWarning`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningKind {
    /// The sample file does not exist yet. Common on first tick before
    /// the patrol has produced any output.
    MissingFile,

    /// Filesystem error other than `NotFound` (permission, I/O fault).
    IoError,

    /// A data line exists but its last column does not parse as `f64`.
    MalformedRow,

    /// The file exists but contains no data rows (possibly only comments
    /// or blanks). Distinct from `MissingFile` because the operator may
    /// want to alert on it differently.
    EmptyFile,
}

/// Outcome of [`read_samples_tolerant`] — parsed values plus any
/// warnings accumulated along the way.
///
/// Values are returned in file order. Warnings are returned in
/// encounter order so that operators reading `events.jsonl` can
/// reconstruct the state of the file at tick time.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SampleRead {
    /// Numeric values, one per successfully parsed data row.
    pub values: Vec<f64>,
    /// Non-fatal problems surfaced by the reader, in encounter order.
    pub warnings: Vec<ConvergenceWarning>,
}

/// Read a TSV sample file and extract the **last whitespace-separated
/// column** of each non-blank, non-comment row as `f64`.
///
/// ## Line policy
///
/// | Line                          | Outcome                      |
/// |-------------------------------|------------------------------|
/// | empty or whitespace-only      | silently skipped             |
/// | starts with `#` (after trim)  | silently skipped (comment)   |
/// | last column parses as `f64`   | value pushed to `values`     |
/// | last column does not parse    | `MalformedRow` warning       |
///
/// ## File-level policy
///
/// | Condition                     | Outcome                             |
/// |-------------------------------|-------------------------------------|
/// | file does not exist           | empty `values`, `MissingFile` warn  |
/// | I/O error (e.g. permission)   | empty `values`, `IoError` warn      |
/// | exists but has 0 data rows    | empty `values`, `EmptyFile` warn    |
///
/// The reader never returns `Err`: a patrol with a temporarily unreadable
/// sample file must not block the rest of the tick. The [`SampleRead`]
/// struct encodes "how much do we know, and what went wrong" in one place.
///
/// ## Why "last column"?
///
/// Operator-facing logs typically follow the shape
/// `<timestamp>\t<label>\t<value>` — the value is the last field. We read
/// the last column so the same format works for both single-column
/// `echo "$v" >> log.tsv` probes and multi-column structured logs. A
/// future `metric = "colN"` selector on `Sunset` can override this.
#[must_use]
pub fn read_samples_tolerant(path: &Path) -> SampleRead {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return SampleRead {
                values: Vec::new(),
                warnings: vec![ConvergenceWarning {
                    path: path.display().to_string(),
                    line: None,
                    kind: WarningKind::MissingFile,
                    detail: "sample file does not exist yet".to_owned(),
                }],
            };
        }
        Err(e) => {
            return SampleRead {
                values: Vec::new(),
                warnings: vec![ConvergenceWarning {
                    path: path.display().to_string(),
                    line: None,
                    kind: WarningKind::IoError,
                    detail: format!("i/o error: {e}"),
                }],
            };
        }
    };

    let mut out = SampleRead::default();
    let mut saw_data_row = false;

    for (idx, raw_line) in raw.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        saw_data_row = true;
        let last = line.split_whitespace().next_back().unwrap_or("");
        match last.parse::<f64>() {
            Ok(v) => out.values.push(v),
            Err(e) => out.warnings.push(ConvergenceWarning {
                path: path.display().to_string(),
                line: Some(idx + 1),
                kind: WarningKind::MalformedRow,
                detail: format!("last column '{last}' not f64: {e}"),
            }),
        }
    }

    if !saw_data_row {
        out.warnings.push(ConvergenceWarning {
            path: path.display().to_string(),
            line: None,
            kind: WarningKind::EmptyFile,
            detail: "file has no data rows (only blanks/comments)".to_owned(),
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::StubEnv;

    // ----- rolling_stdev -------------------------------------------------

    #[test]
    fn rolling_stdev_none_when_window_too_small() {
        assert_eq!(rolling_stdev(0, &[1.0, 2.0]), None);
        assert_eq!(rolling_stdev(1, &[1.0, 2.0]), None);
    }

    #[test]
    fn rolling_stdev_none_when_not_enough_values() {
        assert_eq!(rolling_stdev(10, &[1.0, 2.0, 3.0]), None);
    }

    #[test]
    fn rolling_stdev_zero_on_constant_tail() {
        let vals = [7.0; 20];
        let s = rolling_stdev(10, &vals).expect("enough samples");
        assert!((s - 0.0).abs() < 1e-12, "constant tail ⇒ stdev=0, got {s}");
    }

    #[test]
    fn rolling_stdev_matches_hand_calculation() {
        // Textbook set {2,4,4,4,5,5,7,9}: mean=5, Σ(xᵢ-5)² = 32.
        // Sample stdev (n−1 divisor) = √(32/7) ≈ 2.13808993529...
        //
        // Note: Wikipedia's "2.0" is the *population* stdev (n divisor).
        // We use n−1 for reasons documented in the rolling_stdev docstring.
        let vals = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let s = rolling_stdev(8, &vals).expect("enough samples");
        let expected = (32.0_f64 / 7.0).sqrt();
        assert!((s - expected).abs() < 1e-12, "expected {expected}, got {s}");
    }

    #[test]
    fn rolling_stdev_uses_only_tail() {
        // First half is noisy, last half is constant. Rolling over last
        // 4 should ignore the noise entirely.
        let vals = [100.0, -100.0, 50.0, -50.0, 3.0, 3.0, 3.0, 3.0];
        let s = rolling_stdev(4, &vals).expect("enough");
        assert!(s < 1e-12, "tail-only, got {s}");
    }

    // ----- sample_count_predicate ---------------------------------------

    #[test]
    fn sample_count_predicate_boundary() {
        assert!(!sample_count_predicate(0, 1));
        assert!(!sample_count_predicate(29, 30));
        assert!(sample_count_predicate(30, 30));
        assert!(sample_count_predicate(1_000, 30));
    }

    #[test]
    fn sample_count_predicate_zero_target_always_true() {
        // target=0 means "no gate" — always fire. Unusual but consistent
        // with the comparator semantics.
        assert!(sample_count_predicate(0, 0));
        assert!(sample_count_predicate(10, 0));
    }

    // ----- operator_trigger_predicate -----------------------------------

    #[test]
    fn operator_trigger_predicate_none_path_is_false() {
        let env = StubEnv::default();
        assert!(!operator_trigger_predicate(&env, None));
    }

    #[test]
    fn operator_trigger_predicate_detects_existing_file() {
        let env = StubEnv::default().with_path("/tmp/stop-u2");
        assert!(operator_trigger_predicate(&env, Some("/tmp/stop-u2")));
        assert!(!operator_trigger_predicate(&env, Some("/tmp/absent")));
    }

    // ----- variance_threshold_predicate (composite) ----------------------

    #[test]
    fn variance_threshold_stationary_series_converges() {
        // 100 samples at 1.0 with tiny noise → stdev well below 0.01,
        // variance well below 0.05.
        let vals: Vec<f64> = (0..100)
            .map(|i| {
                // tiny deterministic wobble, ±0.001
                1.0 + (f64::from(i % 3) - 1.0) * 0.001
            })
            .collect();
        assert!(variance_threshold_predicate(20, 30, 0.05, &vals));
    }

    #[test]
    fn variance_threshold_monotone_asymptotic_series_converges() {
        // 1 − 1/(n+1) → 1. Last window of a long series is all ≈ 1.
        let vals: Vec<f64> = (1..=200).map(|n| 1.0 - 1.0 / f64::from(n)).collect();
        assert!(
            variance_threshold_predicate(20, 30, 0.001, &vals),
            "asymptotic monotone should converge — stdev of the tail is tiny"
        );
    }

    #[test]
    fn variance_threshold_stuck_sensor_guarded_by_min_samples() {
        // Same value forever, but only 5 samples on hand. Stdev would
        // say "converged!" — min_samples floor must veto.
        let vals = [42.0; 5];
        assert!(
            !variance_threshold_predicate(3, 30, 0.01, &vals),
            "5 samples should NOT satisfy min_samples=30"
        );

        // Now with enough samples, the stuck sensor converges trivially.
        let vals = [42.0; 40];
        assert!(
            variance_threshold_predicate(10, 30, 0.01, &vals),
            "40 stuck-sensor samples should satisfy min_samples=30 and stdev=0"
        );
    }

    #[test]
    fn variance_threshold_drifting_series_does_not_converge() {
        // Linear drift from 0 to 100, 1 unit per sample. Rolling stdev
        // over window=20 is ≈ 5.77 (stdev of {0..19}). Variance ≈ 33.3.
        // Threshold 0.05 → must not converge.
        let vals: Vec<f64> = (0..100).map(f64::from).collect();
        assert!(
            !variance_threshold_predicate(20, 30, 0.05, &vals),
            "drifting series must not converge with tight threshold"
        );
    }

    #[test]
    fn variance_threshold_empty_input_does_not_converge() {
        assert!(!variance_threshold_predicate(20, 30, 0.05, &[]));
    }

    // ----- read_samples_tolerant ----------------------------------------

    #[test]
    fn read_samples_missing_file_emits_missing_warning_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("never-existed.tsv");

        let out = read_samples_tolerant(&path);
        assert!(out.values.is_empty());
        assert_eq!(out.warnings.len(), 1);
        assert_eq!(out.warnings[0].kind, WarningKind::MissingFile);
        assert_eq!(out.warnings[0].line, None);
    }

    #[test]
    fn read_samples_empty_file_emits_empty_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("blank.tsv");
        fs::write(&path, "").unwrap();

        let out = read_samples_tolerant(&path);
        assert!(out.values.is_empty());
        assert_eq!(out.warnings.len(), 1);
        assert_eq!(out.warnings[0].kind, WarningKind::EmptyFile);
    }

    #[test]
    fn read_samples_comments_and_blanks_only_is_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("comments.tsv");
        fs::write(&path, "# header\n\n   \n#another\n").unwrap();

        let out = read_samples_tolerant(&path);
        assert!(out.values.is_empty());
        assert_eq!(out.warnings.len(), 1);
        assert_eq!(out.warnings[0].kind, WarningKind::EmptyFile);
    }

    #[test]
    fn read_samples_single_column_happy_path() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("values.tsv");
        fs::write(&path, "1.0\n2.0\n3.5\n").unwrap();

        let out = read_samples_tolerant(&path);
        assert_eq!(out.values, vec![1.0, 2.0, 3.5]);
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn read_samples_reads_last_column_of_multi_column_tsv() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("metric.tsv");
        fs::write(
            &path,
            "# ts\tlabel\tvalue\n\
             2026-04-19T09:00:00Z\tlatency_ms\t12.3\n\
             2026-04-19T09:00:30Z\tlatency_ms\t14.1\n",
        )
        .unwrap();

        let out = read_samples_tolerant(&path);
        assert_eq!(out.values, vec![12.3, 14.1]);
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn read_samples_skips_malformed_rows_but_keeps_good_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("mixed.tsv");
        fs::write(
            &path,
            "# header\n\
             1.0\n\
             not-a-number\n\
             2.0\n\
             ts\tlabel\talso-nope\n\
             3.0\n",
        )
        .unwrap();

        let out = read_samples_tolerant(&path);
        assert_eq!(out.values, vec![1.0, 2.0, 3.0]);
        assert_eq!(out.warnings.len(), 2);
        for w in &out.warnings {
            assert_eq!(w.kind, WarningKind::MalformedRow);
            assert!(w.line.is_some(), "malformed rows carry a line number");
        }
    }

    #[test]
    fn read_samples_tolerates_trailing_whitespace_and_tabs() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("space.tsv");
        fs::write(&path, "  1.0  \n\t2.0\t\n").unwrap();

        let out = read_samples_tolerant(&path);
        assert_eq!(out.values, vec![1.0, 2.0]);
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn read_samples_survives_massive_malformed_file() {
        // Degrade-safely smoke test: 1000 malformed rows, zero values.
        // The caller gets an empty values vec + 1000 warnings — the
        // scheduler keeps ticking.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("noise.tsv");
        let junk = "nope nope nope\n".repeat(1000);
        fs::write(&path, junk).unwrap();

        let out = read_samples_tolerant(&path);
        assert!(out.values.is_empty());
        assert_eq!(out.warnings.len(), 1000);
    }

    // ----- end-to-end convergence shapes (readme examples in test form) -

    #[test]
    fn convergence_end_to_end_stationary_reads_then_converges() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("stationary.tsv");
        // 50 samples at 0.5 ± 0.001
        let body: String = (0..50).fold(String::new(), |mut acc, i| {
            use std::fmt::Write as _;
            let _ = writeln!(acc, "0.{}", 500 + (i % 3));
            acc
        });
        fs::write(&path, body).unwrap();

        let read = read_samples_tolerant(&path);
        assert!(read.warnings.is_empty());
        assert!(variance_threshold_predicate(10, 30, 0.001, &read.values));
    }

    #[test]
    fn convergence_end_to_end_empty_source_stays_alive() {
        // Patrol with a sample file that is present but empty must NOT
        // sunset — the predicate must say "keep collecting".
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("empty.tsv");
        fs::write(&path, "# header only\n").unwrap();

        let read = read_samples_tolerant(&path);
        assert_eq!(read.warnings.len(), 1);
        assert_eq!(read.warnings[0].kind, WarningKind::EmptyFile);
        assert!(!variance_threshold_predicate(10, 30, 0.001, &read.values));
        assert!(!sample_count_predicate(read.values.len() as u64, 30));
    }
}
