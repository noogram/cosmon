# SPDX-License-Identifier: AGPL-3.0-only
"""Stop only detached observers of one owned scratch dispatch before sealing logs."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def owned_observer(command, binary, molecule, scratch):
    """Require the binary, subcommand, molecule and both scratch paths to agree."""
    prefix = f"{binary} realized-watch {molecule} --cwd {scratch}/.worktrees/{molecule} --config {scratch}/.cosmon/state"
    if command == prefix or command.startswith(prefix + " "):
        return True
    # This sibling has only a durable molecule-directory argument. Both
    # layouts belong to this same owned scratch repo, never a global store.
    return command in {
        f"{binary} briefing-backstop --state-dir {scratch}/.cosmon/state/{layout}molecules/{molecule}"
        for layout in ("", "fleets/default/")
    }


def processes():
    result = subprocess.run(["ps", "-axo", "pid=,command="], check=True,
                            capture_output=True, text=True)
    return {int(parts[0]): parts[1] for line in result.stdout.splitlines()
            if len(parts := line.strip().split(None, 1)) == 2}


def cleanup(binary, molecule, scratch):
    """Revalidate each exact observer before SIGTERM; refuse an unclean receipt."""
    owned = {pid: command for pid, command in processes().items()
             if owned_observer(command, binary, molecule, scratch)}
    for pid, command in owned.items():
        if processes().get(pid) == command:
            try:
                os.kill(pid, signal.SIGTERM)
                print(f"stopped owned observer pid={pid}")
            except ProcessLookupError:
                pass
    deadline = time.monotonic() + 10
    while True:
        remaining = {pid: command for pid, command in processes().items()
                     if owned_observer(command, binary, molecule, scratch)}
        if not remaining:
            print("owned observers remaining=0")
            return
        if time.monotonic() >= deadline:
            raise RuntimeError(f"owned observers did not stop: {list(remaining)}")
        time.sleep(0.05)


if __name__ == "__main__":
    cleanup(str(Path(sys.argv[1]).resolve()), sys.argv[2], sys.argv[3])
