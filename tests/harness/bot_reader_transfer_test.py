#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Prove that ownership moves between hosts only through a committed offer.

Two temporary host roots stand for two machines. Every scenario drives the
public transfer and listener entry points, and a loopback endpoint counts the
polls it receives itself, so admission is witnessed by the server rather than
by a client-reported outcome. No real credential, endpoint, or message is
involved.
"""

from __future__ import annotations

import fcntl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import unittest
from urllib.parse import parse_qs, urlparse


REPO = Path(__file__).resolve().parents[2]
LISTENER = REPO / "scripts" / "telegram-listen.sh"
TRANSFER = REPO / "scripts" / "bot-reader-transfer.py"
EMPTY_BATCH = b'{"ok":true,"result":[]}'
SECRET = "SYNTHETICSECRETPART"


class CountingEndpoint:
    """Loopback endpoint that records which credential polled at which offset."""

    def __init__(self) -> None:
        self.guard = threading.Lock()
        self.polls: list[tuple[str, str | None]] = []
        endpoint = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                parsed = urlparse(self.path)
                offset = parse_qs(parsed.query).get("offset", [None])[0]
                with endpoint.guard:
                    endpoint.polls.append((parsed.path.split("/")[1], offset))
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(EMPTY_BATCH)))
                self.end_headers()
                self.wfile.write(EMPTY_BATCH)

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
        self.server.shutdown()
        self.server.server_close()


class Host:
    """One machine's isolated reader state and credential."""

    def __init__(self, base: Path, name: str, bot: str = "synthetic-4242") -> None:
        self.name = name
        self.root = base / name
        self.root.mkdir()
        (self.root / "bot.toml").write_text(f'bot_token = "{bot}:{SECRET}"\n')
        self.state = self.root / ".cosmon"

    @property
    def peer(self) -> str:
        return f"local:{self.root}"

    def journal(self) -> dict:
        return json.loads((self.state / "telegram-reader.json").read_text())

    def checkpoint(self) -> str:
        return (self.state / "telegram-offset").read_text().strip()


class TransferTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        base = Path(self.temporary.name)
        self.endpoint = CountingEndpoint()
        self.a = Host(base, "host-a")
        self.b = Host(base, "host-b")
        self.c = Host(base, "host-c")
        self.outputs: list[str] = []

    def tearDown(self) -> None:
        self.endpoint.close()
        self.temporary.cleanup()

    def env(self, host: Host, **extra: str) -> dict[str, str]:
        env = dict(os.environ)
        env.update(
            COSMON_BOT_READER_TEST_MODE="1",
            COSMON_BOT_READER_TEST_ROOT=str(host.root),
            COSMON_BOT_READER_TEST_ENDPOINT=self.endpoint.url,
        )
        env.update(extra)
        return env

    def run_transfer(
        self, host: Host, *args: str, stdin: dict | None = None, **extra: str
    ) -> tuple[int, dict]:
        completed = subprocess.run(
            ["python3", str(TRANSFER), *args],
            env=self.env(host, **extra),
            input=json.dumps(stdin).encode() if stdin is not None else b"",
            capture_output=True,
            timeout=60,
        )
        self.outputs.append(completed.stdout.decode() + completed.stderr.decode())
        lines = completed.stdout.decode().strip().splitlines()
        return completed.returncode, json.loads(lines[-1]) if lines else {}

    def listen(self, host: Host) -> dict:
        completed = subprocess.run(
            ["bash", str(LISTENER)], env=self.env(host), capture_output=True, timeout=60
        )
        self.outputs.append(completed.stdout.decode() + completed.stderr.decode())
        lines = completed.stdout.decode().strip().splitlines()
        return json.loads(lines[-1]) if lines else {}

    def ok(self, host: Host, *args: str, stdin: dict | None = None) -> dict:
        status, result = self.run_transfer(host, *args, stdin=stdin)
        self.assertEqual(status, 0, result)
        return result

    def enroll_source(self, checkpoint: int = 41) -> None:
        self.a.state.mkdir()
        (self.a.state / "telegram-offset").write_text(f"{checkpoint}\n")
        self.ok(self.a, "enroll", "--host-id", "host-a")

    def coordinate(self, source: Host, destination: Host, **extra: str) -> tuple[int, dict]:
        # The coordinator runs on the source host and reaches both peers.
        return self.run_transfer(
            source,
            "transfer",
            "--source", source.peer,
            "--destination", destination.peer,
            "--destination-host-id", destination.name,
            **extra,
        )

    def staged_offer(self) -> dict:
        """Prepare host-b, export from host-a, and stage the offer on host-b."""
        self.ok(self.b, "prepare", "--host-id", "host-b")
        manifest = self.ok(self.a, "export", "--destination", "host-b")["manifest"]
        self.ok(self.b, "import", stdin=manifest)
        return manifest

    def polls_by(self, offset: str | None = None) -> int:
        return sum(1 for _, seen in self.endpoint.polls if offset is None or seen == offset)

    def test_transfer_moves_the_checkpoint_and_leaves_one_reader(self) -> None:
        self.enroll_source(41)
        status, result = self.coordinate(self.a, self.b)
        self.assertEqual((status, result.get("outcome")), (0, "transferred"), result)

        self.assertEqual(self.listen(self.a).get("outcome"), "stopped")
        (self.a.state / "telegram-listen.off").unlink()
        self.assertEqual(self.listen(self.a).get("outcome"), "transfer_pending")
        self.assertEqual(self.polls_by(), 0)

        self.assertEqual(self.listen(self.b).get("outcome"), "empty")
        self.assertEqual(self.endpoint.polls, [(f"botsynthetic-4242:{SECRET}", "42")])
        self.assertEqual(self.b.checkpoint(), "41")
        self.assertEqual((self.b.journal()["phase"], self.b.journal()["epoch"]), ("active", 2))
        self.assertEqual((self.a.journal()["phase"], self.a.journal()["epoch"]), ("transferred", 2))

    def test_restarted_source_after_export_is_not_resurrected(self) -> None:
        self.enroll_source(41)
        self.ok(self.b, "prepare", "--host-id", "host-b")
        manifest = self.ok(self.a, "export", "--destination", "host-b")["manifest"]

        # A restart that clears the stop file, plus a plain copy of the offset
        # onto the destination: neither host may poll.
        (self.a.state / "telegram-listen.off").unlink()
        (self.b.state / "telegram-offset").write_text(f"{manifest['checkpoint']}\n")
        self.assertEqual(self.listen(self.a).get("outcome"), "transfer_pending")
        self.assertIn(self.listen(self.b).get("outcome"), ("stopped", "transfer_pending"))
        self.assertEqual(self.polls_by(), 0)

    def test_staged_manifest_without_receipt_cannot_activate(self) -> None:
        self.enroll_source()
        manifest = self.staged_offer()
        forged = {
            "transfer_id": manifest["transfer_id"],
            "epoch": manifest["epoch"],
            "digest": manifest["digest"],
            "commit_secret": "0" * 64,
        }
        status, result = self.run_transfer(self.b, "activate", stdin=forged)
        self.assertEqual((status, result.get("outcome")), (1, "receipt_mismatch"))
        self.assertEqual(self.b.journal()["phase"], "staged")
        self.assertIn(self.listen(self.b).get("outcome"), ("stopped", "transfer_pending"))
        self.assertEqual(self.polls_by(), 0)

    def test_cancelled_export_cannot_activate_the_destination(self) -> None:
        self.enroll_source()
        manifest = self.staged_offer()
        self.ok(self.a, "cancel", "--transfer-id", manifest["transfer_id"])

        status, result = self.run_transfer(self.a, "commit", stdin=manifest)
        self.assertEqual((status, result.get("outcome")), (1, "offer_mismatch"))
        self.assertEqual(self.a.journal()["phase"], "active")
        (self.b.state / "telegram-listen.off").unlink(missing_ok=True)
        self.assertEqual(self.listen(self.b).get("outcome"), "transfer_pending")
        self.assertEqual(self.listen(self.a).get("outcome"), "empty")
        self.assertEqual(self.polls_by(), 1)

    def test_wrong_destination_export_is_refused(self) -> None:
        self.enroll_source()
        self.ok(self.b, "prepare", "--host-id", "host-b")
        self.ok(self.c, "prepare", "--host-id", "host-c")
        manifest = self.ok(self.a, "export", "--destination", "host-b")["manifest"]

        status, result = self.run_transfer(self.c, "import", stdin=manifest)
        self.assertEqual((status, result.get("outcome")), (1, "wrong_destination"))
        redirected = dict(manifest, destination="host-c")
        status, result = self.run_transfer(self.c, "import", stdin=redirected)
        self.assertEqual((status, result.get("outcome")), (1, "invalid_manifest"))
        status, result = self.run_transfer(self.a, "commit", stdin=redirected)
        self.assertEqual((status, result.get("outcome")), (1, "invalid_manifest"))
        self.assertEqual(self.c.journal()["phase"], "standby")
        self.assertEqual(self.a.journal()["phase"], "offered")

    def test_commit_and_cancel_race_reaches_one_decision(self) -> None:
        for attempt in range(4):
            with self.subTest(attempt=attempt):
                self.tearDown()
                self.setUp()
                self.enroll_source()
                manifest = self.staged_offer()
                lock = os.open(self.a.state / "telegram-listen.lock", os.O_RDWR)
                fcntl.flock(lock, fcntl.LOCK_EX)
                racers = [
                    subprocess.Popen(
                        ["python3", str(TRANSFER), *args],
                        env=self.env(self.a),
                        stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE,
                    )
                    for args in (["commit"], ["cancel", "--transfer-id", manifest["transfer_id"]])
                ]
                racers[0].stdin.write(json.dumps(manifest).encode())
                racers[0].stdin.close()
                racers[1].stdin.close()
                time.sleep(0.3)
                self.assertEqual([racer.poll() for racer in racers], [None, None])
                os.close(lock)
                results = []
                for racer in racers:
                    stdout = racer.stdout.read()
                    racer.wait(timeout=30)
                    racer.stdout.close()
                    racer.stderr.close()
                    results.append((racer.returncode, json.loads(stdout.decode().splitlines()[-1])))
                winners = [result for status, result in results if status == 0]
                self.assertEqual(len(winners), 1, results)
                phase = self.a.journal()["phase"]
                if winners[0]["outcome"] == "committed":
                    self.assertEqual(phase, "transferred")
                    self.assertEqual(results[1][1]["outcome"], "already_committed")
                else:
                    self.assertEqual(phase, "active")
                    self.assertEqual(results[0][1]["outcome"], "offer_mismatch")

    def test_lost_commit_reply_resumes_to_one_reader(self) -> None:
        self.enroll_source(41)
        status, result = self.coordinate(
            self.a, self.b, COSMON_BOT_READER_TEST_INTERRUPT="after_commit"
        )
        self.assertEqual((status, result.get("outcome"), result.get("step")), (1, "interrupted", "commit"))
        self.assertEqual(self.a.journal()["phase"], "transferred")
        self.assertEqual(self.b.journal()["phase"], "staged")
        self.assertIn(self.listen(self.a).get("outcome"), ("stopped", "transfer_pending"))
        self.assertIn(self.listen(self.b).get("outcome"), ("stopped", "transfer_pending"))

        status, result = self.coordinate(self.a, self.b)
        self.assertEqual((status, result.get("outcome")), (0, "transferred"), result)
        # Repeating a completed transfer is idempotent and changes nothing.
        status, again = self.coordinate(self.a, self.b)
        self.assertEqual((status, again.get("outcome")), (0, "transferred"), again)
        self.assertEqual(again["transfer_id"], result["transfer_id"])
        self.assertEqual(self.listen(self.b).get("outcome"), "empty")
        self.assertEqual(self.endpoint.polls, [(f"botsynthetic-4242:{SECRET}", "42")])

    def test_crash_inside_activation_keeps_the_destination_inactive(self) -> None:
        self.enroll_source(41)
        status, result = self.coordinate(
            self.a, self.b, COSMON_BOT_READER_TEST_INTERRUPT="activate_before_journal"
        )
        self.assertEqual((status, result.get("step")), (1, "activate"))
        self.assertEqual(self.b.journal()["phase"], "staged")
        self.assertEqual(self.listen(self.b).get("outcome"), "transfer_pending")
        self.assertEqual(self.polls_by(), 0)
        status, result = self.coordinate(self.a, self.b)
        self.assertEqual((status, result.get("outcome")), (0, "transferred"), result)
        self.assertEqual(self.listen(self.b).get("outcome"), "empty")
        self.assertEqual(self.polls_by("42"), 1)

    def test_stale_manifest_and_receipt_are_refused_after_onward_transfer(self) -> None:
        self.enroll_source(41)
        first = self.ok(self.a, "transfer", "--source", self.a.peer, "--destination", self.b.peer,
                        "--destination-host-id", "host-b")
        status, second = self.coordinate(self.b, self.c)
        self.assertEqual((status, second.get("outcome")), (0, "transferred"), second)

        status, result = self.run_transfer(self.b, "import", stdin=first["manifest"])
        self.assertEqual((status, result.get("outcome")), (1, "stale_epoch"))
        status, result = self.run_transfer(self.b, "activate", stdin=first["receipt"])
        self.assertEqual((status, result.get("outcome")), (1, "receipt_mismatch"))
        (self.b.state / "telegram-listen.off").unlink(missing_ok=True)
        self.assertEqual(self.listen(self.b).get("outcome"), "transfer_pending")
        self.assertEqual(self.listen(self.c).get("outcome"), "empty")
        self.assertEqual(self.endpoint.polls, [(f"botsynthetic-4242:{SECRET}", "42")])
        self.assertEqual(self.c.journal()["epoch"], 3)

    def test_unavailable_source_never_promotes_the_destination(self) -> None:
        missing = Host(Path(self.temporary.name), "host-gone")
        status, result = self.coordinate(missing, self.b)
        self.assertEqual((status, result.get("step")), (1, "export"))
        self.assertEqual(self.b.journal()["phase"], "standby")
        self.assertIn(self.listen(self.b).get("outcome"), ("stopped", "transfer_pending"))
        self.assertEqual(self.polls_by(), 0)

    def test_missing_checkpoint_is_refused_and_not_reset(self) -> None:
        self.enroll_source(41)
        self.ok(self.b, "prepare", "--host-id", "host-b")
        (self.a.state / "telegram-offset").unlink()
        status, result = self.run_transfer(self.a, "export", "--destination", "host-b")
        self.assertEqual((status, result.get("outcome")), (1, "invalid_state"))
        self.assertFalse((self.a.state / "telegram-offset").exists())
        self.assertEqual(self.a.journal()["phase"], "fenced")

        (self.a.state / "telegram-offset").write_text("not a number\n")
        status, result = self.run_transfer(self.a, "export", "--destination", "host-b")
        self.assertEqual((status, result.get("outcome")), (1, "invalid_checkpoint"))
        self.assertEqual(self.a.journal()["phase"], "fenced")

    def test_nonempty_inbox_blocks_export_and_stays_intact(self) -> None:
        self.enroll_source()
        self.ok(self.b, "prepare", "--host-id", "host-b")
        inbox = self.a.state / "telegram-inbox"
        inbox.mkdir()
        (inbox / "41.json").write_text('{"update_id": 41}\n')
        status, result = self.run_transfer(self.a, "export", "--destination", "host-b")
        self.assertEqual((status, result.get("outcome")), (1, "inbox_not_empty"))
        self.assertEqual((inbox / "41.json").read_text(), '{"update_id": 41}\n')
        self.assertEqual(self.a.journal()["phase"], "fenced")

    def test_wrong_bot_is_refused_before_the_cursor_moves(self) -> None:
        other = Host(Path(self.temporary.name), "host-d", bot="synthetic-9999")
        self.enroll_source()
        self.ok(other, "prepare", "--host-id", "host-d")
        manifest = self.ok(self.a, "export", "--destination", "host-d")["manifest"]
        status, result = self.run_transfer(other, "import", stdin=manifest)
        self.assertEqual((status, result.get("outcome")), (1, "wrong_bot"))
        self.assertFalse((other.state / "telegram-offset").exists())

    def test_prepare_refuses_an_active_reader_without_stopping_it(self) -> None:
        self.enroll_source()
        status, result = self.run_transfer(self.a, "prepare", "--host-id", "host-a")
        self.assertEqual((status, result.get("outcome")), (1, "destination_not_inactive"))
        self.assertFalse((self.a.state / "telegram-listen.off").exists())
        self.assertEqual(self.listen(self.a).get("outcome"), "empty")

    def test_credentials_never_reach_manifests_journals_or_diagnostics(self) -> None:
        self.enroll_source()
        status, result = self.coordinate(self.a, self.b)
        self.assertEqual(status, 0, result)
        self.assertNotIn(SECRET, json.dumps(result))
        for host in (self.a, self.b):
            self.assertNotIn(SECRET, (host.state / "telegram-reader.json").read_text())
        for output in self.outputs:
            self.assertNotIn(SECRET, output)


if __name__ == "__main__":
    unittest.main()
