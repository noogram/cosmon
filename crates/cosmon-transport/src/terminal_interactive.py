# SPDX-License-Identifier: AGPL-3.0-only
"""Explicit human input, guarded numeric tmux target, locally rendered frames.

Ctrl-] detaches. Every write/frame/resize is queued behind the incarnation
predicate. Size contributes to tmux sizing; -E preserves session environment.
"""
import os
import select
import signal
import subprocess
import sys
import termios
import time
import tty


def main():
    if not os.isatty(0) or not os.isatty(1):
        print("interactive terminal requires a PTY", file=sys.stderr)
        return 1
    session, pane, guard = sys.argv[1:4]
    process = subprocess.Popen(sys.argv[4:], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    saved = termios.tcgetattr(0)
    tty.setraw(0)
    def close_view(_signum, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGHUP, close_view)
    signal.signal(signal.SIGTERM, close_view)
    failed, buffer, tag, frame = False, b"", None, []
    next_read, last_response, dimensions = 0, time.monotonic(), None
    def guarded(command):
        process.stdin.write((f"if-shell -F -t ${session} '{guard}' '{command}' 'terminal-incarnation-refused'\n").encode())
        process.stdin.flush()
    try:
        while process.poll() is None:
            now = time.monotonic()
            size = os.get_terminal_size(0)
            current_size = (size.columns, size.lines)
            if current_size != dimensions and all(n > 0 for n in current_size):
                guarded(f"refresh-client -C {size.columns},{size.lines}")
                dimensions = current_size
            if now >= next_read:
                guarded(f"capture-pane -p -e -t %{pane}")
                next_read = now + 0.25
            ready, _, _ = select.select([process.stdout, 0], [], [], 0.1)
            if 0 in ready:
                data = os.read(0, 4096)
                if not data or b"\x1d" in data:
                    break
                # No shell quoting and no user text in tmux's parser.
                guarded(f"send-keys -H -t %{pane} " + " ".join(f"{byte:02x}" for byte in data))
            if process.stdout in ready:
                data = os.read(process.stdout.fileno(), 65536)
                if not data:
                    break
                buffer += data
                while b"\n" in buffer:
                    line, buffer = buffer.split(b"\n", 1)
                    if tag is None:
                        if line.startswith(b"%begin "):
                            tag, frame = line.split(b" ", 1)[1], []
                    elif line == b"%end " + tag or line == b"%error " + tag:
                        last_response = time.monotonic()
                        if line.startswith(b"%error "):
                            failed = True
                            break
                        if frame:
                            screen = b"\x1b[H\x1b[2J" + b"\r\n".join(frame) + b"\r\n[interactive; Ctrl-] detaches]"
                            while screen:
                                screen = screen[os.write(1, screen):]
                        tag = None
                    else:
                        frame.append(line)
                if failed:
                    break
            if now - last_response > 3:
                failed = True
                break
    except (KeyboardInterrupt, BrokenPipeError):
        pass
    finally:
        try:
            termios.tcsetattr(0, termios.TCSANOW, saved)
        except (OSError, termios.error):
            pass
        if process.poll() is None:
            process.terminate()
        process.wait()
    if failed:
        print("terminal-incarnation-refused or terminal observation unavailable", file=sys.stderr)
        return 1
    return 0 if process.returncode in (0, -signal.SIGTERM) else 1


sys.exit(main())
