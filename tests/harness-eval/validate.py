#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Judge evaluation runs and reports for the in-process harness comparison.

A run record is accepted as a success only on evidence the worker does not
control: the independent acceptance script ran and passed, every required
artifact exists, and the gate (when the task has one) passed. A zero exit code
and a worker's own "done" claim are recorded but never sufficient.

Usage:
    python3 validate.py --self-test            # evaluator self-test (offline)
    python3 validate.py --report report.json   # judge a finished report
    python3 validate.py --check-corpus         # recompute fixture digests

Run record (JSON object), the contract `run.py` writes and this module judges:

    task, arm, trial, evidence ("live" | "mock"), outcome, exit_code,
    claimed_success, revision, config_digest, input_digest, model_requested,
    model_observed (or null), allowed_tools, retries, interventions,
    wall_seconds,
    acceptance {ran, passed, artifacts {name: bool}, gate (bool | null)},
    usage {requests: [{request_id, input_tokens, output_tokens, cost_usd}]},
    cost {complete, total_usd}

`outcome` is one of: accepted, rejected, inapplicable, invalid. A report holds
every run, grouped per evidence class, and a routing recommendation.
"""
from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
OUTCOMES = ("accepted", "rejected", "inapplicable", "invalid")
EVIDENCE = ("live", "mock")


def load_toml(path: Path) -> dict:
    """Parse a TOML file with the standard library, or `tomli` on older Pythons."""
    try:
        import tomllib as toml
    except ModuleNotFoundError:  # Python < 3.11
        try:
            import tomli as toml
        except ModuleNotFoundError:
            sys.exit("validate.py needs Python 3.11+ (tomllib) or the tomli package")
    with open(path, "rb") as fh:
        return toml.load(fh)


def input_digest(task_id: str, fixtures: Path = HERE / "fixtures") -> str:
    """sha256 over sorted (path, file sha256) of task.md, input/ and peer_message.txt."""
    root = fixtures / task_id
    files = [root / "task.md"]
    peer = root / "peer_message.txt"
    if peer.is_file():
        files.append(peer)
    files += sorted(p for p in (root / "input").rglob("*") if p.is_file())
    h = hashlib.sha256()
    for p in files:
        h.update(str(p.relative_to(root)).encode() + b"\0")
        h.update(hashlib.sha256(p.read_bytes()).hexdigest().encode() + b"\n")
    return h.hexdigest()


def usage_problems(record: dict) -> list[str]:
    """Usage must not repeat a request, and complete cost needs a price on every request."""
    out: list[str] = []
    reqs = (record.get("usage") or {}).get("requests") or []
    seen: set[str] = set()
    for r in reqs:
        rid = r.get("request_id")
        if rid in seen:
            out.append(f"usage: request {rid!r} counted more than once")
        seen.add(rid)
    cost = record.get("cost") or {}
    if cost.get("complete"):
        unpriced = [r.get("request_id") for r in reqs if r.get("cost_usd") is None]
        if not reqs:
            out.append("cost: marked complete with no usage records")
        if unpriced:
            out.append(f"cost: marked complete but {len(unpriced)} request(s) have unknown cost")
        elif reqs and cost.get("total_usd") is None:
            out.append("cost: marked complete without a total")
        elif reqs:
            total = round(sum(r["cost_usd"] for r in reqs), 9)
            if abs(total - (cost.get("total_usd") or 0)) > 1e-9:
                out.append(f"cost: total {cost.get('total_usd')} differs from the sum {total} of its requests")
    elif cost.get("total_usd") is not None:
        out.append("cost: a total is stated although cost is not marked complete")
    return out


def validate_run(record: dict, task: dict, arm: dict | None = None) -> list[str]:
    """Return the reasons `record` cannot stand as reported; empty means consistent.

    A record that reports `outcome == "accepted"` must satisfy every success rule.
    A record that reports `rejected` or `inapplicable` is checked for the usage/cost
    rules only, so a rejected run keeps its (honest) numbers. An `invalid` record is
    accepted as a retained finding when it states its reason.
    """
    outcome = record.get("outcome")
    if outcome == "invalid":
        # An invalid run is a finding the runner already made; it must say why.
        if record.get("problems") or record.get("reason"):
            return []
        return ["marked invalid without a stated reason"]
    out = usage_problems(record)
    if outcome not in OUTCOMES:
        out.append(f"outcome {outcome!r} is not one of {OUTCOMES}")
    if record.get("evidence") not in EVIDENCE:
        out.append(f"evidence {record.get('evidence')!r} is not one of {EVIDENCE}")
    if arm is not None and outcome != "inapplicable":
        missing = [c for c in task.get("requires", []) if c not in arm.get("capabilities", [])]
        if missing:
            out.append(f"arm lacks {missing} required by the task: outcome must be inapplicable")
    if outcome == "inapplicable" and arm is not None:
        if all(c in arm.get("capabilities", []) for c in task.get("requires", [])):
            out.append("marked inapplicable although the arm has every required capability")
    if outcome != "accepted":
        return out
    acc = record.get("acceptance") or {}
    if not acc.get("ran"):
        out.append("success claimed but the acceptance script did not run")
    if acc.get("ran") and not acc.get("passed"):
        out.append("success claimed but the acceptance script failed")
    arts = acc.get("artifacts") or {}
    for name in task.get("required_artifacts", []):
        if not arts.get(name):
            out.append(f"success claimed but required artifact {name!r} is missing")
    if task.get("has_gate") and acc.get("gate") is not True:
        out.append("success claimed but the gate did not pass")
    if record.get("input_digest") != task.get("input_digest"):
        out.append("input digest differs from the frozen corpus")
    return out


def naive_exit_code_judge(record: dict) -> bool:
    """The evaluator this module replaces: success = zero exit code."""
    return record.get("exit_code") == 0


def validate_report(report: dict, manifest: dict) -> list[str]:
    """Check a finished report against the manifest's policy and each run's own rules."""
    out: list[str] = []
    tasks = {t["id"]: t for t in manifest["tasks"]}
    arms = {a["id"]: a for a in manifest["arms"]}
    runs = report.get("runs") or []
    for i, r in enumerate(runs):
        t, a = tasks.get(r.get("task")), arms.get(r.get("arm"))
        if t is None or a is None:
            out.append(f"run {i}: unknown task or arm {r.get('task')!r}/{r.get('arm')!r}")
            continue
        out += [f"run {i} ({r['task']}/{r['arm']}#{r.get('trial')}): {p}" for p in validate_run(r, t, a)]
    # Every planned run is retained, success or not.
    declared = report.get("planned_runs")
    if declared != len(runs):
        out.append(f"report keeps {len(runs)} runs but planned {declared}: every outcome must be retained")
    # Live and mocked evidence are separate classes with their own tallies.
    summary = report.get("summary") or {}
    for ev in {r.get("evidence") for r in runs}:
        if ev not in summary:
            out.append(f"summary has no section for evidence class {ev!r}")
    for ev, arm_rows in summary.items():
        for arm_id, row in arm_rows.items():
            mine = [r for r in runs if r.get("evidence") == ev and r.get("arm") == arm_id]
            if any(r.get("evidence") != ev for r in mine):
                out.append(f"summary[{ev}][{arm_id}] mixes evidence classes")
            for key in ("accepted", "rejected", "inapplicable", "invalid", "interventions", "cost_complete_runs", "runs"):
                if key not in row:
                    out.append(f"summary[{ev}][{arm_id}] lacks {key!r}")
            if row.get("runs") != len(mine):
                out.append(f"summary[{ev}][{arm_id}] counts {row.get('runs')} runs, report holds {len(mine)}")
            if row.get("accepted") != sum(1 for r in mine if r.get("outcome") == "accepted"):
                out.append(f"summary[{ev}][{arm_id}] accepted count disagrees with the runs")
            if row.get("cost_complete_runs") != sum(1 for r in mine if (r.get("cost") or {}).get("complete")):
                out.append(f"summary[{ev}][{arm_id}] cost completeness disagrees with the runs")
            if row.get("interventions") != sum(r.get("interventions", 0) for r in mine):
                out.append(f"summary[{ev}][{arm_id}] interventions disagree with the runs")
    rec = report.get("recommendation") or {}
    if "uncertainty" not in rec or not rec.get("uncertainty"):
        out.append("recommendation states no uncertainty")
    if rec.get("switch_default") is not False and not any(r.get("evidence") == "live" for r in runs):
        out.append("a default switch is recommended without any live evidence")
    if rec.get("switch_default") not in (True, False):
        out.append("recommendation.switch_default must be a boolean")
    if rec.get("switch_default") is True and not rec.get("operator_threshold_met"):
        out.append("a default switch is recommended without the operator's adoption threshold being met")
    return out


def check_corpus(manifest: dict) -> list[str]:
    """Each task's pinned digest must equal the recomputed one; the pin is not a placeholder."""
    out = []
    for t in manifest["tasks"]:
        got = input_digest(t["id"])
        if t.get("input_digest") != got:
            out.append(f"task {t['id']}: pinned digest {t.get('input_digest')!r} != recomputed {got}")
    return out


def _good_run(task: dict) -> dict:
    return {
        "task": task["id"], "arm": "stub-solver", "trial": 0, "evidence": "mock", "outcome": "accepted",
        "exit_code": 0, "claimed_success": True, "input_digest": task["input_digest"],
        "acceptance": {"ran": True, "passed": True, "gate": True if task["has_gate"] else None,
                       "artifacts": {n: True for n in task["required_artifacts"]}},
        "usage": {"requests": [{"request_id": "r1", "input_tokens": 10, "output_tokens": 5, "cost_usd": 0.001}]},
        "cost": {"complete": True, "total_usd": 0.001}, "interventions": 0,
    }


def self_test() -> int:
    """Feed the evaluator runs that look complete and are not; each must be rejected.

    The same runs pass `naive_exit_code_judge`, which is the defect being guarded.
    """
    manifest = load_toml(HERE / "manifest.toml")
    failures: list[str] = []

    def expect(name: str, record: dict, task: dict, *, reject: bool, fragment: str = "", arm=None):
        problems = validate_run(record, task, arm)
        if reject and not any(fragment in p for p in problems):
            failures.append(f"{name}: expected a problem containing {fragment!r}, got {problems}")
        if not reject and problems:
            failures.append(f"{name}: expected a clean record, got {problems}")

    plain = next(t for t in manifest["tasks"] if t["id"] == "text-output")
    gated = next(t for t in manifest["tasks"] if t["id"] == "bounded-edit")
    peer = next(t for t in manifest["tasks"] if t["id"] == "peer-evidence")
    shell_free = next(a for a in manifest["arms"] if a["id"] == "stub-shell-free")

    expect("positive control", _good_run(plain), plain, reject=False)
    expect("positive control with gate", _good_run(gated), gated, reject=False)

    missing = _good_run(plain)
    missing["acceptance"]["artifacts"]["answer.txt"] = False
    expect("missing artifact", missing, plain, reject=True, fragment="required artifact")

    gate_fail = _good_run(gated)
    gate_fail["acceptance"]["gate"] = False
    expect("gate failure", gate_fail, gated, reject=True, fragment="gate did not pass")

    not_run = _good_run(plain)
    not_run["acceptance"]["ran"] = False
    expect("acceptance never ran", not_run, plain, reject=True, fragment="did not run")

    failed = _good_run(plain)
    failed["acceptance"]["passed"] = False
    expect("acceptance failed", failed, plain, reject=True, fragment="acceptance script failed")

    dup = _good_run(plain)
    dup["usage"]["requests"].append(dict(dup["usage"]["requests"][0]))
    dup["cost"] = {"complete": False, "total_usd": None}
    expect("duplicated usage", dup, plain, reject=True, fragment="more than once")

    unknown = _good_run(plain)
    unknown["usage"]["requests"][0]["cost_usd"] = None
    expect("unknown cost claimed complete", unknown, plain, reject=True, fragment="unknown cost")

    bad_sum = _good_run(plain)
    bad_sum["cost"]["total_usd"] = 0.5
    expect("cost total off", bad_sum, plain, reject=True, fragment="differs from the sum")

    honest = _good_run(plain)
    honest["usage"]["requests"][0]["cost_usd"] = None
    honest["cost"] = {"complete": False, "total_usd": None}
    expect("honest unknown cost", honest, plain, reject=False)

    drift = _good_run(plain)
    drift["input_digest"] = "0" * 64
    expect("digest drift", drift, plain, reject=True, fragment="input digest")

    expect("capability mismatch must be inapplicable", _good_run(peer), peer, reject=True,
           fragment="must be inapplicable", arm=shell_free)
    inapp = _good_run(peer)
    inapp["outcome"] = "inapplicable"
    expect("inapplicable is honest", inapp, peer, reject=False, arm=shell_free)

    unexplained = _good_run(plain)
    unexplained["outcome"] = "invalid"
    expect("invalid without reason", unexplained, plain, reject=True, fragment="without a stated reason")
    explained = _good_run(plain)
    explained.update(outcome="invalid", problems=["usage: request 'r1' counted more than once"])
    expect("invalid with reason is retained", explained, plain, reject=False)

    # The defect the validator exists for: the naive judge accepts every bad run above.
    for name, r in (("missing artifact", missing), ("gate failure", gate_fail), ("failed acceptance", failed)):
        if not naive_exit_code_judge(r):
            failures.append(f"{name}: the fixture no longer exercises the naive judge's blind spot")

    # Report-level rules.
    runs = [_good_run(plain)]
    row = {"accepted": 1, "rejected": 0, "inapplicable": 0, "invalid": 0, "interventions": 0,
           "cost_complete_runs": 1, "runs": 1}
    report = {"planned_runs": 1, "runs": runs, "summary": {"mock": {"stub-solver": row}},
              "recommendation": {"switch_default": False, "uncertainty": "mock evidence only"}}
    if validate_report(report, manifest):
        failures.append(f"clean report rejected: {validate_report(report, manifest)}")
    dropped = json.loads(json.dumps(report))
    dropped["planned_runs"] = 2
    if not any("every outcome must be retained" in p for p in validate_report(dropped, manifest)):
        failures.append("a report that dropped a planned run was accepted")
    overclaim = json.loads(json.dumps(report))
    overclaim["recommendation"] = {"switch_default": True, "uncertainty": "none", "operator_threshold_met": True}
    if not any("without any live evidence" in p for p in validate_report(overclaim, manifest)):
        failures.append("a default switch on mocked evidence only was accepted")
    silent = json.loads(json.dumps(report))
    silent["recommendation"]["uncertainty"] = ""
    if not any("no uncertainty" in p for p in validate_report(silent, manifest)):
        failures.append("a recommendation without uncertainty was accepted")
    inflated = json.loads(json.dumps(report))
    inflated["summary"]["mock"]["stub-solver"]["accepted"] = 3
    if not any("accepted count" in p for p in validate_report(inflated, manifest)):
        failures.append("a summary that disagrees with its runs was accepted")

    for f in failures:
        print("FAIL:", f)
    if not failures:
        print("validate.py self-test: ok")
    return 1 if failures else 0


def main(argv: list[str]) -> int:
    manifest = load_toml(HERE / "manifest.toml")
    if "--self-test" in argv:
        return self_test()
    if "--check-corpus" in argv:
        problems = check_corpus(manifest)
        print("\n".join(problems) or "corpus digests match")
        return 1 if problems else 0
    if "--report" in argv:
        report = json.loads(Path(argv[argv.index("--report") + 1]).read_text())
        problems = validate_report(report, manifest)
        print("\n".join(problems) or "report is consistent")
        return 1 if problems else 0
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
