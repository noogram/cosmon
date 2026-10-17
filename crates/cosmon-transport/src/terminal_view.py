# SPDX-License-Identifier: AGPL-3.0-only
"""Output-only tmux control client; never announces a terminal size.

Control clients without refresh-client -C are excluded from tmux sizing even
when they are the last client. Render guarded visible-pane snapshots locally;
no outer keyboard byte or resize is forwarded to tmux.
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
        print("read-only terminal view requires a PTY", file=sys.stderr)
        return 1
    read_command = sys.argv[1].encode() + b"\n"
    process = subprocess.Popen(sys.argv[2:], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    saved = termios.tcgetattr(0)
    tty.setraw(0)
    def close_view(_signum, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGHUP, close_view)
    signal.signal(signal.SIGTERM, close_view)
    failed = False
    buffer = b""
    tag = None
    frame = []
    next_read = 0
    last_response = time.monotonic()
    try:
        while process.poll() is None:
            now = time.monotonic()
            if now >= next_read:
                # Fixed, typed read command supplied by the adapter. No user text.
                process.stdin.write(read_command)
                process.stdin.flush()
                next_read = now + 0.25
            ready, _, _ = select.select([process.stdout, 0], [], [], 0.1)
            if 0 in ready:
                data = os.read(0, 4096)
                if not data or any(key in data for key in (b"q", b"\x1b", b"\x03")):
                    break
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
                            screen = b"\x1b[H\x1b[2J" + b"\r\n".join(frame) + b"\r\n[read-only; q/Esc/Ctrl-C closes]"
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
