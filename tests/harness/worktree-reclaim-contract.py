#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Prove the design tables have one solution; refute the old lock domain.

Python 3 standard library only. No Git, Cargo, filesystem mutations or CI.
"""
import argparse
import itertools
from pathlib import Path

CONTRACT = Path(__file__).resolve().parents[2] / 'docs/design/worktree-reclaim/CONTRACT.md'
STATUSES = ('Pending', 'Queued', 'Running', 'Frozen', 'Starved', 'Completed', 'Collapsed', 'Unknown')
DOMAINS = (STATUSES, ('Registered', 'Unregistered', 'Unknown'),
           ('Held', 'Acquired', 'ProbeFailed'), ('Zero', 'Positive', 'Unknown'),
           ('Clean', 'Dirty', 'Unknown'), ('Absent', 'Present', 'Unknown'),
           ('Present', 'Absent', 'Unknown'))


def cells(line):
    """Read the normative Markdown tables rather than a second expected table."""
    return tuple(part.strip() for part in line.strip().strip('|').split('|'))


def matches(pattern, value):
    """Wildcard includes Unknown; comma alternatives are explicit finite sets."""
    return pattern == '*' or value in [p.strip() for p in pattern.split(',')]


def load_tables():
    """Load the G normalization and the two disjoint output partitions."""
    lines = CONTRACT.read_text().splitlines()
    gate = [cells(line) for line in lines if line.startswith('| Absent | * |')
            or line.startswith('| Present | Completed,')
            or line.startswith('| Present | Pending,')
            or line.startswith('| Unknown | * |')]
    derived = [cells(line) for line in lines if line.startswith('| d')]
    durable = [cells(line) for line in lines if line.startswith('| h')]
    assert len(gate) == 4 and len(derived) == 4 and len(durable) == 6
    return gate, derived, durable


def classify(row, tables, legacy):
    """Require exactly one class in each normative partition for every row."""
    s, r, lock, a, d, i, m = row
    gate, derived, durable = tables
    gs = [g for mp, sp, g in gate if matches(mp, m) and matches(sp, s)]
    assert len(gs) == 1, ('gate overlap/gap', row)
    g = gs[0]
    observed_lock = 'ProbeFailed' if legacy and lock == 'Absent' else lock
    ds = [c for c in derived if matches(c[1], g) and matches(c[2], observed_lock)
          and c[3] == '*']
    hs = [c for c in durable if all(matches(p, v) for p, v in
          zip(c[1:7], (g, r, a, d, i, observed_lock)))]
    assert len(ds) == len(hs) == 1, ('partition overlap/gap', row)
    return g, ds[0], hs[0]


def selected(w, output):
    """Map both binary outputs to exact sets with two configurable roots."""
    derived, durable = output
    return (frozenset((w + '/target', w + '/cache')) if derived else frozenset(),
            frozenset((w,)) if durable else frozenset())


def solve(legacy=False, emit_table=False):
    """Enumerate all local assignments and count global solutions by factoring."""
    domains = list(DOMAINS)
    if legacy:
        domains[2] = ('Held', 'Absent')
    tables = load_tables()
    solutions, classes, gates = {}, {}, {}
    count = 1
    for n, row in enumerate(itertools.product(*domains)):
        g, dc, hc = classify(row, tables, legacy)
        w = 'fixture/' + str(n)
        expected_d = {'= {}': frozenset(), '= E(w)': frozenset((w + '/target', w + '/cache'))}[dc[-1]]
        expected_h = {'= {}': frozenset(), '= H(w)': frozenset((w,))}[hc[-1]]
        candidates = [out for out in itertools.product((False, True), repeat=2)
                      if selected(w, out) == (expected_d, expected_h)]
        count *= len(candidates)
        assert len(candidates) == 1, ('local assignment not unique', row)
        solutions[row] = candidates[0]
        classes[row], gates[row] = (dc[0], hc[0]), g
        if emit_table:
            print(','.join(row + (dc[0], hc[0], str(candidates[0][0]), str(candidates[0][1]))))
    expected_rows = 3888 if legacy else 5832
    assert len(solutions) == expected_rows
    assert {c[0] for c in classes.values()} == ({'d0', 'd1', 'd2'} if legacy else {'d0', 'd1', 'd2', 'd3'})
    assert {c[1] for c in classes.values()} == {'h0', 'h1', 'h2', 'h3', 'h4', 'h5'}

    # Cross-row constraints filter the sole locally satisfying global candidate.
    failures = set()
    unlocked = 'Absent' if legacy else 'Acquired'
    for row, out in solutions.items():
        s, r, lock, a, d, i, m = row
        ds, hs = selected('w', out)
        if d == 'Dirty' or a == 'Positive':
            assert hs == frozenset(), ('false-red durable', row)
        if gates[row] == 'Yes' and lock == 'Held':
            flipped = (s, r, unlocked, a, d, i, m)
            after = selected('w', solutions[flipped])
            if not (ds == frozenset() and after[0] == frozenset(('w/target', 'w/cache'))
                    and hs == after[1]):
                failures.add('lock differential')
        # Irrelevant axes must preserve each predicate, even on Unknown.
        for axis in (1, 3, 4, 5):
            for value in domains[axis]:
                other = list(row)
                other[axis] = value
                assert selected('w', solutions[tuple(other)])[0] == ds
        for value in domains[2]:
            other = list(row)
            other[2] = value
            assert selected('w', solutions[tuple(other)])[1] == hs

    base = ('Collapsed', 'Registered', unlocked, 'Zero', 'Clean', 'Absent', 'Present')
    for axis, value in ((1, 'Unregistered'), (3, 'Positive'), (4, 'Dirty'), (5, 'Present')):
        other = list(base)
        other[axis] = value
        before, after = selected('w', solutions[base]), selected('w', solutions[tuple(other)])
        assert before[1] == frozenset(('w',)) and after[1] == frozenset()
        assert before[0] == after[0]
    required = ('Collapsed', 'Registered', unlocked, 'Positive', 'Dirty', 'Absent', 'Present')
    if selected('w', solutions[required]) != (frozenset(('w/target', 'w/cache')), frozenset()):
        failures.add('collapsed dirty non-ancestor yields target')
    # Batch equalities also catch omitted or extra roots, using unique paths.
    for predicate, selecting_class in ((0, 'd3'), (1, 'h5')):
        actual, expected = set(), set()
        for n, (row, out) in enumerate(solutions.items()):
            w = 'fixture/' + str(n)
            actual.update(selected(w, out)[predicate])
            if classes[row][predicate] == selecting_class:
                expected.update((w + '/target', w + '/cache') if predicate == 0 else (w,))
        assert actual == expected
    final_count = 0 if failures else count
    print('rows={}, local assignments checked={}, satisfying assignments={}'.format(
        len(solutions), len(solutions) * 4, final_count))
    for failure in sorted(failures):
        print('unsatisfied: ' + failure)
    assert final_count == (0 if legacy else 1)
    return 0 if final_count == 1 else 1


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--legacy-two-state', action='store_true')
    parser.add_argument('--table', action='store_true', help='print every expanded row as CSV')
    args = parser.parse_args()
    raise SystemExit(solve(args.legacy_two_state, args.table))
