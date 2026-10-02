#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Prove that one host admits at most one bot-reader poll at a time.

Every scenario drives the public listener entry point against an in-process
loopback endpoint that holds requests at a barrier and counts concurrent
admissions itself, so a client-reported outcome is never the only witness.
No real credential, endpoint, or message is involved.
"""

from __future__ import annotations

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
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


class BarrierEndpoint:
    """Loopback endpoint that holds each poll until released."""

    def __init__(self) -> None:
        self.guard = threading.Lock()
        self.active = 0
        self.max_concurrent = 0
        self.offsets: list[str | None] = []
        self.arrived = threading.Event()
        self.release = threading.Event()
        endpoint = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                query = parse_qs(urlparse(self.path).query)
                with endpoint.guard:
                    endpoint.active += 1
                    endpoint.max_concurrent = max(endpoint.max_concurrent, endpoint.active)
                    endpoint.offsets.append(query.get("offset", [None])[0])
                endpoint.arrived.set()
                endpoint.release.wait(timeout=20)
                try:
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(EMPTY_BATCH)))
                    self.end_headers()
                    self.wfile.write(EMPTY_BATCH)
                except OSError:
                    pass
                finally:
                    with endpoint.guard:
                        endpoint.active -= 1

            def log_message(self, _format: str, *args: object) -> None:
                return

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.server.server_port}"

    @property
    def requests(self) -> int:
        with self.guard:
            return len(self.offsets)

    def close(self) -> None:
        self.release.set()
        self.server.shutdown()
        self.server.server_close()


def wait_until(predicate, timeout: float = 10.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.01)
    return predicate()


class FencingTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        (self.root / "bot.toml").write_text('bot_token = "synthetic-fencing"\n')
        self.state = self.root / ".cosmon"
        self.endpoint = BarrierEndpoint()
        self.processes: list[subprocess.Popen] = []

    def tearDown(self) -> None:
        self.endpoint.close()
        for process in self.processes:
            if process.poll() is None:
                process.kill()
            process.communicate()
        self.temporary.cleanup()

    def env(self, **extra: str) -> dict[str, str]:
        env = dict(os.environ)
        env.update(
            COSMON_BOT_READER_TEST_MODE="1",
            COSMON_BOT_READER_TEST_ROOT=str(self.root),
            COSMON_BOT_READER_TEST_ENDPOINT=self.endpoint.url,
        )
        env.update(extra)
        return env

    def start_listener(self, **extra: str) -> subprocess.Popen:
        process = subprocess.Popen(
            ["bash", str(LISTENER)],
            env=self.env(**extra),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.processes.append(process)
        return process

    def finish(self, process: subprocess.Popen) -> tuple[int, dict]:
        stdout, _ = process.communicate(timeout=30)
        lines = stdout.decode().strip().splitlines()
        return process.returncode, json.loads(lines[-1]) if lines else {}

    def transfer(self, *args: str) -> tuple[int, dict]:
        completed = subprocess.run(
            ["python3", str(TRANSFER), *args],
            env=self.env(),
            capture_output=True,
            timeout=30,
        )
        lines = completed.stdout.decode().strip().splitlines()
        return completed.returncode, json.loads(lines[-1]) if lines else {}

    def enroll(self) -> None:
        status, result = self.transfer("enroll", "--host-id", "host-a")
        self.assertEqual((status, result.get("outcome")), (0, "enrolled"))

    def journal(self) -> dict:
        return json.loads((self.state / "telegram-reader.json").read_text())

    def test_overlapping_invocations_admit_one_request(self) -> None:
        self.enroll()
        first = self.start_listener()
        self.assertTrue(wait_until(self.endpoint.arrived.is_set))
        second = self.start_listener()
        # Give the second invocation every chance to reach the network.
        time.sleep(1.0)
        self.assertEqual(self.endpoint.max_concurrent, 1)
        self.endpoint.release.set()
        self.finish(first)
        self.finish(second)
        self.assertEqual(self.endpoint.max_concurrent, 1)

    def test_fence_between_stop_check_and_admission_admits_no_poll(self) -> None:
        self.enroll()
        pause = self.root / "pause"
        listener = self.start_listener(COSMON_BOT_READER_TEST_PAUSE=str(pause))
        self.assertTrue(wait_until(Path(f"{pause}.waiting").exists))
        (self.state / "telegram-listen.off").write_text("fence\n")
        pause.write_text("release\n")
        self.endpoint.release.set()
        status, result = self.finish(listener)
        self.assertEqual(self.endpoint.requests, 0)
        self.assertEqual((status, result.get("outcome")), (0, "stopped"))

    def test_queued_invocation_rereads_ownership_after_fence(self) -> None:
        self.enroll()
        first = self.start_listener()
        self.assertTrue(wait_until(self.endpoint.arrived.is_set))
        queued = self.start_listener()
        fence = subprocess.Popen(
            ["python3", str(TRANSFER), "fence"],
            env=self.env(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.processes.append(fence)
        self.assertTrue(wait_until((self.state / "telegram-listen.off").exists))
        # The fence is waiting for the admitted poll to drain.
        time.sleep(0.3)
        self.assertIsNone(fence.poll())
        self.endpoint.release.set()
        self.assertEqual(self.finish(first)[1].get("outcome"), "empty")
        fence_status, fence_result = self.finish(fence)
        self.assertEqual((fence_status, fence_result.get("outcome")), (0, "fenced"))
        self.assertIn(self.finish(queued)[1].get("outcome"), ("stopped", "transfer_pending"))
        self.assertEqual(self.endpoint.requests, 1)
        self.assertEqual(self.journal()["phase"], "fenced")

    def test_fenced_record_survives_stop_file_removal(self) -> None:
        self.enroll()
        status, result = self.transfer("fence")
        self.assertEqual((status, result.get("outcome")), (0, "fenced"))
        (self.state / "telegram-listen.off").unlink()
        self.endpoint.release.set()
        status, result = self.finish(self.start_listener())
        self.assertEqual((status, result.get("outcome")), (0, "transfer_pending"))
        self.assertEqual(self.endpoint.requests, 0)

    def test_missing_or_corrupt_state_refuses_admission(self) -> None:
        self.endpoint.release.set()
        status, result = self.finish(self.start_listener())
        self.assertEqual((status, result.get("outcome")), (1, "not_enrolled"))

        self.enroll()
        journal = self.state / "telegram-reader.json"
        original = journal.read_text()
        journal.write_text("{not json")
        status, result = self.finish(self.start_listener())
        self.assertEqual((status, result.get("outcome")), (1, "invalid_state"))

        journal.write_text(original)
        (self.state / "telegram-offset").unlink()
        status, result = self.finish(self.start_listener())
        self.assertEqual((status, result.get("outcome")), (1, "invalid_state"))
        self.assertFalse((self.state / "telegram-offset").exists())
        self.assertEqual(self.endpoint.requests, 0)

    def test_killed_reader_leaves_no_network_child_and_blocks_fence(self) -> None:
        self.enroll()
        listener = self.start_listener()
        self.assertTrue(wait_until(self.endpoint.arrived.is_set))
        children = subprocess.run(
            ["pgrep", "-P", str(listener.pid)], capture_output=True, text=True
        )
        self.assertEqual(children.stdout.strip(), "", "network request runs in a child")
        listener.send_signal(signal.SIGKILL)
        listener.wait()

        status, result = self.transfer("status")
        self.assertEqual(status, 0)
        self.assertEqual(result["lock"], "free")
        self.assertTrue(result["in_flight"])

        status, result = self.transfer("fence")
        self.assertEqual((status, result.get("outcome")), (1, "indeterminate_in_flight"))
        self.assertEqual(self.journal()["phase"], "fenced")
        self.assertTrue((self.state / "telegram-listen.off").exists())

    def test_status_reads_cause_no_network_call_or_write(self) -> None:
        self.enroll()
        before = sorted(
            (str(path), path.stat().st_mtime_ns) for path in self.state.rglob("*")
        )
        status, result = self.transfer("status")
        after = sorted(
            (str(path), path.stat().st_mtime_ns) for path in self.state.rglob("*")
        )
        self.assertEqual(status, 0)
        self.assertEqual(result["phase"], "active")
        self.assertEqual(before, after)
        self.assertEqual(self.endpoint.requests, 0)

    def test_existing_manual_stop_survives_fence(self) -> None:
        self.enroll()
        stop = self.state / "telegram-listen.off"
        stop.write_text("manual\n")
        status, result = self.transfer("fence")
        self.assertEqual((status, result.get("outcome")), (0, "fenced"))
        self.assertEqual(stop.read_text(), "manual\n")
        self.assertTrue(self.journal()["fence"]["stop_preexisting"])

    def test_enrollment_refuses_a_damaged_checkpoint(self) -> None:
        self.state.mkdir()
        (self.state / "telegram-offset").write_text("garbage\n")
        status, result = self.transfer("enroll", "--host-id", "host-a")
        self.assertEqual((status, result.get("outcome")), (1, "invalid_checkpoint"))
        self.assertFalse((self.state / "telegram-reader.json").exists())


if __name__ == "__main__":
    unittest.main()
