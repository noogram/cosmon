#!/usr/bin/env python3
"""Retrospective scan — analyze a cosmon run from events.jsonl + state/.

Quick observability tool pending the formal retrospective tool design
(delib-20260412-01f9). Identifies dysfunctions, summarizes activity,
samples content. Run from a project root containing .cosmon/.

Usage:
    python3 /path/to/cosmon/tools/retro-scan.py                   # scan .cosmon/
    python3 /path/to/cosmon/tools/retro-scan.py --json > report.json
"""

import argparse
import json
import os
import sys
from collections import Counter, defaultdict
from datetime import datetime
from pathlib import Path


def load_events(events_path: Path) -> list[dict]:
    if not events_path.exists():
        return []
    return [json.loads(line) for line in events_path.read_text().splitlines() if line.strip()]


def load_molecules(mol_dir: Path) -> dict[str, dict]:
    mols = {}
    if not mol_dir.exists():
        return mols
    for m in mol_dir.iterdir():
        state_file = m / "state.json"
        if state_file.exists():
            mols[m.name] = json.loads(state_file.read_text())
    return mols


def role_from_topic(topic: str) -> str:
    topic = (topic or "").upper()
    for role in ("MISSION-CONTROLLER", "RESEARCHER-READER", "CITATION-ARBITER",
                 "HORIZONTAL-EDITOR", "CHIEF-EDITOR", "FACT-CHECK", "FACT-CHECKER",
                 "REVIEWER", "REVIEW", "WRITER", "WRITE", "REVISION", "RE-REVIEW"):
        if role in topic:
            return role.lower()
    return "?"


def dysfunction_scan(mols: dict) -> dict:
    report = {
        "orphan_ready": [],       # pending but all blockers done — should have been dispatched
        "dead_running": [],        # running but no session OR very old updated_at
        "stuck_molecules": [],
        "circular_blockers": [],
    }

    for mid, m in mols.items():
        status = m.get("status")
        blockers = [link["source"] for link in m.get("typed_links", []) if link.get("rel") == "blocked_by"]

        if status == "pending" and blockers:
            all_done = all(
                mols.get(b, {}).get("status") in ("completed", "frozen")
                for b in blockers
            )
            if all_done:
                report["orphan_ready"].append({
                    "id": mid,
                    "role": role_from_topic(m.get("variables", {}).get("topic", "")),
                    "blockers": blockers,
                })

        if status == "running":
            # Detect stale: not updated in > 10 min
            updated = m.get("updated_at")
            if updated:
                dt = datetime.fromisoformat(updated.replace("Z", "+00:00"))
                age = (datetime.now(dt.tzinfo) - dt).total_seconds()
                if age > 600:
                    report["dead_running"].append({
                        "id": mid,
                        "age_minutes": round(age / 60, 1),
                        "role": role_from_topic(m.get("variables", {}).get("topic", "")),
                    })

        if status == "stuck":
            report["stuck_molecules"].append({
                "id": mid,
                "reason": m.get("collapse_reason", "(no reason)"),
                "role": role_from_topic(m.get("variables", {}).get("topic", "")),
            })

    return report


def summarize_run(events: list[dict], mols: dict) -> dict:
    status_counts = Counter(m.get("status") for m in mols.values())
    role_counts = Counter(role_from_topic(m.get("variables", {}).get("topic", "")) for m in mols.values())

    event_types = Counter(e.get("type") or e.get("kind") for e in events)

    t0 = t1 = None
    for e in events:
        ts = e.get("timestamp")
        if ts:
            dt = datetime.fromisoformat(ts.replace("Z", "+00:00"))
            if t0 is None or dt < t0:
                t0 = dt
            if t1 is None or dt > t1:
                t1 = dt

    duration_min = round((t1 - t0).total_seconds() / 60, 1) if t0 and t1 else 0

    return {
        "molecules_total": len(mols),
        "status_counts": dict(status_counts),
        "role_counts": dict(role_counts),
        "events_total": len(events),
        "event_types": dict(event_types),
        "duration_minutes": duration_min,
        "timespan": f"{t0.isoformat() if t0 else '?'} → {t1.isoformat() if t1 else '?'}",
    }


def sample_content(project_root: Path) -> dict:
    samples = {"wiki": [], "research": [], "reviews": []}
    for subdir, key in [("wiki", "wiki"), ("research", "research"), ("contributors/reviews", "reviews")]:
        d = project_root / subdir
        if not d.exists():
            continue
        for f in sorted(d.glob("*.md"))[:5]:
            content = f.read_text()
            # First non-empty non-header line
            first_para = next(
                (line for line in content.split("\n")
                 if line.strip() and not line.startswith("#") and not line.startswith("---")),
                "(empty)",
            )
            samples[key].append({
                "file": f.name,
                "size_bytes": len(content),
                "lines": content.count("\n"),
                "first_para": first_para[:120],
            })
    return samples


def emit_text(project_root: Path, summary: dict, dys: dict, samples: dict, events: list):
    print(f"\n═══ RETROSPECTIVE SCAN: {project_root} ═══\n")
    print(f"Molecules: {summary['molecules_total']} | Events: {summary['events_total']} | "
          f"Duration: {summary['duration_minutes']} min")
    print(f"Timespan: {summary['timespan']}\n")

    print("── Status distribution ──")
    for status, count in sorted(summary["status_counts"].items(), key=lambda x: -x[1]):
        print(f"  {status:12} {count}")

    print("\n── Role distribution ──")
    for role, count in sorted(summary["role_counts"].items(), key=lambda x: -x[1]):
        print(f"  {role:20} {count}")

    print("\n── Event types ──")
    for etype, count in sorted(summary["event_types"].items(), key=lambda x: -x[1]):
        print(f"  {str(etype):30} {count}")

    print(f"\n── DYSFUNCTIONS ──")
    if dys["orphan_ready"]:
        print(f"\n🔴 ORPHAN-READY ({len(dys['orphan_ready'])}) — pending with all blockers done, NOT dispatched:")
        for o in dys["orphan_ready"][:10]:
            print(f"   {o['id']}  {o['role']}")
    if dys["dead_running"]:
        print(f"\n🔴 DEAD-RUNNING ({len(dys['dead_running'])}) — running but stale (>10 min):")
        for d in dys["dead_running"][:10]:
            print(f"   {d['id']}  {d['role']}  age={d['age_minutes']} min")
    if dys["stuck_molecules"]:
        print(f"\n🟡 STUCK ({len(dys['stuck_molecules'])}):")
        for s in dys["stuck_molecules"][:10]:
            print(f"   {s['id']}  {s['role']}  reason: {s['reason'][:80]}")
    if not any(dys.values()):
        print("\n  ✅ No dysfunctions detected.")

    print("\n── CONTENT SAMPLES ──")
    for kind, items in samples.items():
        if items:
            print(f"\n  {kind}/ ({len(items)} files shown):")
            for s in items:
                print(f"    {s['file']:50} {s['size_bytes']:>6}B  {s['first_para']}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--project-root", default=".", help="Project root (default: cwd)")
    parser.add_argument("--json", action="store_true", help="Emit JSON only")
    args = parser.parse_args()

    root = Path(args.project_root).resolve()
    cosmon_dir = root / ".cosmon"
    events = load_events(cosmon_dir / "state" / "events.jsonl")
    mols = load_molecules(cosmon_dir / "state" / "fleets" / "default" / "molecules")

    if not mols:
        print(f"No molecules found in {cosmon_dir}", file=sys.stderr)
        return 1

    summary = summarize_run(events, mols)
    dys = dysfunction_scan(mols)
    samples = sample_content(root)

    if args.json:
        print(json.dumps({
            "project": str(root),
            "summary": summary,
            "dysfunctions": dys,
            "content_samples": samples,
        }, indent=2, default=str))
    else:
        emit_text(root, summary, dys, samples, events)

    return 0


if __name__ == "__main__":
    sys.exit(main())
