#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Real generic terminal input/resize/refusal acceptance using only an owned fixture."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import termios
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--cs", required=True)
    args = parser.parse_args()
    cs = [args.cs, "--config", str(args.repo / ".cosmon/state"), "terminal", "task-20990101-a001"]
    def run(argv):
        return subprocess.check_output(argv, cwd=args.repo, timeout=5)
    initial = json.loads(run(cs))
    explicit = json.loads(run(cs + ["--mode", "interactive"]))
    assert initial["kind"] == "cosmon.terminal" and initial["capability"] == "read_only_ignore_size"
    assert explicit["kind"] == "cosmon.terminal.interactive" and explicit["capability"] == "interactive_resize"
    assert explicit["expected"] == initial["expected"]
    identity = initial["identity"]
    tmux = ["tmux", "-S", identity["socket_path"]]
    pane = "%" + str(identity["pane_id"])
    session = "$" + str(identity["session_id"])
    environment = run(tmux + ["show-environment", "-t", session])
    def dimensions():
        return run(tmux + ["display-message", "-p", "-t", pane, "#{pane_width}x#{pane_height}"]).strip()
    checks = []
    before = dimensions()
    input_file = args.repo / "worker-input"
    def view(interactive):
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(args.repo)
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 37, 111, 0, 0))
            os.execv(args.cs, cs + ["--attach", "--expect", initial["expected"]] + (["--mode", "interactive"] if interactive else []))
        return pid, fd
    def wait(name, predicate, fd=None):
        deadline = time.monotonic() + 6
        while time.monotonic() < deadline:
            if fd is not None and select.select([fd], [], [], .05)[0]:
                try:
                    os.read(fd, 65536)
                except OSError:
                    pass
            if predicate():
                checks.append(name)
                return
            time.sleep(.05)
        raise AssertionError(name)
    def attached():
        return bool(run(tmux + ["list-clients"]).strip())
    def stop(pid, fd, key):
        os.write(fd, key)
        wait("client detached", lambda: not attached(), fd)
        os.waitpid(pid, 0)
        os.close(fd)
    pid, fd = view(False)
    try:
        wait("default readonly attached", attached, fd)
        os.write(fd, b"MUST-NOT-FORWARD\r")
        time.sleep(.3)
        assert not input_file.exists()
        assert before == dimensions()
    finally:
        stop(pid, fd, b"q")
    checks.append("readonly input and size unchanged")
    pid, fd = view(True)
    try:
        wait("interactive dimensions contribute", lambda: dimensions() == b"111x37", fd)
        literal = b"human input ' ; $(touch SENTINEL)\r"
        os.write(fd, literal)
        wait("literal input reached exact dummy", lambda: input_file.exists() and input_file.read_bytes() == literal.replace(b"\r", b"\n"), fd)
        assert not (args.repo / "SENTINEL").exists()
        assert run(tmux + ["show-environment", "-t", session]) == environment
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 39, 113, 0, 0))
        wait("interactive resize updates worker", lambda: dimensions() == b"113x39", fd)
    finally:
        stop(pid, fd, b"\x1d")
    assert json.loads(run(cs))["expected"] == initial["expected"]
    checks.append("interactive close preserved exact worker and environment")
    pid, fd = view(True)
    try:
        wait("second interactive attached", attached, fd)
        prior = input_file.read_bytes()
        run(tmux + ["respawn-pane", "-k", "-t", pane, "exec python3 worker.py"])
        os.write(fd, b"STALE-MUST-NOT-REACH\r")
        wait("ongoing guard detached stale interactive view", lambda: not attached(), fd)
        _, status = os.waitpid(pid, 0)
        assert os.waitstatus_to_exitcode(status) != 0
        assert input_file.read_bytes() == prior
        stale = subprocess.run(cs + ["--mode", "interactive", "--attach", "--expect", initial["expected"]], cwd=args.repo, capture_output=True, timeout=5)
        assert stale.returncode != 0
        checks.append("stale reconnect refused without sending input")
    finally:
        os.close(fd)
    print(json.dumps({"checks": checks, "outcome": "passed"}))


if __name__ == "__main__":
    main()
