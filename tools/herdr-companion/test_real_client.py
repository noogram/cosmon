#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Explicit disposable Herdr + tmux PTY acceptance, invoked by the ignored Rust test.

Never targets operator panes/registry. Requires real herdr/tmux/Python executables.
Set COSMON_CLIENT_RECEIPTS to retain JSON receipts outside the temporary fixture.
"""

import argparse
import hashlib
import fcntl
import struct
import termios
import json
import os
import pty
import select
import signal
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--cs", required=True)
    parser.add_argument("--tmux-socket", required=True)
    args = parser.parse_args()
    args.repo = args.repo.resolve()
    receipt = {"checks": [], "commands": [], "ui": "not assessed; real daemon socket and PTY only"}
    output_dir = Path(os.environ.get("COSMON_CLIENT_RECEIPTS") or tempfile.mkdtemp(prefix="cosmon-client-receipts-"))
    output_dir.mkdir(parents=True, exist_ok=True)
    state = args.repo / ".cosmon" / "state"
    plugin = Path(__file__).resolve().parent
    cs = [args.cs, "--config", str(state)]
    server = None

    def run(argv, env=None):
        result = subprocess.run(argv, cwd=args.repo, env=env, capture_output=True, text=True, timeout=25)
        receipt["commands"].append({"argv": [str(x) for x in argv], "exit": result.returncode,
                                     "stdout": result.stdout, "stderr": result.stderr})
        assert result.returncode == 0, result.stderr + result.stdout
        return result.stdout

    def check(name, condition):
        assert condition, name
        receipt["checks"].append(name)

    def wait_for(name, predicate, seconds=10):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            value = predicate()
            if value:
                return value
            time.sleep(0.1)
        raise AssertionError("bounded acceptance wait expired: " + name)

    with tempfile.TemporaryDirectory(prefix="cv-", dir="/tmp") as temp:
        owned = Path(temp).resolve()
        env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
        env.update(XDG_CONFIG_HOME=str(owned / "config"), XDG_STATE_HOME=str(owned / "state"),
                   HERDR_CONFIG_PATH=str(owned / "config" / "herdr" / "config.toml"),
                   COSMON_CLIENT_CS=args.cs)
        Path(env["HERDR_CONFIG_PATH"]).parent.mkdir(parents=True)
        Path(env["HERDR_CONFIG_PATH"]).write_text("")
        # Source inspection confirms both registry and runtime directories honor XDG.
        # Live config-dir verifies the executable agrees BEFORE link mutates anything.
        config_dir = run(["herdr", "plugin", "config-dir", "cosmon.client"], env).strip()
        check("plugin config path isolated before registration", Path(config_dir).is_relative_to(owned))
        try:
            run(["herdr", "plugin", "link", str(plugin)], env)
            registry = owned / "config" / "herdr" / "plugins.json"
            check("only isolated registry registered", registry.is_file() and str(plugin) in registry.read_text())
            def decode(raw):
                value = json.loads(raw)
                return value["result"] if isinstance(value, dict) and "result" in value else value
            receipt["registration"] = decode(run(["herdr", "plugin", "list", "--json"], env))
            log = open(output_dir / "herdr-server.log", "w")
            server = subprocess.Popen(["herdr", "server"], env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            def ready():
                p = subprocess.run(["herdr", "status", "server", "--json"], env=env, capture_output=True, text=True, timeout=4)
                try:
                    status = json.loads(p.stdout)
                    return status if status.get("running") else None
                except ValueError:
                    return None
            status = wait_for("owned server", ready)
            address = status["socket"]
            check("real socket is isolated", Path(address).is_relative_to(owned))
            env["HERDR_SOCKET_PATH"] = address
            receipt["server"] = status
            receipt["actions"] = decode(run(["herdr", "plugin", "action", "list", "--plugin", "cosmon.client"], env))

            def api(method, params):
                with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
                    client.settimeout(10)
                    client.connect(address)
                    client.sendall(json.dumps({"id": "acceptance", "method": method, "params": params}).encode() + b"\n")
                    with client.makefile("rb") as stream:
                        value = json.loads(stream.readline(4 * 1024 * 1024))
                receipt["commands"].append({"method": method, "params": params, "response": value})
                assert "result" in value and "error" not in value, value
                return value["result"]

            workspace = decode(run(["herdr", "workspace", "create", "--cwd", str(args.repo), "--label", "Cosmon owned acceptance", "--focus"], env))
            wid = workspace["workspace"]["workspace_id"]
            before = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in state.rglob("*") if p.is_file()}
            initial_terminal = json.loads(run(cs + ["--json", "terminal", "task-20990101-a001"], env))
            pane_target = "%" + str(initial_terminal["identity"]["pane_id"])
            before_environment = run(["tmux", "-L", args.tmux_socket, "show-environment", "-t", "=fixture"])
            before_size = run(["tmux", "-L", args.tmux_socket, "display-message", "-p", "-t", pane_target, "#{pane_width}x#{pane_height}"]).strip()
            worker_dimensions = tuple(map(int, before_size.split("x")))
            check("worker size contains positive dimensions", len(worker_dimensions) == 2 and all(n > 0 for n in worker_dimensions))
            env.update(HERDR_WORKSPACE_ID=wid,
                       HERDR_PLUGIN_CONTEXT_JSON=json.dumps({"workspace_id": wid, "workspace_cwd": str(args.repo)}),
                       HERDR_BIN_PATH=shutil.which("herdr"))
            # Invoke the actual registered action. Its launched pane receives context.
            run(["herdr", "plugin", "action", "invoke", "cosmon.client.open"], env)
            def panes():
                return decode(run(["herdr", "pane", "list", "--workspace", wid], env))["panes"]
            def selector_pane():
                return next((p for p in panes() if p.get("label") == "Cosmon selector"), None)
            selector = wait_for("registered selector PTY", selector_pane)
            sid = selector["pane_id"]
            def read(pane):
                value = api("pane.read", {"pane_id": pane, "source": "recent", "lines": 200, "strip_ansi": True})
                return value["read"]["text"]
            wait_for("selector list rendered", lambda: "Molecule number" in read(sid))
            def enter(text):
                api("pane.send_text", {"pane_id": sid, "text": text})
                api("pane.send_keys", {"pane_id": sid, "keys": ["Enter"]})
            enter("1")
            wait_for("selected molecule details rendered", lambda: "Action:" in read(sid))
            before_terminal_panes = {p["pane_id"] for p in panes()}
            enter("t")
            def clients():
                return run(["tmux", "-L", args.tmux_socket, "list-clients", "-F", "#{client_readonly}\t#{client_flags}\t#{client_width}x#{client_height}\t#{client_session}"])
            attached = wait_for("readonly client", lambda: clients().strip())
            receipt["viewer_sizes"] = [line.split("\t")[2] for line in attached.splitlines()]
            check("viewer is an unsized control client", all("control-mode" in line for line in attached.splitlines()))
            check("readonly and ignore-size flags with exact session", all("read-only" in line and "ignore-size" in line and line.endswith("fixture") for line in attached.splitlines()))
            worker_view = next(p for p in panes() if p["pane_id"] not in before_terminal_panes)
            tid = worker_view["pane_id"]
            check("outer viewer rows differ from worker", worker_view["scroll"]["viewport_rows"] != worker_dimensions[1])
            check("correct worker terminal content", bool(wait_for("worker frame", lambda: "WORKER-READONLY-WITNESS" in read(tid))))
            api("pane.send_text", {"pane_id": tid, "text": "INPUT-MUST-NOT-REACH-WORKER"})
            api("pane.send_keys", {"pane_id": tid, "keys": ["Enter"]})
            time.sleep(0.2)
            check("readonly client keys never reached dummy worker", not (args.repo / "worker-input").exists())
            after_size = run(["tmux", "-L", args.tmux_socket, "display-message", "-p", "-t", pane_target, "#{pane_width}x#{pane_height}"]).strip()
            check("worker pane size unchanged", before_size == after_size)
            check("session environment unchanged", before_environment == run(["tmux", "-L", args.tmux_socket, "show-environment", "-t", "=fixture"]))
            controller_pid, controller_fd = pty.fork()
            if controller_pid == 0:
                fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 49, 132, 0, 0))
                controller_env = {k: v for k, v in env.items() if k not in ("TMUX", "TMUX_PANE")}
                controller_env["TERM"] = "xterm-256color"
                os.execvpe("tmux", ["tmux", "-S", initial_terminal["identity"]["socket_path"], "attach-session", "-E", "-t", "$" + str(initial_terminal["identity"]["session_id"])], controller_env)
            def dimensions():
                return run(["tmux", "-L", args.tmux_socket, "display-message", "-p", "-t", pane_target, "#{pane_width}x#{pane_height}"]).strip()
            try:
                ordinary_size = wait_for("ordinary controller resizes real worker", lambda: (size if size != before_size else None) if (size := dimensions()) else None)
            finally:
                os.kill(controller_pid, signal.SIGTERM)
                os.waitpid(controller_pid, 0)
                os.close(controller_fd)
            time.sleep(0.15)
            after_controller_detach = dimensions()
            check("ordinary controller detach does not fall back to viewer size", after_controller_detach == ordinary_size)
            receipt["controller_dimensions"] = {"ordinary_attached": ordinary_size, "ordinary_detached": after_controller_detach}
            api("pane.close", {"pane_id": tid})
            wait_for("view detached", lambda: not clients().strip())
            receipt["controller_dimensions"]["view_closed"] = dimensions()
            check("closing sole control view preserves changed worker size", dimensions() == ordinary_size)
            view = json.loads(run(cs + ["--json", "terminal", "task-20990101-a001"], env))
            check("closing view preserves worker incarnation", view["availability"] == "available")
            before_worktree_panes = {p["pane_id"] for p in panes()}
            enter("w")
            worktree_pane = wait_for("worktree tab", lambda: next((p for p in panes() if p["pane_id"] not in before_worktree_panes), None))
            check("existing worktree cwd is exact", worktree_pane["cwd"] == str(args.repo))
            before_artifact_panes = {p["pane_id"] for p in panes()}
            enter("a")
            wait_for("artifact selector", lambda: "Artifact number:" in read(sid))
            enter("1")
            def artifact_pane():
                return next((p for p in panes() if p["pane_id"] not in before_artifact_panes), None)
            artifact = wait_for("artifact PTY", artifact_pane)
            check("canonical artifact readable", bool(wait_for("artifact text", lambda: "owned artifact witness" in read(artifact["pane_id"]))))
            after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in state.rglob("*") if p.is_file()}
            receipt["state_diffs"] = {p: {"before": before.get(p), "after": after.get(p)} for p in before.keys() | after.keys() if before.get(p) != after.get(p)}
            audit_path = str(state / "instrumentation" / "authz.jsonl")
            check("molecule state and artifacts byte-identical across views", all(p == audit_path for p in receipt["state_diffs"]))
            audit = [json.loads(line) for line in Path(audit_path).read_text().splitlines()]
            check("only existing observe authorization audit appended", bool(audit) and all(row["verb"] == "observe" and row["decision"] == "allow" for row in audit))
            receipt["authorization_audit_rows"] = len(audit)
            check("worker input still unchanged", not (args.repo / "worker-input").exists())
            receipt["terminal_observation"] = view
            receipt["selector_tail"] = read(sid)
            receipt["panes"] = panes()
            receipt["state_hashes"] = before
            receipt["worker_size_before"] = before_size
            receipt["worker_size_after"] = after_size
            # Force a pane respawn AFTER kernel validation but BEFORE tmux's
            # if-shell queue executes. This targets the actual execution guard,
            # with a real PTY so a missing guard would successfully attach.
            before_guard_panes = {p["pane_id"] for p in panes()}
            run([sys.executable, str(plugin / "cosmon-herdr.py"), "--repo", str(args.repo), "--cs", args.cs,
                 "open", "task-20990101-a001", "--terminal", "--workspace", wid], env)
            frame_guard_pane = wait_for("second guarded view", lambda: next((p for p in panes() if p["pane_id"] not in before_guard_panes), None))
            wait_for("second control client attached", lambda: clients().strip())
            real_tmux = shutil.which("tmux")
            proxy_dir = owned / "proxy"
            proxy_dir.mkdir()
            marker = proxy_dir / "race-executed"
            proxy = proxy_dir / "tmux"
            proxy.write_text("#!" + sys.executable + "\n" +
                "import os, subprocess, sys, pathlib\n" +
                "if 'if-shell' in sys.argv:\n" +
                " subprocess.run([os.environ['REAL_TMUX'], '-S', os.environ['RACE_SOCKET'], 'respawn-pane', '-k', '-t', os.environ['RACE_PANE'], 'exec python3 worker.py'], check=True)\n" +
                " pathlib.Path(os.environ['RACE_MARKER']).write_text('respawned before guarded attach')\n" +
                "os.execv(os.environ['REAL_TMUX'], [os.environ['REAL_TMUX'], *sys.argv[1:]])\n")
            proxy.chmod(0o755)
            race_env = dict(env, PATH=str(proxy_dir) + os.pathsep + env['PATH'],
                            REAL_TMUX=real_tmux, RACE_SOCKET=view["identity"]["socket_path"],
                            RACE_PANE="%" + str(view["identity"]["pane_id"]), RACE_MARKER=str(marker),
                            TERM="xterm-256color")
            pid, descriptor = pty.fork()
            if pid == 0:
                os.chdir(args.repo)
                os.execvpe(args.cs, cs + ["terminal", "task-20990101-a001", "--attach", "--expect", view["expected"]], race_env)
            transcript = bytearray()
            deadline = time.monotonic() + 4
            exit_status = None
            try:
                while time.monotonic() < deadline:
                    if select.select([descriptor], [], [], 0.05)[0]:
                        try:
                            transcript.extend(os.read(descriptor, 65536))
                        except OSError:
                            pass
                    exited, status_code = os.waitpid(pid, os.WNOHANG)
                    if exited:
                        exit_status = os.waitstatus_to_exitcode(status_code)
                        break
            finally:
                if exit_status is None:
                    os.kill(pid, signal.SIGTERM)
                    os.waitpid(pid, 0)
                os.close(descriptor)
            if marker.is_file() and exit_status not in (None, 0):
                wait_for("existing frame view refuses respawn", lambda: not clients().strip())
            receipt["guard_race"] = {"executed": marker.is_file(), "exit": exit_status,
                                      "output": transcript.decode(errors="replace"), "clients": clients()}
            check("execution guard refused a real intervening pane respawn", marker.is_file() and exit_status not in (None, 0) and "terminal-incarnation-refused" in receipt["guard_race"]["output"] and not receipt["guard_race"]["clients"].strip())
            receipt["frame_guard_closed"] = bool(wait_for("frame guard closes view", lambda: not any(p["pane_id"] == frame_guard_pane["pane_id"] for p in panes())))
            check("ongoing frame guard also closed the stale view", receipt["frame_guard_closed"])
            api("workspace.close", {"workspace_id": wid, "close_group": False})
            run(["herdr", "plugin", "unlink", "cosmon.client"], env)
            receipt["outcome"] = "passed"
        finally:
            if server:
                subprocess.run(["herdr", "server", "stop"], env=env, capture_output=True, timeout=10)
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.terminate()
                    server.wait(timeout=5)
            (output_dir / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print("Real client receipt: " + str(output_dir / "receipt.json"))


if __name__ == "__main__":
    main()
