#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Paste-to-CR delay x briefing-size matrix against a live Claude Code TUI.

The question (COSMON #26 residual): when a briefing is pasted into a Claude
Code pane and the submit CR follows it, is the CR ever *swallowed* — and if so,
is the byte missing from the application's PTY (our defect) or present and
ignored (a TUI race that a longer pause would dodge)?

One trial:

  1. start `claude` in a fresh tmux session on an isolated socket, wrapped in
     `ptyspy.py` so every byte the application receives is on disk;
  2. wait for the composer to render;
  3. reproduce `TmuxBackend::send_input` exactly once: `C-u`,
     `paste-buffer -d -p`, sleep <delay>, `send-keys -H 0d`;
  4. **do not retry** — the production retry loop is what hides the phenomenon,
     so this harness presses submit once and watches;
  5. poll the composer for `settle_s` seconds, recording when it clears;
  6. read the PTY log back: was a `0d` delivered after the bracketed-paste
     terminator?

Each trial appends one JSON line to the results file before the next starts, so
a machine that sleeps mid-run loses at most the trial in flight.
"""

import argparse
import atexit
import json
import os
import shutil
import signal
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
SPY = os.path.join(HERE, "ptyspy.py")

# Bracketed-paste terminator: everything after this in the PTY stream is the
# separately-sent submit keystroke, not paste payload.
PASTE_END = "1b5b3230317e"
PASTE_START = "1b5b3230307e"

# Environment a cosmon worker carries that would poison a child Claude Code
# (see the CB_DEPTH / ANTHROPIC_MODEL findings); stripped for every trial.
STRIP_ENV = [
    "ANTHROPIC_MODEL",
    "CB_DEPTH",
    "CB_SESSION_ROLE",
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "COSMON_MOL_DIR",
    "COSMON_PARENT_MOL_ID",
]

COMPOSER_GLYPHS = ("❯", "›", "> ")
PASTED_PLACEHOLDER = "Pasted text"
# Claude Code >= 2.1.220 seeds the empty composer with a rotating hint
# (`❯ Try "fix lint errors"...`). An empty composer is therefore no longer a
# bare glyph, and a readiness gate demanding one waits out its timeout forever.
COMPOSER_PLACEHOLDER_PREFIX = 'Try "'


class Tmux:
    """A private tmux server, killed on every exit path including panic."""

    def __init__(self, socket: str):
        self.socket = socket
        atexit.register(self.kill_server)
        for sig in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, self._on_signal)

    def _on_signal(self, *_a):
        self.kill_server()
        sys.exit(130)

    def run(self, args, timeout=20):
        return subprocess.run(
            ["tmux", "-L", self.socket, *args],
            capture_output=True,
            text=True,
            timeout=timeout,
        )

    def kill_server(self):
        try:
            subprocess.run(
                ["tmux", "-L", self.socket, "kill-server"],
                capture_output=True,
                timeout=15,
            )
        except Exception:
            pass


def briefing(lines: int, marker: str) -> str:
    """A briefing of exactly `lines` lines whose last line is `marker`."""
    if lines <= 1:
        return marker
    body = [
        "Reply with exactly the three letters ACK and nothing else.",
        *(
            f"filler line {i:03d} lorem ipsum dolor sit amet consectetur adipiscing elit"
            for i in range(lines - 2)
        ),
        marker,
    ]
    return "\n".join(body)


def composer_region(pane: str) -> list[str]:
    """The visible composer, mirroring `composer_indicates_pending`'s scoping.

    Scoped, not a tail scan: a *submitted* briefing is echoed into the
    transcript above the composer, so an unscoped search for the marker would
    read every successful submission as pending.
    """
    lines = [line.rstrip() for line in pane.splitlines()]
    stripped = [line.strip() for line in lines]
    for idx in range(len(stripped) - 1, -1, -1):
        if any(stripped[idx].startswith(g) for g in COMPOSER_GLYPHS):
            return stripped[idx:]
    return stripped[-6:]


def composer_pending(pane: str, marker: str) -> bool:
    region = composer_region(pane)
    return any(PASTED_PLACEHOLDER in line or marker in line for line in region)


def parse_pty_log(path: str):
    """(hex stream, [(timestamp, hex chunk)]) as the application received it."""
    chunks = []
    with open(path) as fh:
        for line in fh:
            parts = line.split()
            if len(parts) == 2:
                chunks.append((float(parts[0]), parts[1]))
    return "".join(c for _, c in chunks), chunks


def cr_after_paste(stream: str, chunks) -> dict:
    """Did a submit CR reach the PTY after the bracketed paste closed?"""
    end = stream.rfind(PASTE_END)
    start = stream.rfind(PASTE_START)
    out = {
        "paste_start_seen": start >= 0,
        "paste_end_seen": end >= 0,
        "cr_after_paste": False,
        "cr_count_after_paste": 0,
        "cr_ts": None,
        "paste_end_ts": None,
    }
    if end < 0:
        return out
    tail = stream[end + len(PASTE_END) :]
    # Byte-align: hex pairs, count 0x0d bytes in the tail.
    tail_bytes = bytes.fromhex(tail) if len(tail) % 2 == 0 else b""
    out["cr_count_after_paste"] = tail_bytes.count(b"\r")
    out["cr_after_paste"] = out["cr_count_after_paste"] > 0
    # Timestamps: the chunk that carried the paste terminator, and the first
    # chunk after it containing a CR.
    cursor = 0
    for ts, chunk in chunks:
        chunk_end = cursor + len(chunk)
        if out["paste_end_ts"] is None and cursor <= end < chunk_end:
            out["paste_end_ts"] = ts
        elif out["paste_end_ts"] is not None and out["cr_ts"] is None:
            if b"\r" in bytes.fromhex(chunk):
                out["cr_ts"] = ts
        cursor = chunk_end
    return out


def composer_line_is_idle(line: str) -> bool:
    """Is this composer line an *empty* composer?

    Two renderings mean empty, and only these two: a bare glyph, and a glyph
    followed by the rotating placeholder hint the TUI draws when there is
    nothing to submit. Anything else after the glyph is real text a user (or a
    paste) put there, which is exactly the state readiness must keep rejecting.
    """
    text = line.strip()
    for glyph in COMPOSER_GLYPHS:
        marker = glyph.strip()
        if not text.startswith(marker):
            continue
        rest = text[len(marker) :].strip()
        return not rest or rest.startswith(COMPOSER_PLACEHOLDER_PREFIX)
    return False


def wait_ready(tm: Tmux, session: str, timeout_s: float) -> bool:
    """Composer rendered and empty — the state production waits for."""
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        pane = tm.run(["capture-pane", "-t", session, "-p"]).stdout
        region = composer_region(pane)
        if region and composer_line_is_idle(region[0]):
            # A composer glyph with nothing but the placeholder after it:
            # ready and idle.
            time.sleep(0.4)
            return True
        time.sleep(0.5)
    return False


def trial(tm: Tmux, args, delay_ms: int, size: int, rep: int) -> dict:
    session = f"t{size}_{delay_ms}_{rep}"
    marker = f"MARKER-{size}-{delay_ms}-{rep}"
    log_path = os.path.join(args.workdir, "logs", f"{session}.log")
    brief_path = os.path.join(args.workdir, f"{session}.txt")
    text = briefing(size, marker)
    with open(brief_path, "w") as fh:
        fh.write(text)

    env = {k: v for k, v in os.environ.items() if k not in STRIP_ENV}
    env["TERM"] = "xterm-256color"

    cmd = f"python3 {SPY} {log_path} -- {args.claude_bin}"
    proc = subprocess.run(
        [
            "tmux",
            "-L",
            tm.socket,
            "new-session",
            "-d",
            "-s",
            session,
            "-x",
            "200",
            "-y",
            "50",
            "-c",
            args.ws,
            cmd,
        ],
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
    )
    row = {
        "size_lines": size,
        "delay_ms": delay_ms,
        "rep": rep,
        "session": session,
    }
    if proc.returncode != 0:
        row["error"] = f"new-session failed: {proc.stderr.strip()}"
        return row

    try:
        if not wait_ready(tm, session, args.ready_timeout):
            row["error"] = "composer never rendered"
            row["pane"] = tm.run(["capture-pane", "-t", session, "-p"]).stdout[-800:]
            return row

        if args.busy:
            # The reported symptom is a nudge sent to a worker that is already
            # thinking, so the idle-composer grid alone cannot speak for it.
            # Put the TUI into a long response first, then paste on top.
            tm.run(
                [
                    "send-keys",
                    "-t",
                    session,
                    "-l",
                    "Count from 1 to 400, one number per line, no commentary.",
                ]
            )
            tm.run(["send-keys", "-t", session, "-H", "0d"])
            time.sleep(args.busy_wait_s)
            row["busy"] = True

        buf = f"b{session}"
        tm.run(["load-buffer", "-b", buf, brief_path])
        tm.run(["send-keys", "-t", session, "C-u"])
        t_paste = time.time()
        tm.run(["paste-buffer", "-d", "-p", "-b", buf, "-t", session])
        time.sleep(delay_ms / 1000.0)
        t_cr = time.time()
        if not args.no_cr:
            tm.run(["send-keys", "-t", session, "-H", "0d"])

        # Single-shot: no retry. Poll until the composer clears or we give up.
        cleared_at = None
        deadline = time.time() + args.settle_s
        last_pane = ""
        while time.time() < deadline:
            time.sleep(args.poll_s)
            last_pane = tm.run(["capture-pane", "-t", session, "-p"]).stdout
            if not composer_pending(last_pane, marker):
                cleared_at = time.time()
                break
        row["pending_after_settle"] = cleared_at is None
        row["clear_after_cr_ms"] = (
            None if cleared_at is None else round((cleared_at - t_cr) * 1000)
        )
        row["paste_to_cr_actual_ms"] = round((t_cr - t_paste) * 1000)
        row["pane_tail"] = "\n".join(composer_region(last_pane))[:400]
    finally:
        tm.run(["kill-session", "-t", session])
        time.sleep(0.3)

    try:
        stream, chunks = parse_pty_log(log_path)
        row.update(cr_after_paste(stream, chunks))
        if row.get("cr_ts") and row.get("paste_end_ts"):
            row["pty_paste_to_cr_ms"] = round(
                (row["cr_ts"] - row["paste_end_ts"]) * 1000
            )
    except OSError as exc:
        row["error"] = f"pty log unreadable: {exc}"
    return row


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--workdir", required=True)
    ap.add_argument("--ws", required=True, help="cwd for the Claude Code process")
    ap.add_argument("--out", required=True)
    ap.add_argument("--claude-bin", default="claude")
    ap.add_argument("--socket", default="cosmon-test-submitrace")
    ap.add_argument("--delays", default="0,100,250,500,1000")
    ap.add_argument("--sizes", default="1,12,100,300")
    ap.add_argument("--reps", type=int, default=5)
    ap.add_argument("--settle-s", type=float, default=4.0)
    ap.add_argument("--poll-s", type=float, default=0.3)
    ap.add_argument("--ready-timeout", type=float, default=60.0)
    ap.add_argument(
        "--busy",
        action="store_true",
        help="paste onto a TUI that is already mid-response — the condition "
        "under which the swallowed Enter was reported.",
    )
    ap.add_argument("--busy-wait-s", type=float, default=3.0)
    ap.add_argument(
        "--no-cr",
        action="store_true",
        help="negative control: paste and never press submit. Every cell must "
        "report pending=True, otherwise the composer detector is vacuous and "
        "the matrix proves nothing.",
    )
    args = ap.parse_args()

    os.makedirs(os.path.join(args.workdir, "logs"), exist_ok=True)
    if not shutil.which("tmux"):
        print("tmux not found", file=sys.stderr)
        return 2

    delays = [int(x) for x in args.delays.split(",") if x]
    sizes = [int(x) for x in args.sizes.split(",") if x]
    tm = Tmux(args.socket)
    tm.kill_server()

    total = len(delays) * len(sizes) * args.reps
    done = 0
    with open(args.out, "a", buffering=1) as out:
        for rep in range(args.reps):
            for size in sizes:
                for delay in delays:
                    row = trial(tm, args, delay, size, rep)
                    out.write(json.dumps(row) + "\n")
                    done += 1
                    print(
                        f"[{done}/{total}] size={size} delay={delay} rep={rep} "
                        f"pending={row.get('pending_after_settle')} "
                        f"cr_at_pty={row.get('cr_after_paste')} "
                        f"clear_ms={row.get('clear_after_cr_ms')} "
                        f"{row.get('error', '')}",
                        flush=True,
                    )
    tm.kill_server()
    return 0


if __name__ == "__main__":
    sys.exit(main())
