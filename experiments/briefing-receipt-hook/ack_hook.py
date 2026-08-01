#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""The candidate delivery receipt: a `UserPromptSubmit` hook that signs a nonce.

Claude Code runs this once per submitted prompt, with the hook payload on
stdin. It writes one small file into a cosmon-owned directory and exits. That
file is the receipt: its existence means *this session accepted a prompt into
its lifecycle after cosmon stamped the nonce* — see `SEMANTICS` below for what
that does and does not prove.

Five properties are load-bearing, and each is enforced here rather than assumed:

1. **No stdout, ever.** `UserPromptSubmit` is the one hook event whose stdout is
   injected into the model's context. A stray byte — a Python warning, a
   library banner — becomes text the model reads as if the user typed it. So
   fd 1 is pointed at /dev/null before anything else runs, which makes the
   property structural instead of a promise about the code below.
2. **No prompt content leaves this process.** The payload carries `prompt`,
   `cwd`, `transcript_path` and `session_id`. Only `session_id` and
   `hook_event_name` are read; the rest is never touched, never logged, never
   written. `transcript_path` in particular would point a reader at the whole
   conversation.
3. **Atomic.** The receipt is written to a temp file in the destination
   directory, fsynced, then `os.rename`d into place. A reader therefore sees
   either no file or a complete one — never a half-written JSON object.
4. **Never blocks the user.** Every failure path exits 0. A hook that exits
   non-zero on `UserPromptSubmit` can block the prompt; a receipt mechanism
   that can eat prompts is worse than no receipt mechanism.
5. **Fast.** No network, no imports beyond the stdlib, one small write.

Environment (the ephemeral per-session overlay supplies these):

    COSMON_RECEIPT_DIR         directory to write receipts into (required)
    COSMON_RECEIPT_NONCE_FILE  file whose first line is the current nonce
    COSMON_RECEIPT_STDOUT_LEAK experiment only: emit a sentinel on stdout, to
                               measure what leaked stdout actually does
    COSMON_RECEIPT_MEASURE     experiment only: also record a keyed digest and
                               the length of the prompt, to answer whether an
                               exact per-prompt correlation is even available.
                               Off by default and not part of the proposal.
    COSMON_RECEIPT_KEY         HMAC key for COSMON_RECEIPT_MEASURE

SEMANTICS — what a receipt proves:

    A receipt proves the prompt entered Claude Code's `UserPromptSubmit`
    lifecycle. It does NOT prove the model began processing: another hook on
    the same event can still exit 2 and block the prompt after this one has
    written its file, and `experiments/briefing-receipt-hook` measures exactly
    that case. Treat it as "the submit landed", never as "the worker is
    working"; the latter is still the `Working` / `⏺` acceptance signal's job.
"""

import hashlib
import hmac
import json
import os
import sys
import tempfile
import time


def _mute_stdout():
    """Point fd 1 at /dev/null before any other code can write to it.

    Returns the original fd 1, duplicated, so the experiment-only stdout-leak
    mode can still deliberately write to the real stream.
    """
    real = os.dup(1)
    devnull = os.open(os.devnull, os.O_WRONLY)
    os.dup2(devnull, 1)
    os.close(devnull)
    return real


def _read_nonce() -> str:
    """The nonce cosmon stamped for the dispatch in flight.

    Read from a file rather than baked into the hook command, because the
    command is fixed for the session's lifetime while the nonce rotates once
    per dispatch. A missing or unreadable file is not an error: the receipt is
    still written, keyed `nokey`, which is how a reader tells "the hook never
    ran" apart from "the hook ran but cosmon had stamped nothing".
    """
    path = os.environ.get("COSMON_RECEIPT_NONCE_FILE", "")
    if not path:
        return "nokey"
    try:
        with open(path, "r") as fh:
            line = fh.readline().strip()
    except OSError:
        return "nokey"
    # Keep the nonce to a filename-safe alphabet; a nonce is minted by cosmon,
    # but this hook must not be the thing that turns a bad nonce into a path.
    safe = "".join(c for c in line if c.isalnum() or c in "-_")[:64]
    return safe or "nokey"


def _atomic_write(directory: str, name: str, payload: dict) -> bool:
    """Write `payload` as JSON at `directory/name`, atomically. False on any error."""
    try:
        fd, tmp = tempfile.mkstemp(dir=directory, prefix=".ack-tmp-")
    except OSError:
        return False
    try:
        with os.fdopen(fd, "w") as fh:
            json.dump(payload, fh)
            fh.flush()
            os.fsync(fh.fileno())
        os.rename(tmp, os.path.join(directory, name))
        return True
    except OSError:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        return False


def main() -> int:
    real_stdout = _mute_stdout()
    t_hook = time.time()

    raw = ""
    try:
        raw = sys.stdin.read()
    except (OSError, ValueError):
        pass

    session_id = ""
    event = ""
    prompt = None
    try:
        payload = json.loads(raw) if raw else {}
        if isinstance(payload, dict):
            # Correlation fields only. `prompt`, `cwd` and `transcript_path`
            # are deliberately left in `payload` and never copied out, except
            # under the experiment-only measurement flag below.
            session_id = str(payload.get("session_id", ""))[:64]
            event = str(payload.get("hook_event_name", ""))[:32]
            if os.environ.get("COSMON_RECEIPT_MEASURE") == "1":
                prompt = payload.get("prompt")
    except (ValueError, TypeError):
        pass

    nonce = _read_nonce()
    record = {
        "nonce": nonce,
        "session_id": session_id,
        "event": event,
        "hook_ts": t_hook,
        "written_ts": time.time(),
    }

    if prompt is not None and isinstance(prompt, str):
        # Experiment only, and still not the prompt: a keyed digest cannot be
        # reversed without the key, and the length is recorded to answer
        # whether the `prompt` field is even byte-identical to what cosmon
        # pasted (it is not, when the TUI collapses a paste).
        key = os.environ.get("COSMON_RECEIPT_KEY", "").encode()
        record["prompt_len"] = len(prompt)
        record["prompt_tag"] = hmac.new(
            key, prompt.encode("utf-8", "replace"), hashlib.sha256
        ).hexdigest()[:16]

    directory = os.environ.get("COSMON_RECEIPT_DIR", "")
    if directory:
        name = f"ack-{nonce}.json" if nonce != "nokey" else f"ack-nokey-{os.getpid()}.json"
        _atomic_write(directory, name, record)

    if os.environ.get("COSMON_RECEIPT_STDOUT_LEAK"):
        # Deliberate, experiment-only: measure what a hook's stdout does to the
        # model's context. Never enabled in the proposed configuration.
        try:
            os.write(real_stdout, os.environ["COSMON_RECEIPT_STDOUT_LEAK"].encode())
        except OSError:
            pass

    # Always 0: a receipt hook must never be able to block a prompt.
    return 0


if __name__ == "__main__":
    sys.exit(main())
