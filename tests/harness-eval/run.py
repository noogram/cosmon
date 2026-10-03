#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Run the frozen task corpus against harness arms and write raw runs plus a report.

Offline by default: only `mock` arms run, and they are deterministic stand-ins
that go through the same code path as a real arm. A `live` arm runs only when
`--live` and `--spend-cap-usd` are given, and only if its `command` is set in the
manifest; nothing here talks to a provider on its own.

    python3 run.py --out DIR                      # mocked arms, all tasks
    python3 run.py --out DIR --arms stub-solver,stub-noop --trials 2
    python3 run.py --fixtures-witness             # baseline fails, reference solution passes
    python3 run.py --out DIR --live --spend-cap-usd 20 --trials 5

Raw runs go to `DIR/runs/<task>__<arm>__<trial>/` (workspace, worker result,
acceptance output) and are never deleted by this script. `DIR/report.json` is
judged by `validate.py` before it is written.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import validate  # noqa: E402

FIXTURES = HERE / "fixtures"


# ---- mock workers -----------------------------------------------------------

def _overlay(src: Path, dst: Path) -> None:
    shutil.copytree(src, dst, dirs_exist_ok=True)


def _usage(rows: int, priced: bool) -> dict:
    reqs = [{"request_id": f"req-{i}", "input_tokens": 100 + i, "output_tokens": 20,
             "cost_usd": 0.0 if priced else None} for i in range(rows)]
    return {"requests": reqs}


def worker(behavior: str, workspace: Path, run_dir: Path, task_id: str) -> int:
    """Deterministic stand-in for a harness arm; writes `worker-result.json`.

    solver: applies the reference solution and reports priced usage.
    noop: changes nothing, exits 0 and claims success; its cost is unknown.
    replayer: solves the task but replays already-applied side effects after a
    simulated interruption and reports one usage row twice.
    """
    solution = FIXTURES / task_id / "solution"
    usage = _usage(2, priced=True)
    cost = {"complete": True, "total_usd": 0.0}
    if behavior == "solver":
        _overlay(solution, workspace)
    elif behavior == "noop":
        usage, cost = _usage(1, priced=False), {"complete": False, "total_usd": None}
    elif behavior == "replayer":
        _overlay(solution, workspace)
        if task_id == "interrupted-effects":
            # Interrupted after e2, continued without its record: e1 and e2 run again.
            with open(workspace / "effects.log", "a") as log:
                log.write("applied e1\napplied e2\n")
        usage["requests"].append(dict(usage["requests"][0]))
    else:
        print(f"unknown behavior {behavior!r}", file=sys.stderr)
        return 2
    (run_dir / "worker-result.json").write_text(json.dumps({
        "claimed_success": True, "model_observed": "stub-model", "usage": usage, "cost": cost,
        "interventions": 0, "retries": 0}))
    return 0


# ---- one run ----------------------------------------------------------------

def _git_revision() -> str:
    try:
        return subprocess.run(["git", "-C", str(HERE), "rev-parse", "HEAD"], capture_output=True,
                              text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return "unknown"


def _accept(task_id: str, workspace: Path) -> dict:
    done = subprocess.run([sys.executable, str(FIXTURES / task_id / "accept.py"), str(workspace)],
                          capture_output=True, text=True, timeout=120)
    try:
        verdict = json.loads(done.stdout.strip().splitlines()[-1])
    except (ValueError, IndexError):
        return {"ran": True, "passed": False, "artifacts": {}, "gate": None,
                "stderr": done.stderr[-2000:]}
    return {"ran": True, "passed": done.returncode == 0 and bool(verdict.get("passed")),
            "artifacts": verdict.get("artifacts", {}), "gate": verdict.get("gate")}


def run_one(task: dict, arm: dict, trial: int, out: Path, revision: str, model: str, live: bool) -> dict:
    run_dir = out / "runs" / f"{task['id']}__{arm['id']}__{trial}"
    evidence = "live" if arm["kind"] == "live" else "mock"
    base = {
        "task": task["id"], "arm": arm["id"], "trial": trial, "evidence": evidence,
        "revision": revision, "input_digest": validate.input_digest(task["id"]),
        "config_digest": hashlib.sha256(json.dumps(arm, sort_keys=True).encode()).hexdigest(),
        "model_requested": model if evidence == "live" else "stub-model",
        "allowed_tools": arm["tools"], "model_observed": None, "retries": 0, "interventions": 0,
        "wall_seconds": 0.0, "exit_code": None, "claimed_success": False,
        "acceptance": {"ran": False, "passed": False, "artifacts": {}, "gate": None},
        "usage": {"requests": []}, "cost": {"complete": False, "total_usd": None},
    }
    missing = [c for c in task["requires"] if c not in arm["capabilities"]]
    if missing:
        return {**base, "outcome": "inapplicable", "reason": f"arm lacks {missing}"}
    if evidence == "live" and (not live or not arm["command"]):
        return {**base, "outcome": "invalid", "reason": "live arm not configured or --live not given"}

    workspace = run_dir / "workspace"
    if run_dir.exists():
        sys.exit(f"refusing to overwrite raw run {run_dir}")
    workspace.mkdir(parents=True)
    _overlay(FIXTURES / task["id"] / "input", workspace)
    peer = FIXTURES / task["id"] / "peer_message.txt"
    argv = [a.format(python=sys.executable, run_py=str(HERE / "run.py"), workspace=workspace,
                     run_dir=run_dir, task_id=task["id"], task_file=FIXTURES / task["id"] / "task.md",
                     model=model, peer_message=peer if peer.is_file() else "")
            for a in arm["command"]]
    started = time.monotonic()
    try:
        done = subprocess.run(argv, capture_output=True, text=True, timeout=1800 if live else 120)
        exit_code, stderr = done.returncode, done.stderr
    except subprocess.TimeoutExpired:
        exit_code, stderr = 124, "timed out"
    wall = round(time.monotonic() - started, 3)
    (run_dir / "worker.stderr").write_text(stderr)
    try:
        result = json.loads((run_dir / "worker-result.json").read_text())
    except (OSError, ValueError):
        result = {}
    acceptance = _accept(task["id"], workspace)
    (run_dir / "acceptance.json").write_text(json.dumps(acceptance, indent=2))
    record = {**base, "exit_code": exit_code, "wall_seconds": wall,
              "claimed_success": bool(result.get("claimed_success")),
              "model_observed": result.get("model_observed"), "retries": result.get("retries", 0),
              "interventions": result.get("interventions", 0), "acceptance": acceptance,
              "usage": result.get("usage") or {"requests": []},
              "cost": result.get("cost") or {"complete": False, "total_usd": None}}
    record["outcome"] = "accepted" if acceptance["passed"] else "rejected"
    problems = validate.validate_run(record, task, arm)
    if problems:
        record["outcome"], record["problems"] = "invalid", problems
    (run_dir / "record.json").write_text(json.dumps(record, indent=2))
    return record


# ---- report -----------------------------------------------------------------

def wilson(k: int, n: int, z: float = 1.96) -> list[float]:
    """95% Wilson interval for k accepted out of n applicable runs."""
    if n == 0:
        return [0.0, 1.0]
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return [round(max(0.0, c - h), 4), round(min(1.0, c + h), 4)]


def build_report(runs: list[dict], manifest: dict, planned: int) -> dict:
    summary: dict = {}
    for r in runs:
        row = summary.setdefault(r["evidence"], {}).setdefault(r["arm"], {
            "runs": 0, "accepted": 0, "rejected": 0, "inapplicable": 0, "invalid": 0,
            "interventions": 0, "cost_complete_runs": 0})
        row["runs"] += 1
        row[r["outcome"]] += 1
        row["interventions"] += r.get("interventions", 0)
        row["cost_complete_runs"] += 1 if (r.get("cost") or {}).get("complete") else 0
    baseline = manifest["policy"]["baseline_arm"]
    live = [r for r in runs if r["evidence"] == "live"]
    per_arm = {}
    for arm in {r["arm"] for r in live}:
        mine = [r for r in live if r["arm"] == arm and r["outcome"] != "inapplicable"]
        k = sum(1 for r in mine if r["outcome"] == "accepted")
        per_arm[arm] = {"accepted": k, "applicable_runs": len(mine), "wilson95": wilson(k, len(mine))}
    if not live:
        uncertainty = ("Mocked evidence only: it exercises the evaluator, not the harness. "
                       "No routing conclusion follows.")
    elif baseline not in per_arm:
        uncertainty = f"No live runs of the baseline arm {baseline!r}; no comparison is possible."
    else:
        uncertainty = ("Intervals are 95% Wilson intervals over applicable runs; the operator's "
                       "adoption threshold and trial count were not computed here.")
    recommendation = {"switch_default": False, "operator_threshold_met": False,
                      "uncertainty": uncertainty, "per_arm_live": per_arm,
                      "note": "The runner never switches a default; the operator decides from the live section."}
    return {"corpus_version": manifest["corpus_version"], "planned_runs": planned, "runs": runs,
            "summary": summary, "recommendation": recommendation}


def ordered_arms(arms: list[dict], trial: int) -> list[dict]:
    """Rotate-left-by-trial ordering from the manifest policy."""
    k = trial % len(arms)
    return arms[k:] + arms[:k]


def fixtures_witness(manifest: dict) -> int:
    """Baseline input must fail acceptance and the reference solution must pass it."""
    bad = 0
    for t in manifest["tasks"]:
        for label, overlays in (("baseline", ["input"]), ("solution", ["input", "solution"])):
            tmp = Path(__import__("tempfile").mkdtemp(prefix="harness-eval-"))
            try:
                for o in overlays:
                    _overlay(FIXTURES / t["id"] / o, tmp)
                passed = _accept(t["id"], tmp)["passed"]
            finally:
                shutil.rmtree(tmp, ignore_errors=True)
            want = label == "solution"
            if passed != want:
                bad += 1
                print(f"FAIL {t['id']}: {label} acceptance passed={passed}, expected {want}")
    print("fixtures witness: ok" if not bad else f"fixtures witness: {bad} failure(s)")
    return 1 if bad else 0


def pipeline_self_test(manifest: dict) -> int:
    """Run every mock arm end to end and check what each one must be judged as."""
    import tempfile
    out = Path(tempfile.mkdtemp(prefix="harness-eval-selftest-"))
    failures: list[str] = []
    try:
        arms = [a for a in manifest["arms"] if a["kind"] == "mock"]
        runs = [run_one(t, a, 0, out, "selftest", "n/a", False) for t in manifest["tasks"] for a in arms]
        by = {(r["task"], r["arm"]): r for r in runs}

        def want(task: str, arm: str, outcome: str) -> None:
            got = by[(task, arm)]["outcome"]
            if got != outcome:
                failures.append(f"{task}/{arm}: outcome {got!r}, expected {outcome!r}")

        for t in manifest["tasks"]:
            want(t["id"], "stub-noop", "rejected")        # exit 0 and a success claim, no deliverable
            want(t["id"], "stub-replayer", "invalid")     # duplicated usage is not a success
            capable = all(c in next(a for a in arms if a["id"] == "stub-shell-free")["capabilities"] for c in t["requires"])
            want(t["id"], "stub-solver", "accepted")
            want(t["id"], "stub-shell-free", "accepted" if capable else "inapplicable")
        noop = by[("text-output", "stub-noop")]
        if not (noop["exit_code"] == 0 and noop["claimed_success"] and validate.naive_exit_code_judge(noop)):
            failures.append("stub-noop no longer fools the naive judge")
        replay = json.loads((out / "runs" / "interrupted-effects__stub-replayer__0" / "acceptance.json").read_text())
        if replay["passed"]:
            failures.append("replayed side effects passed acceptance")
        report = build_report(runs, manifest, len(runs))
        failures += validate.validate_report(report, manifest)
        if report["recommendation"]["switch_default"] is not False:
            failures.append("mock evidence produced a default switch")
    finally:
        shutil.rmtree(out, ignore_errors=True)
    for f in failures:
        print("FAIL:", f)
    print("run.py self-test: ok" if not failures else f"run.py self-test: {len(failures)} failure(s)")
    return 1 if failures else 0


def main(argv: list[str]) -> int:
    if argv and argv[0] == "worker":
        return worker(argv[1], Path(argv[2]), Path(argv[3]), argv[4])
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path)
    ap.add_argument("--arms", help="comma-separated arm ids (default: all mock arms)")
    ap.add_argument("--tasks", help="comma-separated task ids (default: all)")
    ap.add_argument("--trials", type=int)
    ap.add_argument("--model", default="unset")
    ap.add_argument("--live", action="store_true")
    ap.add_argument("--spend-cap-usd", type=float)
    ap.add_argument("--fixtures-witness", action="store_true")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args(argv)
    manifest = validate.load_toml(HERE / "manifest.toml")
    problems = validate.check_corpus(manifest)
    if problems:
        print("\n".join(problems), file=sys.stderr)
        return 1
    if args.fixtures_witness:
        return fixtures_witness(manifest)
    if args.self_test:
        return pipeline_self_test(manifest)
    if not args.out:
        ap.error("--out is required")
    if args.live and args.spend_cap_usd is None:
        ap.error("--live requires --spend-cap-usd")
    wanted = args.arms.split(",") if args.arms else [a["id"] for a in manifest["arms"] if a["kind"] == "mock"]
    arms = [a for a in manifest["arms"] if a["id"] in wanted]
    if len(arms) != len(wanted):
        ap.error(f"unknown arm in {wanted}")
    if any(a["kind"] == "live" for a in arms) and not args.live:
        ap.error("live arms need --live")
    tasks = [t for t in manifest["tasks"] if not args.tasks or t["id"] in args.tasks.split(",")]
    trials = args.trials if args.trials is not None else manifest["policy"]["trials"]
    revision, runs, spent = _git_revision(), [], 0.0
    planned = len(tasks) * len(arms) * trials
    for trial in range(trials):
        for task in tasks:
            for arm in ordered_arms(arms, trial):
                if args.live and args.spend_cap_usd is not None and spent >= args.spend_cap_usd and arm["kind"] == "live":
                    rec = run_one(task, {**arm, "command": []}, trial, args.out, revision, args.model, False)
                    rec["reason"] = "spend cap reached before this run"
                else:
                    rec = run_one(task, arm, trial, args.out, revision, args.model, args.live)
                spent += sum((r.get("cost_usd") or 0.0) for r in rec["usage"]["requests"])
                runs.append(rec)
    report = build_report(runs, manifest, planned)
    problems = validate.validate_report(report, manifest)
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "report.json").write_text(json.dumps(report, indent=2))
    for p in problems:
        print("report problem:", p, file=sys.stderr)
    for ev, rows in report["summary"].items():
        for arm_id, row in rows.items():
            print(f"[{ev}] {arm_id}: {row}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
