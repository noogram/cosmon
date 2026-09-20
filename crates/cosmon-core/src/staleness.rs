// SPDX-License-Identifier: AGPL-3.0-only

//! Staleness arithmetic — the one definition of "how old is the backlog".
//!
//! `cs status` and `cs peek` both answer "what has been sitting here too
//! long". They used to answer it twice: `peek` counted `pending` rows older
//! than 48h in its vitals line, and `status` had no notion of age at all. A
//! pending molecule went unnoticed for 39 days on this repository because the
//! command a session actually runs first could not see what the TUI already
//! computed.
//!
//! Two definitions of staleness drift; one cannot. So the predicate lives
//! here, in the I/O-free core, and both surfaces call it. Nothing in this
//! module touches the filesystem, the clock, or a store — `now` is an
//! argument, which is also what makes the threshold testable at the boundary
//! rather than only in a fixture that happens to be old enough.
//!
//! # What counts as backlog
//!
//! [`counts_as_backlog`] is the whole rule: a molecule is backlog when it is
//! waiting for somebody to pick it up ([`MoleculeStatus::Pending`]) **and** it
//! is not a lease.
//!
//! A *lease molecule* carries a pilot lease between successive cockpit
//! sessions (see `cosmon_core::pilot_lease`). It is `Pending` by construction
//! and converges by design never — a session takes the lease at an epoch,
//! works, dies, and the next session takes it at the next epoch. Counting it
//! as backlog makes every backlog number permanently wrong by one, which
//! teaches the reader to discount the number. The lease bit is supplied by
//! the caller because deciding it needs the lease ledger on disk, which is an
//! adapter concern; the *rule* that a lease is not backlog is a domain fact
//! and belongs here.

use chrono::{DateTime, Duration, Utc};

use crate::id::MoleculeId;
use crate::molecule::MoleculeStatus;

/// How long a `Pending` molecule may sit before it is called stale.
///
/// 48 hours, the threshold `cs peek` has rendered in its vitals line since it
/// gained one. Kept as the single constant both surfaces read so the number
/// in the TUI and the number in `cs status` cannot disagree.
#[must_use]
pub fn stale_backlog_after() -> Duration {
    Duration::hours(48)
}

/// How long a surface projection may sit before its freshness is doubted.
///
/// Seven days. The figure is not a deadline — a reconcile is cheap and a
/// project that has not needed one for a week is usually a project nobody
/// touched — it is the point past which a green tick would be asserting
/// something it has not checked.
#[must_use]
pub fn stale_reconcile_after() -> Duration {
    Duration::days(7)
}

/// One molecule reduced to what staleness arithmetic needs.
///
/// Deliberately not `MoleculeData`: `cs peek` builds its rows from a snapshot
/// that has already dropped most of the record, and forcing it to reconstitute
/// a full molecule just to be counted would be the kind of coupling that makes
/// a shared definition unattractive enough to fork.
#[derive(Debug, Clone)]
pub struct BacklogItem {
    /// The molecule this item stands for, so the oldest one can be named.
    pub id: MoleculeId,
    /// Current lifecycle status.
    pub status: MoleculeStatus,
    /// Nucleation time. `None` for a legacy molecule whose record predates
    /// the field — it is counted as backlog but contributes no age, because
    /// an invented timestamp is a fabricated fact.
    pub created_at: Option<DateTime<Utc>>,
    /// True when this molecule carries a pilot lease. See the module doc.
    pub is_lease: bool,
}

/// Does this `(status, is_lease)` pair count toward the backlog?
///
/// The whole rule, in one place, so a caller cannot half-apply it.
#[must_use]
pub const fn counts_as_backlog(status: MoleculeStatus, is_lease: bool) -> bool {
    matches!(status, MoleculeStatus::Pending) && !is_lease
}

/// The staleness summary a session needs at the moment it opens.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BacklogAge {
    /// Molecules that count as backlog.
    pub counted: usize,
    /// Of those, how many are older than [`stale_backlog_after`].
    pub stale: usize,
    /// Age of the oldest counted molecule, or `None` when the backlog is
    /// empty or carries no usable timestamp.
    pub oldest: Option<Duration>,
    /// Which molecule that age belongs to. Paired with `oldest` so a reader
    /// can go look at it without a second query.
    pub oldest_id: Option<MoleculeId>,
    /// Lease molecules skipped. Reported rather than silently dropped: a
    /// number that excludes something must be able to say what.
    pub leases_excluded: usize,
}

/// Fold a set of molecules into the staleness summary.
///
/// `now` is passed in rather than read, so a test can age a fixture past the
/// threshold and back under it without sleeping.
#[must_use]
pub fn backlog_age<I>(items: I, now: DateTime<Utc>) -> BacklogAge
where
    I: IntoIterator<Item = BacklogItem>,
{
    let threshold = stale_backlog_after();
    let mut out = BacklogAge::default();

    for item in items {
        if item.is_lease && matches!(item.status, MoleculeStatus::Pending) {
            out.leases_excluded += 1;
            continue;
        }
        if !counts_as_backlog(item.status, item.is_lease) {
            continue;
        }
        out.counted += 1;
        let Some(created) = item.created_at else {
            continue;
        };
        let age = now.signed_duration_since(created);
        if age > threshold {
            out.stale += 1;
        }
        if out.oldest.is_none_or(|current| age > current) {
            out.oldest = Some(age);
            out.oldest_id = Some(item.id);
        }
    }

    out
}

/// Is a projection of this age old enough that a green tick would mislead?
#[must_use]
pub fn reconcile_is_stale(age: Duration) -> bool {
    age > stale_reconcile_after()
}

/// Render a duration as a compact age token — `39d`, `5h`, `12m`, `30s`.
///
/// No "ago" suffix: the caller decides whether the number is an age or a
/// delay, and a formatter that presumes tends to be wrapped rather than
/// reused. A negative duration (clock skew, a molecule created "in the
/// future") renders as `0s` rather than a minus sign nobody can act on.
#[must_use]
pub fn format_age(d: Duration) -> String {
    let secs = d.num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mol(n: &str) -> MoleculeId {
        MoleculeId::new(format!("task-20260811-{n}")).expect("valid fixture id")
    }

    /// A fixture aged exactly `age` relative to `now`.
    ///
    /// `now` is threaded through rather than read here: reading the clock
    /// twice makes "exactly 48h" land a few microseconds under the threshold,
    /// which is the boundary these tests exist to pin.
    fn item(
        now: DateTime<Utc>,
        n: &str,
        status: MoleculeStatus,
        age: Duration,
        is_lease: bool,
    ) -> BacklogItem {
        BacklogItem {
            id: mol(n),
            status,
            created_at: Some(now - age),
            is_lease,
        }
    }

    /// The fixture the brief asks for: aged past the threshold the count is
    /// non-zero, aged under it the same fixture counts zero.
    #[test]
    fn stale_count_flips_on_the_48h_threshold() {
        let now = Utc::now();

        let over = backlog_age(
            vec![item(
                now,
                "aaaa",
                MoleculeStatus::Pending,
                Duration::hours(49),
                false,
            )],
            now,
        );
        assert_eq!(over.counted, 1);
        assert_eq!(over.stale, 1, "49h old must read as stale");

        let under = backlog_age(
            vec![item(
                now,
                "aaaa",
                MoleculeStatus::Pending,
                Duration::hours(47),
                false,
            )],
            now,
        );
        assert_eq!(under.counted, 1);
        assert_eq!(under.stale, 0, "47h old must not read as stale");
    }

    #[test]
    fn a_lease_is_not_backlog() {
        let now = Utc::now();
        let summary = backlog_age(
            vec![
                item(
                    now,
                    "aaaa",
                    MoleculeStatus::Pending,
                    Duration::days(39),
                    true,
                ),
                item(
                    now,
                    "bbbb",
                    MoleculeStatus::Pending,
                    Duration::hours(3),
                    false,
                ),
            ],
            now,
        );
        assert_eq!(summary.counted, 1, "only the non-lease molecule is backlog");
        assert_eq!(summary.leases_excluded, 1);
        assert_eq!(summary.stale, 0, "the 39d lease must not inflate stale");
        assert_eq!(summary.oldest_id, Some(mol("bbbb")));
    }

    #[test]
    fn oldest_names_the_molecule_it_measures() {
        let now = Utc::now();
        let summary = backlog_age(
            vec![
                item(
                    now,
                    "aaaa",
                    MoleculeStatus::Pending,
                    Duration::days(3),
                    false,
                ),
                item(
                    now,
                    "bbbb",
                    MoleculeStatus::Pending,
                    Duration::days(39),
                    false,
                ),
                item(
                    now,
                    "cccc",
                    MoleculeStatus::Pending,
                    Duration::hours(1),
                    false,
                ),
            ],
            now,
        );
        assert_eq!(summary.counted, 3);
        assert_eq!(summary.stale, 2);
        assert_eq!(summary.oldest_id, Some(mol("bbbb")));
        assert_eq!(format_age(summary.oldest.expect("an age")), "39d");
    }

    #[test]
    fn only_pending_is_backlog() {
        let now = Utc::now();
        for status in [
            MoleculeStatus::Running,
            MoleculeStatus::Queued,
            MoleculeStatus::Frozen,
            MoleculeStatus::Completed,
            MoleculeStatus::Collapsed,
        ] {
            let summary = backlog_age(
                vec![item(now, "aaaa", status, Duration::days(39), false)],
                now,
            );
            assert_eq!(summary.counted, 0, "{status} must not count as backlog");
        }
    }

    #[test]
    fn a_legacy_molecule_counts_without_an_age() {
        let now = Utc::now();
        let summary = backlog_age(
            vec![BacklogItem {
                id: mol("aaaa"),
                status: MoleculeStatus::Pending,
                created_at: None,
                is_lease: false,
            }],
            now,
        );
        assert_eq!(summary.counted, 1);
        assert_eq!(summary.stale, 0);
        assert!(summary.oldest.is_none());
    }

    #[test]
    fn reconcile_staleness_has_a_week_of_grace() {
        assert!(!reconcile_is_stale(Duration::days(6)));
        assert!(reconcile_is_stale(Duration::days(19)));
    }

    #[test]
    fn age_tokens_are_compact() {
        assert_eq!(format_age(Duration::seconds(30)), "30s");
        assert_eq!(format_age(Duration::minutes(2)), "2m");
        assert_eq!(format_age(Duration::hours(2)), "2h");
        assert_eq!(format_age(Duration::days(39)), "39d");
        assert_eq!(format_age(Duration::seconds(-5)), "0s");
    }
}
