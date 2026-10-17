#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Own one public cs attachment PTY; never forward stdin or client resize."""
import errno
import codecs
import fcntl
import json
import os
import pty
import select
import signal
import struct
import subprocess
import sys
import termios


def main():
    """Relay bounded raw chunks; pipe closure terminates only the owned reader group."""
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 0, 0))
    child = subprocess.Popen(sys.argv[1:], stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
    os.close(slave)
    stopping = False
    decoder = codecs.getincrementaldecoder("utf8")(errors="replace")

    def stop(_signum=None, _frame=None):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        while not stopping:
            ready, _, _ = select.select([master, sys.stdin], [], [], .1)
            if sys.stdin in ready:
                # stdin is a lifetime pipe, not a worker input channel.
                if not os.read(sys.stdin.fileno(), 4096):
                    break
            if master in ready:
                try:
                    data = os.read(master, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not data:
                    break
                print(json.dumps({'data': decoder.decode(data)}), flush=True)
            if child.poll() is not None and not ready:
                break
    except BrokenPipeError:
        pass
    finally:
        # The session contains only cs and its output renderer/control client.
        # Never signals tmux's server or the worker (independent sessions).
        try:
            os.killpg(child.pid, signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            # macOS can return EPERM for an already-exited process group.
            if child.poll() is None:
                child.terminate()
        try:
            child.wait(timeout=.4)
        except subprocess.TimeoutExpired:
            pass
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            if child.poll() is None:
                child.kill()
        child.wait(timeout=1)
        os.close(master)
    return child.returncode if child.returncode and child.returncode > 0 else 0


if __name__ == '__main__':
    sys.exit(main())
