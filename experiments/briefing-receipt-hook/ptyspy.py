#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Record every byte a child process receives on its PTY, then forward it.

Why this exists: `tmux send-keys -H 0d` is observable at the tmux end, but the
question the briefing-submit experiment asks is whether the *application*
received the carriage return. Nothing on the tmux side answers that, and
`capture-pane` only shows what the application chose to draw.

So the tmux pane runs `ptyspy.py <log> -- <child argv...>` instead of the child
directly. The spy allocates a second PTY for the child, copies its own stdin
(the pane's PTY) into it, and appends one line per read chunk to <log>:

    <unix-timestamp-with-microseconds> <lowercase-hex of the chunk>

Output flows back unmodified, and window size is propagated on start and on
every SIGWINCH so the child's TUI lays out exactly as it would without the spy.
"""

import errno
import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios
import time
import tty


def main() -> int:
    if len(sys.argv) < 3:
        sys.stderr.write("usage: ptyspy.py <logfile> -- <argv...>\n")
        return 2
    log_path = sys.argv[1]
    argv = sys.argv[2:]
    if argv and argv[0] == "--":
        argv = argv[1:]
    if not argv:
        sys.stderr.write("ptyspy: no child argv\n")
        return 2

    pid, master = pty.fork()
    if pid == 0:
        try:
            os.execvp(argv[0], argv)
        except OSError as exc:  # pragma: no cover - child path
            sys.stderr.write(f"ptyspy: exec failed: {exc}\n")
            os._exit(127)

    def copy_winsize(*_args):
        try:
            packed = fcntl.ioctl(0, termios.TIOCGWINSZ, struct.pack("HHHH", 0, 0, 0, 0))
            fcntl.ioctl(master, termios.TIOCSWINSZ, packed)
        except OSError:
            pass

    copy_winsize()
    signal.signal(signal.SIGWINCH, copy_winsize)

    log = open(log_path, "ab", buffering=0)

    try:
        saved = termios.tcgetattr(0)
        tty.setraw(0)
    except (termios.error, ValueError):
        saved = None

    try:
        while True:
            try:
                readable, _, _ = select.select([0, master], [], [])
            except (InterruptedError, OSError) as exc:
                if isinstance(exc, OSError) and exc.errno != errno.EINTR:
                    break
                continue
            if master in readable:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    data = b""
                if not data:
                    break
                os.write(1, data)
            if 0 in readable:
                try:
                    data = os.read(0, 65536)
                except OSError:
                    data = b""
                if data:
                    log.write(f"{time.time():.6f} {data.hex()}\n".encode())
                    os.write(master, data)
    finally:
        if saved is not None:
            try:
                termios.tcsetattr(0, termios.TCSAFLUSH, saved)
            except termios.error:
                pass
        log.close()
        try:
            os.close(master)
        except OSError:
            pass
        try:
            os.waitpid(pid, 0)
        except OSError:
            pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
