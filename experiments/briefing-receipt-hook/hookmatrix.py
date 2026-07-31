#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Event-driven briefing receipt vs. the timed one — measured against a live TUI.

The question (follow-up to `experiments/briefing-submit-race`): cosmon's submit
path pays a fixed 500 ms pause and then screen-scrapes a composer every 300 ms
to *guess* whether the briefing was submitted. Claude Code can instead be asked
to say so. Is the receipt faster, is it as reliable, and what does it do when
hooks are unavailable, blocked, broken, or lying?

Two arms, one trial each:

  prod   — `TmuxBackend::send_input` reproduced: C-u, paste, sleep 500 ms, CR,
           then poll `capture-pane` every 300 ms and re-press CR while the
           composer still shows the paste.
  event  — the candidate: an ephemeral `--settings` overlay registers a
           `UserPromptSubmit` hook; C-u, paste, CR immediately, then poll a
           local file for a receipt keyed to a per-dispatch nonce, re-pressing
           CR only while none exists.

Both arms run the target under `ptyspy.py`, so the carriage returns each one
actually delivered to the application are counted from the PTY stream rather
than from what the driver believes it sent.

Scenarios (see `SCENARIOS`) cover the failure modes the mechanism has to
survive: a pre-existing operator hook, a second hook that blocks the prompt,
our hook command failing to execute, hooks not installed at all, a receipt
directory that cannot be written, a receipt destination that is not a
directory, a hook that leaks stdout, and a first carriage return that is
deliberately never sent.

Isolation: a private tmux socket (`cosmon-test-` by convention), killed on
every exit path including a signal. No settings file outside the scratch
directory is read or written.
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

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import receipt as rcpt  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
SPY = os.path.join(HERE, "ptyspy.py")

PASTE_END = "1b5b3230317e"
PASTE_START = "1b5b3230307e"

# Environment a cosmon worker carries that would poison a child Claude Code
# (the CB_DEPTH / ANTHROPIC_MODEL findings); stripped for every trial.
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

#: Production constants, mirrored so the `prod` arm is the real thing.
PROD_PASTE_PAUSE_MS = 500
PROD_POLL_INTERVAL_MS = 300
PROD_RETRY_BUDGET_BASE = 5
PROD_LINES_PER_PASTE_BLOCK = 12

STDOUT_SENTINEL = "COSMON-HOOK-STDOUT-SENTINEL-7QX"


class Tmux:
    """A private tmux server, killed on every exit path including a signal."""

    def __init__(self, socket: str):
        self.socket = socket
        atexit.register(self.kill_server)
        for sig in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, self._on_signal)

    def _on_signal(self, *_a):
        self.kill_server()
        sys.exit(130)

    def run(self, args, timeout=20):
        try:
            return subprocess.run(
                ["tmux", "-L", self.socket, *args],
                capture_output=True,
                text=True,
                timeout=timeout,
            )
        except subprocess.TimeoutExpired:
            return subprocess.CompletedProcess(args, 1, "", "timeout")

    def kill_server(self):
        try:
            subprocess.run(
                ["tmux", "-L", self.socket, "kill-server"],
                capture_output=True,
                timeout=15,
            )
        except Exception:
            pass


# --------------------------------------------------------------------------
# Pane reading — identical scoping to the race harness, so the two are
# comparable measurements of the same detector.
# --------------------------------------------------------------------------


def composer_region(pane: str) -> list:
    lines = [line.rstrip() for line in pane.splitlines()]
    stripped = [line.strip() for line in lines]
    for idx in range(len(stripped) - 1, -1, -1):
        if any(stripped[idx].startswith(g) for g in COMPOSER_GLYPHS):
            return stripped[idx:]
    return stripped[-6:]


def composer_pending(pane: str, marker: str) -> bool:
    region = composer_region(pane)
    return any(PASTED_PLACEHOLDER in line or marker in line for line in region)


def wait_ready(tm: Tmux, session: str, timeout_s: float) -> bool:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        pane = tm.run(["capture-pane", "-t", session, "-p"]).stdout
        region = composer_region(pane)
        if region and any(
            line.strip() in {g.strip() for g in COMPOSER_GLYPHS} for line in region[:1]
        ):
            time.sleep(0.4)
            return True
        time.sleep(0.5)
    return False


def briefing(lines: int, marker: str) -> str:
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


# --------------------------------------------------------------------------
# PTY stream reading — the carriage returns the application actually received
# --------------------------------------------------------------------------


def parse_pty_log(path: str):
    chunks = []
    with open(path) as fh:
        for line in fh:
            parts = line.split()
            if len(parts) == 2:
                chunks.append((float(parts[0]), parts[1]))
    return "".join(c for _, c in chunks), chunks


def crs_after_paste(stream: str, chunks) -> dict:
    """Every `0d` the application received after the bracketed paste closed.

    The count, not just the presence, is what this experiment needs: the
    duplicate-CR problem the receipt is meant to remove is invisible to a
    boolean.
    """
    end = stream.rfind(PASTE_END)
    out = {
        "paste_end_seen": end >= 0,
        "cr_count_after_paste": 0,
        "cr_ts": [],
        "paste_end_ts": None,
    }
    if end < 0:
        return out
    tail = stream[end + len(PASTE_END) :]
    tail_bytes = bytes.fromhex(tail) if len(tail) % 2 == 0 else b""
    out["cr_count_after_paste"] = tail_bytes.count(b"\r")
    cursor = 0
    for ts, chunk in chunks:
        chunk_end = cursor + len(chunk)
        if out["paste_end_ts"] is None and cursor <= end < chunk_end:
            out["paste_end_ts"] = ts
        elif out["paste_end_ts"] is not None:
            n = bytes.fromhex(chunk).count(b"\r")
            out["cr_ts"].extend([ts] * n)
        cursor = chunk_end
    return out


# --------------------------------------------------------------------------
# Scenarios
# --------------------------------------------------------------------------

# Each scenario is a dict of switches read by `build_overlay` and `trial`.
#   overlay:      "ours" | "none" | "ours+user" | "ours+blocking" | "broken"
#   ack_dir:      "normal" | "unwritable" | "not_a_dir"
#   busy:         put the TUI mid-response before pasting
#   suppress_cr:  never send the first CR; only the receipt-driven retry can
#                 recover the submit
#   dispatches:   how many briefings to send into the one session
#   stdout_leak:  the hook writes a sentinel to stdout
#   no_retry:     press submit exactly once, so the receipt's arrival time
#                 measures the hook's own latency and cannot be confounded by
#                 a retry that happened to land first
#   retry_ms:     override the re-press interval (the tuned configuration)
SCENARIOS = {
    "normal": {},
    "no_retry": {"no_retry": True},
    "tuned": {"retry_ms": 2000},
    "tuned_busy": {"retry_ms": 2000, "busy": True},
    "busy": {"busy": True},
    "user_hook": {"overlay": "ours+user"},
    "blocking_hook": {"overlay": "ours+blocking"},
    "hook_fail": {"overlay": "broken"},
    "hooks_disabled": {"overlay": "none"},
    "unwritable_ack": {"ack_dir": "unwritable"},
    "malformed_dest": {"ack_dir": "not_a_dir"},
    "stdout_leak": {"stdout_leak": True},
    "suppress_first_cr": {"suppress_cr": True},
    "two_dispatches": {"dispatches": 2},
}


def build_overlay(kind: str, overlay_path: str, receipt_dir: str, nonce_file: str,
                  stdout_leak: bool) -> bool:
    """Write the ephemeral overlay. Returns False when the arm runs without one."""
    if kind == "none":
        return False
    ours = rcpt.hook_command(
        receipt_dir,
        nonce_file,
        stdout_leak=STDOUT_SENTINEL if stdout_leak else None,
    )
    if kind == "broken":
        # The hook command cannot execute: the exact shape of "the mechanism is
        # configured but does not work", which must land in the fallback rather
        # than hang.
        cmds = [os.path.join(HERE, "definitely-not-a-real-binary-9d2f")]
    elif kind == "ours+user":
        # An operator hook that already exists on this event. Ours must coexist,
        # not replace — and must not be delayed past usefulness by theirs.
        cmds = ["/usr/bin/env sleep 0.25", ours]
    elif kind == "ours+blocking":
        # A second hook that blocks the prompt. This is the case that decides
        # what a receipt is allowed to claim.
        cmds = [ours, "/bin/sh -c 'exit 2'"]
    else:
        cmds = [ours]
    rcpt.mint_overlay(overlay_path, cmds)
    return True


# --------------------------------------------------------------------------
# The two submit paths
# --------------------------------------------------------------------------


def prod_submit(tm: Tmux, session: str, buf: str, marker: str, size: int,
                settle_s: float) -> dict:
    """`TmuxBackend::send_input`, reproduced: fixed pause, then poll pixels."""
    budget = PROD_RETRY_BUDGET_BASE + max(
        0, (size + PROD_LINES_PER_PASTE_BLOCK - 1) // PROD_LINES_PER_PASTE_BLOCK - 1
    )
    tm.run(["send-keys", "-t", session, "C-u"])
    t0 = time.time()
    tm.run(["paste-buffer", "-d", "-p", "-b", buf, "-t", session])
    time.sleep(PROD_PASTE_PAUSE_MS / 1000.0)
    tm.run(["send-keys", "-t", session, "-H", "0d"])
    submits = 1
    cleared_at = None
    polls = 0
    while polls < budget:
        time.sleep(PROD_POLL_INTERVAL_MS / 1000.0)
        polls += 1
        pane = tm.run(["capture-pane", "-t", session, "-p"]).stdout
        if not composer_pending(pane, marker):
            cleared_at = time.time()
            break
        tm.run(["send-keys", "-t", session, "-H", "0d"])
        submits += 1
    return {
        "arm": "prod",
        "evidence": rcpt.COMPOSER if cleared_at else rcpt.UNOBSERVED,
        "latency_ms": round(((cleared_at or time.time()) - t0) * 1000),
        "submits_sent": submits,
        "polls": polls,
        "budget": budget,
        "t_paste": t0,
    }


def event_submit(tm: Tmux, session: str, buf: str, marker: str, nonce: str,
                 receipt_dir: str, nonce_file: str, suppress_cr: bool,
                 ack_deadline_s: float, retry_s: float) -> dict:
    """The candidate: stamp the nonce, paste, submit at once, wait for a receipt."""
    rcpt.stamp_nonce(nonce_file, nonce)
    tm.run(["send-keys", "-t", session, "C-u"])
    t0 = time.time()
    tm.run(["paste-buffer", "-d", "-p", "-b", buf, "-t", session])

    def press():
        tm.run(["send-keys", "-t", session, "-H", "0d"])

    def read_composer():
        pane = tm.run(["capture-pane", "-t", session, "-p"]).stdout
        if not pane:
            return None
        return composer_pending(pane, marker)

    r = rcpt.await_receipt(
        nonce=nonce,
        receipt_dir=receipt_dir,
        press_submit=press,
        composer_pending=read_composer,
        ack_deadline_s=ack_deadline_s,
        retry_interval_s=retry_s,
        t0=t0,
        first_submit=not suppress_cr,
    )
    row = r.as_row()
    row["arm"] = "event"
    row["t_paste"] = t0
    return row


# --------------------------------------------------------------------------
# One trial
# --------------------------------------------------------------------------


def trial(tm: Tmux, args, arm: str, scen_name: str, size: int, rep: int) -> dict:
    scen = SCENARIOS[scen_name]
    tag = f"{arm}_{scen_name}_{size}_{rep}"
    workdir = os.path.join(args.workdir, tag)
    os.makedirs(os.path.join(workdir, "logs"), exist_ok=True)
    receipt_dir = os.path.join(workdir, "receipts")
    nonce_file = os.path.join(workdir, "nonce")
    overlay_path = os.path.join(workdir, "overlay.settings.json")
    log_path = os.path.join(workdir, "logs", "pty.log")

    row = {
        "arm": arm,
        "scenario": scen_name,
        "size_lines": size,
        "rep": rep,
    }

    ack_dir_mode = scen.get("ack_dir", "normal")
    if ack_dir_mode == "not_a_dir":
        # The destination is a regular file: a receipt can never be created,
        # and the hook must not fail the prompt over it.
        with open(receipt_dir, "w") as fh:
            fh.write("this is not a directory\n")
    else:
        os.makedirs(receipt_dir, exist_ok=True)
        if ack_dir_mode == "unwritable":
            os.chmod(receipt_dir, 0o500)

    use_overlay = False
    if arm == "event":
        use_overlay = build_overlay(
            scen.get("overlay", "ours"),
            overlay_path,
            receipt_dir,
            nonce_file,
            scen.get("stdout_leak", False),
        )
    row["overlay"] = scen.get("overlay", "ours") if arm == "event" else "none"

    env = {k: v for k, v in os.environ.items() if k not in STRIP_ENV}
    env["TERM"] = "xterm-256color"

    claude_argv = args.claude_bin
    if use_overlay:
        claude_argv += f" --settings {overlay_path}"
    cmd = f"python3 {SPY} {log_path} -- {claude_argv}"

    session = f"s{abs(hash(tag)) % 10**8}"
    proc = subprocess.run(
        [
            "tmux", "-L", tm.socket, "new-session", "-d", "-s", session,
            "-x", "200", "-y", "50", "-c", args.ws, cmd,
        ],
        capture_output=True, text=True, env=env, timeout=30,
    )
    if proc.returncode != 0:
        row["error"] = f"new-session failed: {proc.stderr.strip()}"
        return row

    dispatches = scen.get("dispatches", 1)
    results = []
    try:
        if not wait_ready(tm, session, args.ready_timeout):
            row["error"] = "composer never rendered"
            row["pane"] = tm.run(["capture-pane", "-t", session, "-p"]).stdout[-600:]
            return row

        if scen.get("busy"):
            tm.run([
                "send-keys", "-t", session, "-l",
                "Count from 1 to 400, one number per line, no commentary.",
            ])
            tm.run(["send-keys", "-t", session, "-H", "0d"])
            time.sleep(args.busy_wait_s)
            row["busy"] = True

        for d in range(dispatches):
            marker = f"MARK-{tag}-{d}"
            text = briefing(size, marker)
            brief_path = os.path.join(workdir, f"brief{d}.txt")
            with open(brief_path, "w") as fh:
                fh.write(text)
            buf = f"b{session}{d}"
            tm.run(["load-buffer", "-b", buf, brief_path])

            if arm == "prod":
                res = prod_submit(tm, session, buf, marker, size, args.settle_s)
            else:
                retry_s = (
                    1e9
                    if scen.get("no_retry")
                    else scen.get("retry_ms", PROD_POLL_INTERVAL_MS) / 1000.0
                )
                res = event_submit(
                    tm, session, buf, marker,
                    rcpt.mint_nonce(), receipt_dir, nonce_file,
                    scen.get("suppress_cr", False), args.ack_deadline_s,
                    retry_s,
                )
            res["dispatch_index"] = d

            # Everything after the evidence arrived: did anything keep pressing?
            # The prototype must send zero further carriage returns.
            time.sleep(args.post_evidence_watch_s)
            pane = tm.run(["capture-pane", "-t", session, "-p"]).stdout
            res["composer_pending_after"] = composer_pending(pane, marker)
            res["pane_tail"] = "\n".join(composer_region(pane))[:300]
            res["stdout_sentinel_in_pane"] = STDOUT_SENTINEL in pane
            results.append(res)
            if d + 1 < dispatches:
                time.sleep(args.between_dispatch_s)

        # The whole pane, once, for the stdout-leak scenario: hook stdout is
        # injected as context, so if it shows up anywhere it shows up here.
        full = tm.run(["capture-pane", "-t", session, "-p", "-S", "-"]).stdout
        row["sentinel_anywhere"] = STDOUT_SENTINEL in full
    finally:
        tm.run(["kill-session", "-t", session])
        if ack_dir_mode == "unwritable":
            try:
                os.chmod(receipt_dir, 0o700)
            except OSError:
                pass
        time.sleep(0.3)

    # Receipts on disk, independent of what the driver thought it saw.
    try:
        acks = sorted(os.listdir(receipt_dir))
    except (OSError, NotADirectoryError):
        acks = []
    row["ack_files"] = acks
    row["ack_count"] = len(acks)

    try:
        stream, chunks = parse_pty_log(log_path)
        row.update(crs_after_paste(stream, chunks))
    except OSError as exc:
        row["pty_error"] = str(exc)

    # Per-dispatch CR accounting: how many carriage returns reached the
    # application after each dispatch's evidence timestamp.
    row["dispatches"] = results
    return row


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--workdir", required=True)
    ap.add_argument("--ws", required=True, help="a directory Claude Code already trusts")
    ap.add_argument("--out", required=True)
    ap.add_argument("--claude-bin", default="claude")
    ap.add_argument("--socket", default="cosmon-test-receipt")
    ap.add_argument("--arms", default="prod,event")
    ap.add_argument("--scenarios", default="normal")
    ap.add_argument("--sizes", default="12")
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--settle-s", type=float, default=4.0)
    ap.add_argument("--ack-deadline-s", type=float, default=8.0)
    ap.add_argument("--post-evidence-watch-s", type=float, default=1.5)
    ap.add_argument("--between-dispatch-s", type=float, default=2.0)
    ap.add_argument("--ready-timeout", type=float, default=90.0)
    ap.add_argument("--busy-wait-s", type=float, default=3.0)
    args = ap.parse_args()

    if not shutil.which("tmux"):
        print("tmux not found", file=sys.stderr)
        return 2
    if not args.socket.startswith("cosmon-test-"):
        print("refusing a socket outside the cosmon-test- namespace", file=sys.stderr)
        return 2

    os.makedirs(args.workdir, exist_ok=True)
    arms = [a for a in args.arms.split(",") if a]
    scenarios = [s for s in args.scenarios.split(",") if s]
    sizes = [int(x) for x in args.sizes.split(",") if x]
    for s in scenarios:
        if s not in SCENARIOS:
            print(f"unknown scenario: {s}", file=sys.stderr)
            return 2

    # Refuse to share a socket with a run already in progress. Two runners on
    # one socket cost this experiment a whole matrix: they interleaved trials,
    # deleted each other's scratch directories mid-trial, and appended to the
    # same results file, so the failures they recorded belonged to the harness
    # rather than to the mechanism.
    probe = subprocess.run(
        ["tmux", "-L", args.socket, "list-sessions"],
        capture_output=True, text=True, timeout=15,
    )
    if probe.returncode == 0 and probe.stdout.strip():
        print(
            f"socket {args.socket} already has sessions; refusing to share it",
            file=sys.stderr,
        )
        return 2

    tm = Tmux(args.socket)
    tm.kill_server()
    total = len(arms) * len(scenarios) * len(sizes) * args.reps
    done = 0
    with open(args.out, "a", buffering=1) as out:
        for rep in range(args.reps):
            for scen in scenarios:
                for size in sizes:
                    for arm in arms:
                        if arm == "prod" and scen not in ("normal", "busy"):
                            # The hook scenarios have no meaning without a hook;
                            # `prod` is the baseline for the two real conditions.
                            continue
                        row = trial(tm, args, arm, scen, size, rep)
                        out.write(json.dumps(row) + "\n")
                        done += 1
                        d0 = (row.get("dispatches") or [{}])[0]
                        print(
                            f"[{done}/{total}] {arm}/{scen} size={size} rep={rep} "
                            f"ev={d0.get('evidence')} lat={d0.get('latency_ms')} "
                            f"crs={row.get('cr_count_after_paste')} "
                            f"acks={row.get('ack_count')} "
                            f"{row.get('error', '')}",
                            flush=True,
                        )
    tm.kill_server()
    return 0


if __name__ == "__main__":
    sys.exit(main())
