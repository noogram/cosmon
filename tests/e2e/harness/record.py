# SPDX-License-Identifier: AGPL-3.0-only
"""The run record: every exchange, on disk, in the order it happened.

The shell harness this replaces wrote one NDJSON line per step. That
line is kept — a nightly reader still wants a one-screen answer to
"which step broke" — but it is no longer the only artefact. Each
exchange also gets its own file under ``artifacts/``, holding the full
request and the full response, because "which step broke" and "what
exactly did the server say" are different questions and the second one
is the one a debugger asks.

Nothing here redacts by guessing. The recorder writes what it was given;
callers pass :meth:`Recorder.exchange` a command line whose secrets are
already elided (see :mod:`harness.remote`, which never puts a token on a
command line in the first place).
"""
from __future__ import annotations

import json
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import List, Optional


@dataclass
class Exchange:
    """One request/response pair, whatever the transport was.

    ``request`` is a human-readable rendering of what was sent (an argv
    for a CLI call, a method + URL for a direct HTTP probe) and
    ``response`` of what came back. They are strings rather than typed
    payloads on purpose: the value of this record is that it can hold a
    500's HTML body and a malformed JSON envelope as faithfully as it
    holds a well-formed one.
    """

    step: str
    request: str
    response: str
    rc: int
    ms: int
    stderr: str = ""

    def render(self) -> str:
        """Format the exchange for a failure report."""
        lines = [
            f"  step     : {self.step}  (rc={self.rc}, {self.ms} ms)",
            f"  request  : {self.request}",
            f"  response : {_indent_block(self.response)}",
        ]
        if self.stderr.strip():
            lines.append(f"  stderr   : {_indent_block(self.stderr)}")
        return "\n".join(lines)


def _indent_block(text: str, limit: int = 2000) -> str:
    text = text.strip()
    if not text:
        return "<empty>"
    if len(text) > limit:
        text = text[:limit] + f"… <truncated, {len(text)} bytes total>"
    lines = text.splitlines()
    if len(lines) == 1:
        return lines[0]
    return "\n" + "\n".join("             " + line for line in lines)


@dataclass
class Recorder:
    """Append-only record of a run, on disk and in memory.

    In memory so that :mod:`harness.expect` can quote the last exchange
    in a failure report without re-reading a file; on disk so that a
    nightly whose process is gone still has the evidence.
    """

    artifacts: Path
    ndjson: Path
    exchanges: List[Exchange] = field(default_factory=list)
    _seq: int = 0

    @classmethod
    def open(cls, artifacts: Path) -> "Recorder":
        artifacts.mkdir(parents=True, exist_ok=True)
        ndjson = artifacts / "e2e.ndjson"
        ndjson.touch()
        return cls(artifacts=artifacts, ndjson=ndjson)

    def exchange(
        self,
        step: str,
        request: str,
        response: str,
        rc: int,
        started: float,
        stderr: str = "",
    ) -> Exchange:
        """Record one exchange and return it."""
        ex = Exchange(
            step=step,
            request=request,
            response=response,
            rc=rc,
            ms=int((time.time() - started) * 1000),
            stderr=stderr,
        )
        self.exchanges.append(ex)
        self._seq += 1
        # One line per step — the same shape the shell harness wrote, so
        # a reader who knows `e2e.ndjson` keeps knowing it.
        with self.ndjson.open("a", encoding="utf-8") as fh:
            fh.write(
                json.dumps(
                    {"seq": self._seq, "step": ex.step, "rc": ex.rc, "ms": ex.ms},
                    sort_keys=True,
                )
                + "\n"
            )
        # …and one file per step, holding what the line cannot.
        safe = "".join(c if c.isalnum() or c in "-_" else "-" for c in step)
        path = self.artifacts / f"{self._seq:03d}-{safe}.txt"
        path.write_text(ex.render() + "\n", encoding="utf-8")
        return ex

    def last(self, step: Optional[str] = None) -> Optional[Exchange]:
        """The most recent exchange, optionally the most recent for a step."""
        for ex in reversed(self.exchanges):
            if step is None or ex.step == step:
                return ex
        return None
