#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Turn `hookmatrix.py` trial lines into the tables the write-up quotes.

Deliberately dumb: medians and counts, no smoothing, no exclusion of outliers.
Every trial that ran appears in exactly one row of exactly one table, and any
trial with an `error` is reported as an error rather than dropped — a matrix
that silently drops its failures is the failure.
"""

import json
import statistics
import sys
from collections import defaultdict


def load(paths):
    """Every trial line, with the injected CPU load folded into the scenario.

    The load condition is part of what a row *is*, not a footnote: the same
    scenario at load 0 and at load 8 are different measurements, and merging
    them would silently average a machine that was busy with one that was not.
    """
    rows = []
    for p in paths:
        with open(p) as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                r = json.loads(line)
                if r.get("cpu_load"):
                    r["scenario"] = f"{r['scenario']}+load{r['cpu_load']}"
                rows.append(r)
    return rows


def pct(n, d):
    return "—" if not d else f"{100.0 * n / d:.0f} %"


def quantiles(xs):
    if not xs:
        return "—"
    xs = sorted(xs)
    med = statistics.median(xs)
    return f"{med:.0f} [{xs[0]:.0f}–{xs[-1]:.0f}]"


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: aggregate.py <results.jsonl...>", file=sys.stderr)
        return 2
    rows = load(sys.argv[1:])

    errors = [r for r in rows if r.get("error")]
    ok = [r for r in rows if not r.get("error")]
    print(f"trials: {len(rows)} ({len(errors)} harness errors)")
    for r in errors:
        print(f"  ERROR {r['arm']}/{r['scenario']} rep={r['rep']}: {r['error']}")
    print()

    # ---- per (arm, scenario) summary -------------------------------------
    groups = defaultdict(list)
    for r in ok:
        for d in r.get("dispatches", []):
            groups[(r["arm"], r["scenario"])].append((r, d))

    print("| arm | scenario | n | evidence | latency ms med [min–max] | CRs/dispatch med | acks med | left pending |")
    print("|---|---|---|---|---|---|---|---|")
    for key in sorted(groups):
        pairs = groups[key]
        n = len(pairs)
        ev = defaultdict(int)
        for _, d in pairs:
            ev[d["evidence"]] += 1
        evs = " ".join(f"{k}={v}" for k, v in sorted(ev.items()))
        lat = [d["latency_ms"] for _, d in pairs if d.get("latency_ms") is not None]
        crs = [r["cr_count_after_paste"] for r, _ in pairs if "cr_count_after_paste" in r]
        acks = [r["ack_count"] for r, _ in pairs if "ack_count" in r]
        pend = sum(1 for _, d in pairs if d.get("composer_pending_after"))
        print(
            f"| {key[0]} | {key[1]} | {n} | {evs} | {quantiles(lat)} | "
            f"{statistics.median(crs):.0f} | {statistics.median(acks):.0f} | "
            f"{pend}/{n} |"
        )
    print()

    # ---- hook latency, isolated ------------------------------------------
    iso = [
        (r, d)
        for r, d in [(r, d) for r in ok for d in r.get("dispatches", [])]
        if r["scenario"] == "no_retry"
    ]
    if iso:
        print("## Hook latency, single carriage return (no retry can confound it)")
        by_size = defaultdict(list)
        swallowed = defaultdict(lambda: [0, 0])
        for r, d in iso:
            size = r["size_lines"]
            swallowed[size][1] += 1
            if d["evidence"] == "event_ack":
                hook_ts = (d.get("extra") or {}).get("hook_ts")
                crs = r.get("cr_ts") or []
                if hook_ts and crs:
                    by_size[size].append((hook_ts - crs[0]) * 1000)
            else:
                swallowed[size][0] += 1
        print()
        print("| briefing lines | n | receipt not delivered | CR→hook ms med [min–max] |")
        print("|---|---|---|---|")
        for size in sorted(swallowed):
            bad, tot = swallowed[size]
            print(f"| {size} | {tot} | {bad} ({pct(bad, tot)}) | {quantiles(by_size[size])} |")
        allms = [x for v in by_size.values() for x in v]
        print(f"| **all** | {len(iso)} | "
              f"{sum(v[0] for v in swallowed.values())} | {quantiles(allms)} |")
        print()

    # ---- the safety property: no CRs create a second submission ----------
    print("## Duplicate carriage returns vs. duplicate submissions")
    print()
    print("| arm | scenario | trials | CRs > 1 | acks > dispatches |")
    print("|---|---|---|---|---|")
    per_trial = defaultdict(list)
    for r in ok:
        per_trial[(r["arm"], r["scenario"])].append(r)
    for key in sorted(per_trial):
        trials = per_trial[key]
        multi_cr = sum(1 for r in trials if (r.get("cr_count_after_paste") or 0) > 1)
        over = sum(
            1
            for r in trials
            if r.get("ack_count", 0) > len(r.get("dispatches", []) or [1])
        )
        print(f"| {key[0]} | {key[1]} | {len(trials)} | {multi_cr} | {over} |")
    print()

    # ---- fallback typing --------------------------------------------------
    print("## What each failure mode produced")
    print()
    print("| scenario | evidence | fallback_reason | acks on disk | sentinel in pane |")
    print("|---|---|---|---|---|")
    seen = set()
    for r in ok:
        for d in r.get("dispatches", []):
            k = (r["scenario"], d["evidence"], d.get("fallback_reason"))
            if k in seen:
                continue
            seen.add(k)
            print(
                f"| {r['scenario']} | {d['evidence']} | {d.get('fallback_reason') or '—'} "
                f"| {r.get('ack_count')} | {r.get('sentinel_anywhere')} |"
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
