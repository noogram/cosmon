#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""The hook's invariants, checked without a TUI in the loop.

These are the properties that make the mechanism safe to put in front of every
prompt a fleet worker ever submits. They are cheap and deterministic, so they
belong in a file that runs in a second rather than in an hour-long matrix:

  * stdout is empty — `UserPromptSubmit` stdout becomes model context;
  * the exit status is 0 on every failure path — a receipt hook must never be
    able to block a prompt;
  * no prompt content, transcript path, or cwd is written anywhere;
  * a hostile nonce cannot make the hook write outside its directory;
  * the receipt is complete when it appears (write-then-rename).

Run: `python3 test_ack_hook.py` — prints one line per check, exits non-zero on
the first failure.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
HOOK = os.path.join(HERE, "ack_hook.py")

PROMPT = "SENSITIVE-BRIEFING-BODY-DO-NOT-PERSIST"
TRANSCRIPT = "/Users/someone/.claude/projects/x/transcript.jsonl"
PAYLOAD = json.dumps(
    {
        "session_id": "sess-abc",
        "hook_event_name": "UserPromptSubmit",
        "prompt": PROMPT,
        "cwd": "/Users/someone/secret-project",
        "transcript_path": TRANSCRIPT,
    }
)

failures = []


def check(name: str, ok: bool, detail: str = "") -> None:
    print(f"{'ok  ' if ok else 'FAIL'} {name}{(' — ' + detail) if detail else ''}")
    if not ok:
        failures.append(name)


def run_hook(env: dict, payload: str = PAYLOAD):
    e = dict(os.environ)
    e.update(env)
    return subprocess.run(
        [sys.executable, HOOK],
        input=payload,
        capture_output=True,
        text=True,
        env=e,
        timeout=20,
    )


def tree_contains(root: str, needle: str) -> bool:
    for dirpath, _dirs, files in os.walk(root):
        for f in files:
            try:
                with open(os.path.join(dirpath, f), "rb") as fh:
                    if needle.encode() in fh.read():
                        return True
            except OSError:
                continue
    return False


def main() -> int:
    tmp = tempfile.mkdtemp(prefix="cosmon-receipt-test-")
    try:
        receipts = os.path.join(tmp, "receipts")
        os.makedirs(receipts)
        nonce_file = os.path.join(tmp, "nonce")
        with open(nonce_file, "w") as fh:
            fh.write("deadbeefcafe\n")
        base = {
            "COSMON_RECEIPT_DIR": receipts,
            "COSMON_RECEIPT_NONCE_FILE": nonce_file,
        }

        # --- the happy path -------------------------------------------------
        r = run_hook(base)
        check("happy: exit 0", r.returncode == 0, f"rc={r.returncode}")
        check("happy: stdout empty", r.stdout == "", repr(r.stdout[:60]))
        ack = os.path.join(receipts, "ack-deadbeefcafe.json")
        check("happy: receipt keyed to the nonce", os.path.exists(ack))
        if os.path.exists(ack):
            with open(ack) as fh:
                rec = json.load(fh)
            check("happy: receipt is complete JSON", rec.get("nonce") == "deadbeefcafe")
            check(
                "happy: only correlation fields",
                set(rec) == {"nonce", "session_id", "event", "hook_ts", "written_ts"},
                str(sorted(rec)),
            )

        check("no prompt content anywhere under the receipt dir",
              not tree_contains(tmp, PROMPT))
        check("no transcript path anywhere under the receipt dir",
              not tree_contains(tmp, TRANSCRIPT))
        check("no temp file left behind",
              not any(f.startswith(".ack-tmp-") for f in os.listdir(receipts)))

        # --- failure paths: all must exit 0, all must stay silent -----------
        cases = {
            "unwritable receipt dir": {**base, "COSMON_RECEIPT_DIR": os.path.join(tmp, "ro")},
            "receipt dir is a file": {**base, "COSMON_RECEIPT_DIR": os.path.join(tmp, "afile")},
            "receipt dir unset": {"COSMON_RECEIPT_NONCE_FILE": nonce_file},
            "nonce file missing": {**base, "COSMON_RECEIPT_NONCE_FILE": os.path.join(tmp, "nope")},
        }
        os.makedirs(os.path.join(tmp, "ro"), exist_ok=True)
        os.chmod(os.path.join(tmp, "ro"), 0o500)
        with open(os.path.join(tmp, "afile"), "w") as fh:
            fh.write("not a directory\n")
        for name, env in cases.items():
            r = run_hook(env)
            check(f"{name}: exit 0", r.returncode == 0, f"rc={r.returncode}")
            check(f"{name}: stdout empty", r.stdout == "")
        os.chmod(os.path.join(tmp, "ro"), 0o700)

        for name, payload in {
            "empty stdin": "",
            "non-JSON stdin": "}}}not json{{{",
            "JSON that is not an object": "[1, 2, 3]",
        }.items():
            r = run_hook(base, payload)
            check(f"{name}: exit 0", r.returncode == 0, f"rc={r.returncode}")
            check(f"{name}: stdout empty", r.stdout == "")

        # --- a hostile nonce must not escape the receipt directory ----------
        with open(nonce_file, "w") as fh:
            fh.write("../../../../tmp/escaped\n")
        before = set(os.listdir(receipts))
        r = run_hook(base)
        after = set(os.listdir(receipts))
        check("path traversal: exit 0", r.returncode == 0)
        check("path traversal: nothing written outside the receipt dir",
              not os.path.exists("/tmp/escaped.json") and len(after - before) == 1,
              str(sorted(after - before)))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    if failures:
        print(f"\n{len(failures)} check(s) failed: {failures}")
        return 1
    print("\nall checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
