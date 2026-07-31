#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""The prototype of the receipt-driven submit path, as cosmon would run it.

This is the piece a later implementation molecule would port into
`cosmon-transport`. It is kept in Python here so the experiment measures the
mechanism rather than a Rust rewrite of it, and so the typed outcome below can
be exercised against every failure mode without touching production code.

Three things live here:

- `mint_overlay` — the ephemeral per-session settings file. It is a *new* file
  in a scratch directory passed with `claude --settings`; it never reads,
  merges into, or rewrites the user, project, local or managed settings.
- `Receipt` — the typed outcome. Evidence is either an event acknowledgement or
  a composer reading, and the two are different variants precisely so no code
  path can quietly present the second as the first.
- `await_receipt` — paste, submit, and wait: for the receipt when hooks work,
  for the composer when they do not, with the reason for the demotion recorded.
"""

import json
import os
import secrets
import stat
import time
from dataclasses import dataclass, field, asdict
from typing import Callable, Optional

HERE = os.path.dirname(os.path.abspath(__file__))
ACK_HOOK = os.path.join(HERE, "ack_hook.py")


# --------------------------------------------------------------------------
# The overlay
# --------------------------------------------------------------------------


def hook_command(
    receipt_dir: str,
    nonce_file: str,
    *,
    measure_key: Optional[str] = None,
    stdout_leak: Optional[str] = None,
    hook_path: str = ACK_HOOK,
) -> str:
    """The shell command Claude Code runs on `UserPromptSubmit`.

    Environment is set inline rather than inherited, because the hook's
    contract must not depend on what the pane happened to export.
    """
    env = [
        f"COSMON_RECEIPT_DIR={receipt_dir}",
        f"COSMON_RECEIPT_NONCE_FILE={nonce_file}",
    ]
    if measure_key:
        env.append("COSMON_RECEIPT_MEASURE=1")
        env.append(f"COSMON_RECEIPT_KEY={measure_key}")
    if stdout_leak:
        env.append(f"COSMON_RECEIPT_STDOUT_LEAK={stdout_leak}")
    return " ".join(env) + f" /usr/bin/env python3 {hook_path}"


def mint_overlay(path: str, commands, timeout_s: int = 5) -> str:
    """Write a settings overlay registering `commands` on `UserPromptSubmit`.

    `commands` is a list of shell strings; more than one models the real case
    where the operator already has a `UserPromptSubmit` hook of their own and
    ours has to coexist with it rather than replace it.

    The file is created 0600. It holds nothing secret in the proposed
    configuration — a directory path and a nonce path — but the experiment's
    measurement mode puts an HMAC key in the command line, and a settings file
    that is sometimes secret should be always-0600 rather than
    conditionally-so.
    """
    doc = {
        "hooks": {
            "UserPromptSubmit": [
                {
                    "hooks": [
                        {"type": "command", "command": cmd, "timeout": timeout_s}
                        for cmd in commands
                    ]
                }
            ]
        }
    }
    with open(path, "w") as fh:
        json.dump(doc, fh, indent=2)
    os.chmod(path, stat.S_IRUSR | stat.S_IWUSR)
    return path


# --------------------------------------------------------------------------
# The typed outcome
# --------------------------------------------------------------------------

#: Evidence kinds, in descending order of what they prove.
EVENT_ACK = "event_ack"  # the application signed a receipt for our nonce
COMPOSER = "composer_cleared"  # we read pixels and inferred a submit
UNOBSERVED = "unobserved"  # neither; the submit is not known to have landed


@dataclass
class Receipt:
    """What a dispatch learned about its own submission.

    `evidence` is the whole point. A composer reading and an event
    acknowledgement answer the same question with very different confidence,
    and every previous version of this code path had only the weaker one, so
    there was no type to be honest with. Here there is: a caller that needs
    the strong claim checks `evidence == EVENT_ACK`, and a caller that only
    needs "probably submitted" accepts either — but neither can confuse them
    by accident, and `fallback_reason` always names why the demotion happened.
    """

    evidence: str
    nonce: str
    latency_ms: Optional[int] = None
    submits_sent: int = 0
    submits_after_evidence: int = 0
    fallback_reason: Optional[str] = None
    ack_session_id: Optional[str] = None
    extra: dict = field(default_factory=dict)

    @property
    def submitted(self) -> bool:
        return self.evidence in (EVENT_ACK, COMPOSER)

    def as_row(self) -> dict:
        return asdict(self)


def mint_nonce() -> str:
    return secrets.token_hex(8)


def stamp_nonce(nonce_file: str, nonce: str) -> None:
    """Publish the nonce for the dispatch about to happen, atomically.

    Written-then-renamed for the same reason the receipt is: the hook may read
    this file at any instant, including the instant we are rewriting it, and a
    half-written nonce would key the receipt to nothing.
    """
    tmp = nonce_file + ".tmp"
    with open(tmp, "w") as fh:
        fh.write(nonce + "\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.rename(tmp, nonce_file)


def read_ack(receipt_dir: str, nonce: str) -> Optional[dict]:
    """The receipt for `nonce`, or None. Never raises on a hostile directory."""
    path = os.path.join(receipt_dir, f"ack-{nonce}.json")
    try:
        with open(path) as fh:
            return json.load(fh)
    except (OSError, ValueError):
        return None


# --------------------------------------------------------------------------
# The submit path
# --------------------------------------------------------------------------


def await_receipt(
    *,
    nonce: str,
    receipt_dir: str,
    press_submit: Callable[[], None],
    composer_pending: Callable[[], Optional[bool]],
    ack_deadline_s: float = 8.0,
    retry_interval_s: float = 0.3,
    poll_s: float = 0.05,
    grace_after_ack_s: float = 0.0,
    t0: Optional[float] = None,
    first_submit: bool = True,
) -> Receipt:
    """Submit, then wait for the strongest evidence available.

    The loop is the proposal in miniature:

      * press submit immediately — there is no fixed pause to pay, because the
        receipt, not a guessed interval, is what says whether it landed;
      * poll for the receipt at 50 ms, which is cheap because it is a `stat`
        on a local file rather than a `capture-pane` subprocess;
      * re-press submit only while no receipt exists, at the production retry
        interval;
      * stop pressing the instant a receipt appears. `submits_after_evidence`
        is recorded and must be 0: the duplicate carriage returns the composer
        loop sends into an already-submitted composer are the failure this
        mechanism is meant to remove, so a prototype that reproduced them would
        be no better than what it replaces.

    When the deadline passes with no receipt, the composer is consulted and the
    outcome is *demoted*, never relabelled: `evidence` becomes `COMPOSER` (or
    `UNOBSERVED`) and `fallback_reason` says which of the failure modes this is.
    """
    t0 = t0 if t0 is not None else time.time()
    submits = 0
    if first_submit:
        press_submit()
        submits += 1
    last_retry = time.time()
    deadline = t0 + ack_deadline_s

    while time.time() < deadline:
        ack = read_ack(receipt_dir, nonce)
        if ack is not None:
            if grace_after_ack_s:
                time.sleep(grace_after_ack_s)
            return Receipt(
                evidence=EVENT_ACK,
                nonce=nonce,
                latency_ms=round((time.time() - t0) * 1000),
                submits_sent=submits,
                submits_after_evidence=0,
                ack_session_id=ack.get("session_id") or None,
                extra={
                    k: ack[k]
                    for k in ("prompt_len", "prompt_tag", "hook_ts", "written_ts")
                    if k in ack
                },
            )
        now = time.time()
        if now - last_retry >= retry_interval_s:
            press_submit()
            submits += 1
            last_retry = now
        time.sleep(poll_s)

    # No receipt. Everything below is the fallback, and it is labelled as such.
    pending = composer_pending()
    if pending is None:
        reason = "ack_absent_composer_unobservable"
        evidence = UNOBSERVED
    elif pending:
        reason = "ack_absent_composer_pending"
        evidence = UNOBSERVED
    else:
        reason = "ack_absent_composer_cleared"
        evidence = COMPOSER
    return Receipt(
        evidence=evidence,
        nonce=nonce,
        latency_ms=round((time.time() - t0) * 1000),
        submits_sent=submits,
        fallback_reason=reason,
    )
