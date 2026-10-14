#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Serve synthetic poll responses and record acknowledgements independently.

Two forms share this file. Run as a script, it serves one fixed response and
appends each request to a JSON-lines file, which is what the shell tests use.
Imported, ``FakeBotApi`` is a loopback stand-in for the bot API's long poll
that the handover test drives: it keeps a queue of synthetic updates, treats a
request at offset ``K`` as acknowledging every update below ``K`` exactly as
the real protocol does, can hold a request at a barrier, can answer 409, and
counts concurrent requests itself. Its trace is the witness of what the
readers did; nothing in it comes from a client-reported outcome.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import threading
import time
from urllib.parse import parse_qs, urlparse


OPERATOR_CHAT = 100000000


@dataclass
class Poll:
    """One request as the server saw it."""

    credential: str
    offset: int | None
    started: float
    finished: float | None = None
    status: int | None = None
    delivered: list[int] = field(default_factory=list)


class Hold:
    """A barrier the next request waits at until the test releases it."""

    def __init__(self) -> None:
        self.arrived = threading.Event()
        self.released = threading.Event()
        self.offset: int | None = None


class FakeBotApi:
    """Loopback long-poll endpoint with a queue of synthetic updates.

    ``credential`` in the trace is the token's part after its colon, so two
    hosts sharing one bot identity remain distinguishable by the server.
    """

    def __init__(self, empty_hold_seconds: float = 0.15) -> None:
        self.condition = threading.Condition()
        self.pending: list[dict] = []
        self.polls: list[Poll] = []
        self.holds: list[Hold] = []
        self.active = 0
        self.max_concurrent = 0
        self.acknowledged = 0
        self.conflict = False
        self.empty_hold_seconds = empty_hold_seconds
        api = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                api._serve(self)

            def log_message(self, _format: str, *args: object) -> None:
                return

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.server.server_port}"

    def close(self) -> None:
        with self.condition:
            for hold in self.holds:
                hold.released.set()
            self.condition.notify_all()
        self.server.shutdown()
        self.server.server_close()

    def add(self, update_id: int, chat: int = OPERATOR_CHAT, text: str = "synthetic") -> None:
        """Queue one synthetic message update; a foreign chat is meant to be filtered."""
        update = {
            "update_id": update_id,
            "message": {
                "message_id": update_id,
                "date": 1700000000 + update_id,
                "chat": {"id": chat},
                "from": {"id": 1, "is_bot": False, "first_name": "synthetic"},
                "text": f"{text} {update_id}",
            },
        }
        with self.condition:
            self.pending.append(update)
            self.condition.notify_all()

    def hold_next(self) -> Hold:
        """Make the next request wait at a barrier until ``released`` is set."""
        hold = Hold()
        with self.condition:
            self.holds.append(hold)
        return hold

    def trace(self) -> list[Poll]:
        with self.condition:
            return list(self.polls)

    def polls_by(self, credential: str) -> list[Poll]:
        return [poll for poll in self.trace() if poll.credential == credential]

    def _serve(self, handler: BaseHTTPRequestHandler) -> None:
        parsed = urlparse(handler.path)
        raw_offset = parse_qs(parsed.query).get("offset", [None])[0]
        offset = int(raw_offset) if raw_offset is not None and raw_offset.isdecimal() else None
        token = parsed.path.split("/")[1].removeprefix("bot")
        poll = Poll(credential=token.partition(":")[2], offset=offset, started=time.monotonic())
        with self.condition:
            self.active += 1
            self.max_concurrent = max(self.max_concurrent, self.active)
            self.polls.append(poll)
            hold = self.holds.pop(0) if self.holds else None
        try:
            if hold is not None:
                hold.offset = offset
                hold.arrived.set()
                hold.released.wait(timeout=30)
            status, body = self._answer(poll, offset)
            handler.send_response(status)
            handler.send_header("Content-Type", "application/json")
            handler.send_header("Content-Length", str(len(body)))
            handler.end_headers()
            handler.wfile.write(body)
        except OSError:
            pass
        finally:
            with self.condition:
                self.active -= 1
                poll.finished = time.monotonic()

    def _answer(self, poll: Poll, offset: int | None) -> tuple[int, bytes]:
        with self.condition:
            if self.conflict:
                poll.status = 409
                return 409, b'{"ok":false,"error_code":409,"description":"Conflict"}'
            if offset is not None:
                # A request above an update confirms it: the server drops it.
                self.acknowledged = max(self.acknowledged, offset - 1)
                self.pending = [u for u in self.pending if u["update_id"] >= offset]
            deadline = time.monotonic() + self.empty_hold_seconds
            while not self.pending and time.monotonic() < deadline:
                self.condition.wait(timeout=max(0.0, deadline - time.monotonic()))
            updates = list(self.pending)
            poll.status = 200
            poll.delivered = [u["update_id"] for u in updates]
        return 200, json.dumps({"ok": True, "result": updates}).encode()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--response", type=Path, required=True)
    parser.add_argument("--requests", type=Path, required=True)
    parser.add_argument("--port-file", type=Path, required=True)
    parser.add_argument("--status", type=int, default=200)
    args = parser.parse_args()

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            parsed = urlparse(self.path)
            query = parse_qs(parsed.query)
            acknowledgement = {
                "offset": query.get("offset", [None])[0],
                "allowed_updates": query.get("allowed_updates", [None])[0],
                "credential_is_synthetic": parsed.path.startswith("/botsynthetic-"),
            }
            with args.requests.open("a", encoding="utf-8") as handle:
                handle.write(json.dumps(acknowledgement, sort_keys=True) + "\n")
            body = args.response.read_bytes()
            self.send_response(args.status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, _format: str, *args: object) -> None:
            return

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    args.port_file.write_text(str(server.server_port), encoding="utf-8")
    server.serve_forever()


if __name__ == "__main__":
    main()
