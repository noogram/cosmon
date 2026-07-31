#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Fold `matrix.py`'s JSONL trials into the two tables the decision rule needs.

Table 1 — outcome per (briefing size x paste-to-CR delay) cell: how many trials
left the briefing sitting in the composer after a single submit keystroke.

Table 2 — the same cells read at the PTY: how many trials delivered the `0d`
byte to the application at all. The decision rule turns on the pair. A cell
with `pending` trials whose CR *did* reach the PTY is the TUI race; a cell
whose CR never reached the PTY is a defect on cosmon's side.
"""

import argparse
import json
import statistics
from collections import defaultdict


def load(path):
    rows = []
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("results")
    args = ap.parse_args()
    rows = load(args.results)

    ok = [r for r in rows if "error" not in r]
    errs = [r for r in rows if "error" in r]

    sizes = sorted({r["size_lines"] for r in ok})
    delays = sorted({r["delay_ms"] for r in ok})
    cells = defaultdict(list)
    for r in ok:
        cells[(r["size_lines"], r["delay_ms"])].append(r)

    print(f"trials: {len(rows)}  usable: {len(ok)}  errored: {len(errs)}\n")

    print("## Unsubmitted after one CR (pending / trials)\n")
    print("| lines \\ delay | " + " | ".join(f"{d} ms" for d in delays) + " |")
    print("|---|" + "---|" * len(delays))
    for s in sizes:
        cs = []
        for d in delays:
            trials = cells[(s, d)]
            pend = sum(1 for t in trials if t.get("pending_after_settle"))
            cs.append(f"{pend}/{len(trials)}")
        print(f"| {s} | " + " | ".join(cs) + " |")

    print("\n## CR observed at the application PTY (delivered / trials)\n")
    print("| lines \\ delay | " + " | ".join(f"{d} ms" for d in delays) + " |")
    print("|---|" + "---|" * len(delays))
    for s in sizes:
        cs = []
        for d in delays:
            trials = cells[(s, d)]
            got = sum(1 for t in trials if t.get("cr_after_paste"))
            cs.append(f"{got}/{len(trials)}")
        print(f"| {s} | " + " | ".join(cs) + " |")

    print("\n## Time from CR to composer clear, ms (median [min-max], n)\n")
    print("| lines \\ delay | " + " | ".join(f"{d} ms" for d in delays) + " |")
    print("|---|" + "---|" * len(delays))
    for s in sizes:
        cs = []
        for d in delays:
            vals = [
                t["clear_after_cr_ms"]
                for t in cells[(s, d)]
                if t.get("clear_after_cr_ms") is not None
            ]
            if vals:
                cs.append(
                    f"{round(statistics.median(vals))} [{min(vals)}-{max(vals)}], {len(vals)}"
                )
            else:
                cs.append("—")
        print(f"| {s} | " + " | ".join(cs) + " |")

    pend_rows = [r for r in ok if r.get("pending_after_settle")]
    print(
        f"\npending trials: {len(pend_rows)}; of those, CR reached the PTY in "
        f"{sum(1 for r in pend_rows if r.get('cr_after_paste'))}"
    )
    lost = [r for r in ok if not r.get("cr_after_paste")]
    print(f"trials whose CR never reached the PTY: {len(lost)}")
    for r in lost:
        print(
            f"  - size={r['size_lines']} delay={r['delay_ms']} rep={r['rep']} "
            f"pending={r.get('pending_after_settle')} "
            f"paste_end_seen={r.get('paste_end_seen')}"
        )
    if errs:
        print("\nerrored trials:")
        for r in errs:
            print(
                f"  - size={r.get('size_lines')} delay={r.get('delay_ms')} "
                f"rep={r.get('rep')}: {r['error']}"
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
