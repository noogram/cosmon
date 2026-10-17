#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Opt-in cmux + witnessed dummy worker acceptance; never targets operator panes.

Run through owned_cmux_client_acceptance, which owns the temporary galaxy/tmux.
Requires a running cmux app with its existing socket policy; changes no config.
Receipts include private runtime observations and must stay outside the repo.
"""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def main():
    """Exercise real selector input, PTY output and teardown, preserving failures."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, type=Path)
    parser.add_argument("--cs", required=True)
    parser.add_argument("--tmux-socket", required=True)
    args = parser.parse_args()
    args.repo = args.repo.resolve()
    receipt_dir = Path(os.environ.get("COSMON_CLIENT_RECEIPTS") or tempfile.mkdtemp(prefix="cmux-receipts-"))
    receipt_dir.mkdir(parents=True, exist_ok=True)
    receipt = {"checks": [], "commands": [], "ui": "real cmux app terminal surfaces read through socket; no visual inspection"}
    owned_workspaces = []
    companion = Path(__file__).with_name("cosmon-cmux.py")
    base = [sys.executable, str(companion), "--repo", str(args.repo), "--project-id", "example-project", "--cs", args.cs]
    state = args.repo / ".cosmon/state"
    molecule_id = "task-20990101-a001"
    source_run = subprocess.run

    def run(argv, check=True):
        result = source_run(argv, cwd=args.repo, capture_output=True, text=True, timeout=25)
        receipt["commands"].append({"argv": list(map(str, argv)), "exit": result.returncode,
            "stdout": result.stdout, "stderr": result.stderr})
        if check:
            assert result.returncode == 0, result.stderr + result.stdout
        return result

    def rpc(method, params):
        value = json.loads(run(["cmux", "--json", "rpc", method, json.dumps(params)]).stdout)
        if method == "workspace.create":
            owned_workspaces.append(value["workspace_id"])
        return value

    def check(name, condition):
        assert condition, name
        receipt["checks"].append(name)

    def wait_for(name, predicate, seconds=12):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            value = predicate()
            if value:
                return value
            time.sleep(0.1)
        raise AssertionError("bounded acceptance wait expired: " + name)

    def read(view):
        return rpc("surface.read_text", {"workspace_id": view["workspace_id"], "surface_id": view["surface_id"]})["text"]

    def enter(view, text):
        # Test input goes ONLY to our freshly created selector/view, never worker.
        rpc("surface.send_text", {"workspace_id": view["workspace_id"], "surface_id": view["surface_id"], "text": text + "\r"})

    def client_open(*command):
        value = json.loads(run(base + list(command)).stdout)["cmux"]
        owned_workspaces.append(value["workspace_id"])
        return value

    def worker(argv):
        return run(["tmux", "-L", args.tmux_socket] + argv).stdout

    def hashes():
        return {str(p.relative_to(state)): hashlib.sha256(p.read_bytes()).hexdigest() for p in state.rglob("*") if p.is_file()}

    try:
        receipt["cmux_version"] = run(["cmux", "version"]).stdout.strip()
        receipt["cmux_capabilities"] = rpc("system.capabilities", {})
        check("fixture listed by independent consumer", any(row["id"] == molecule_id for row in json.loads(run(base + ["list"]).stdout)["molecules"]))
        before = hashes()
        initial = json.loads(run([args.cs, "--config", str(state), "--json", "terminal", molecule_id]).stdout)
        check("real fixture has public v1 available witness", initial["availability"] == "available" and initial["schema_version"] == 1)
        pane = "%" + str(initial["identity"]["pane_id"])
        size = worker(["display-message", "-p", "-t", pane, "#{pane_width}x#{pane_height}"])
        environment = worker(["show-environment", "-t", "=fixture"])
        selector = client_open("launch")
        wait_for("selector rendering", lambda: "Molecule number" in read(selector))
        enter(selector, "1")
        check("real numbered selection renders exact molecule", bool(wait_for("selected detail", lambda: molecule_id in read(selector) and "t read-only worker" in read(selector))))
        # Open via the selector itself and discover ONLY newly created ids.
        before_ids = {r["id"] for r in rpc("workspace.list", {"window_id": selector["window_id"]})["workspaces"]}
        enter(selector, "t")
        def new_view():
            rows = rpc("workspace.list", {"window_id": selector["window_id"]})["workspaces"]
            for row in rows:
                if row["id"] not in before_ids and row.get("current_directory") == str(args.repo):
                    surfaces = rpc("surface.list", {"workspace_id": row["id"]})["surfaces"]
                    if surfaces:
                        return {"workspace_id": row["id"], "surface_id": surfaces[0]["id"], "window_id": selector["window_id"]}
            return None
        view = wait_for("new worker view", new_view)
        owned_workspaces.append(view["workspace_id"])
        check("correct dummy worker frame in actual cmux surface", bool(wait_for("worker frame", lambda: "WORKER-READONLY-WITNESS" in read(view))))
        clients = worker(["list-clients", "-F", "#{client_flags}\t#{client_session}"])
        check("real client readonly ignore-size control-mode exact session", bool(clients.strip()) and all(all(flag in line for flag in ("read-only", "ignore-size", "control-mode")) and line.endswith("fixture") for line in clients.splitlines()))
        enter(view, "INPUT-MUST-NOT-REACH-WORKER")
        time.sleep(0.3)
        check("cmux view input never reaches dummy worker", not (args.repo / "worker-input").exists())
        check("worker dimensions unchanged", worker(["display-message", "-p", "-t", pane, "#{pane_width}x#{pane_height}"]) == size)
        check("worker session environment unchanged", worker(["show-environment", "-t", "=fixture"]) == environment)
        rpc("workspace.close", {"workspace_id": view["workspace_id"]})
        owned_workspaces.remove(view["workspace_id"])
        wait_for("client detached", lambda: not worker(["list-clients", "-F", "#{client_flags}"]).strip())
        after_close = json.loads(run([args.cs, "--config", str(state), "--json", "terminal", molecule_id]).stdout)
        check("closing cmux view preserves worker incarnation", after_close["availability"] == "available" and after_close["expected"] == initial["expected"])
        worktree = client_open("open", molecule_id, "--worktree")
        enter(worktree, "pwd")
        check("worktree shell renders exact existing cwd", bool(wait_for("worktree cwd", lambda: str(args.repo) in read(worktree))))
        artifact = client_open("open", molecule_id, "--artifact", "synthesis.md")
        check("canonical artifact rendered in real cmux view", bool(wait_for("artifact text", lambda: "owned artifact witness" in read(artifact))))
        # Force a stale token AFTER the companion probe but BEFORE its public
        # attachment executes. No private backend address is passed by the helper.
        spec = importlib.util.spec_from_file_location("cmux_client", companion)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        settings = argparse.Namespace(repo=args.repo, project_id="example-project", fleet="default", cs=args.cs,
            cmux="cmux", socket=None, command="open", id=molecule_id, terminal=True, worktree=False, artifact=None)
        module.configure(settings)
        real_open = module.open_view
        def race_open(settings, argv, cwd, title):
            worker(["respawn-pane", "-k", "-t", pane, "exec python3 worker.py"])
            result = real_open(settings, argv, cwd, title)
            owned_workspaces.append(result["cmux"]["workspace_id"])
            return result
        module.open_view = race_open
        stale = module.execute(settings)["cmux"]
        wait_for("stale refusal", lambda: "identity" in read(stale).lower() or "incarnation" in read(stale).lower())
        check("stale token refuses actual cmux attachment", not worker(["list-clients", "-F", "#{client_flags}"]).strip())
        after = hashes()
        diffs = {key: {"before": before.get(key), "after": after.get(key)} for key in before.keys() | after.keys() if before.get(key) != after.get(key)}
        receipt["state_diffs"] = diffs
        check("lifecycle process and artifact bytes unchanged", all(key == "instrumentation/authz.jsonl" for key in diffs))
        receipt.update(worker_size=size, state_hashes=before, terminal_observation=initial,
            selector_tail=read(selector), stale_refusal=read(stale), result="passed")
    except BaseException as error:
        receipt.update(result="failed", error=repr(error))
        raise
    finally:
        for workspace_id in reversed(owned_workspaces):
            result = run(["cmux", "--json", "rpc", "workspace.close", json.dumps({"workspace_id": workspace_id})], check=False)
            if result.returncode:
                receipt.setdefault("cleanup_failures", []).append(workspace_id)
        (receipt_dir / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    assert not receipt.get("cleanup_failures"), "owned workspace cleanup incomplete"
    print(json.dumps({"checks": len(receipt["checks"]), "receipt": str(receipt_dir / "receipt.json")}))


if __name__ == "__main__":
    main()
