#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Prove the two-host bot-reader handover against an independent server trace.

Each scenario gives two or three temporary host roots their own scheduler-like
loop that keeps invoking the public listener, moves ownership with the public
transfer entry point, and lets one loopback ``FakeBotApi`` witness what
happened: which host polled, at which offset, how many requests overlapped,
and which updates the readers acknowledged. A client-reported outcome is never
the only evidence. No real credential, endpoint, or message is involved.

Set ``BOT_READER_HANDOVER_TRACE`` to a directory to keep each scenario's
server trace as JSON, whether the scenario passed or failed.
"""

from __future__ import annotations

from dataclasses import asdict
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from bot_reader_fake_endpoint import FakeBotApi  # noqa: E402


REPO = Path(__file__).resolve().parents[2]
LISTENER = REPO / "scripts" / "telegram-listen.sh"
TRANSFER = REPO / "scripts" / "bot-reader-transfer.py"
FOREIGN_CHAT = 555
QUIET_WINDOW = 0.6

SSH_SHIM = """#!/usr/bin/env bash
# Stand-in for ssh: maps an alias to a host root; a verb listed in
# <alias>.down is refused as an unreachable host would be.
while [ "$1" != "--" ]; do shift; done
shift
alias="$1"; shift
verb="$3"
[ -d "$FAKE_SSH_HOSTS/$alias" ] || { echo "ssh: $alias: refused" >&2; exit 255; }
if [ -f "$FAKE_SSH_HOSTS/$alias.down" ] && grep -qx "$verb" "$FAKE_SSH_HOSTS/$alias.down"; then
  echo "ssh: $alias: refused" >&2
  exit 255
fi
exec env -i PATH="$PATH" HOME="$HOME" \\
  COSMON_BOT_READER_TEST_MODE=1 COSMON_BOT_READER_TEST_ROOT="$FAKE_SSH_HOSTS/$alias" "$@"
"""


def wait_until(predicate, timeout: float = 15.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.01)
    return predicate()


def last_json(stdout: bytes) -> dict:
    lines = stdout.decode().strip().splitlines()
    try:
        return json.loads(lines[-1]) if lines else {}
    except json.JSONDecodeError:
        return {}


class Host:
    """One machine: its own state root and its own copy of the bot credential.

    Both hosts hold the same bot identity; only the part after the colon
    differs, so the server can tell which machine sent a request.
    """

    def __init__(self, base: Path, name: str) -> None:
        self.name = name
        self.root = base / name
        self.state = self.root / ".cosmon"
        self.state.mkdir(parents=True)
        self.credential = "SYNTHETIC" + name.replace("-", "").upper()
        (self.root / "bot.toml").write_text(f'bot_token = "synthetic-4242:{self.credential}"\n')
        self.archive = base / f"{name}-archive"

    @property
    def peer(self) -> str:
        return f"local:{self.root}"

    def journal(self) -> dict:
        return json.loads((self.state / "telegram-reader.json").read_text())

    def checkpoint(self) -> int | None:
        try:
            return int((self.state / "telegram-offset").read_text().strip())
        except (FileNotFoundError, ValueError):
            return None

    def inbox(self) -> set[int]:
        return {int(path.stem) for path in (self.state / "telegram-inbox").glob("*.json")}

    def captured(self) -> set[int]:
        """Updates this host has durably captured, drained or not."""
        archived = {int(path.stem) for path in self.archive.glob("*.json")}
        return self.inbox() | archived

    def drain(self) -> None:
        """Stand in for the downstream consumer archiving handled records."""
        self.archive.mkdir(exist_ok=True)
        for path in (self.state / "telegram-inbox").glob("*.json"):
            shutil.move(str(path), self.archive / path.name)


class ReaderLoop(threading.Thread):
    """Invoke the listener again and again, as the scheduler does."""

    def __init__(self, env: dict[str, str], interval: float = 0.02) -> None:
        super().__init__(daemon=True)
        self.env = env
        self.interval = interval
        self.halt = threading.Event()
        self.outcomes: list[str] = []

    def run(self) -> None:
        while not self.halt.is_set():
            completed = subprocess.run(
                ["bash", str(LISTENER)], env=self.env, capture_output=True, timeout=60
            )
            self.outcomes.append(last_json(completed.stdout).get("outcome", "no_outcome"))
            self.halt.wait(self.interval)

    def stop(self) -> None:
        self.halt.set()
        self.join(timeout=60)


class HandoverTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.base = Path(self.temporary.name)
        self.api = FakeBotApi()
        self.a = Host(self.base, "host-a")
        self.b = Host(self.base, "host-b")
        self.c = Host(self.base, "host-c")
        self.filtered: set[int] = set()
        self.loops: dict[str, ReaderLoop] = {}
        self.processes: list[subprocess.Popen] = []

    def tearDown(self) -> None:
        for loop in self.loops.values():
            loop.halt.set()
        self.api.close()
        if os.environ.get("BOT_READER_HANDOVER_TRACE"):
            trace = {
                "max_concurrent": self.api.max_concurrent,
                "acknowledged": self.api.acknowledged,
                "polls": [asdict(poll) for poll in self.api.trace()],
            }
            target = Path(os.environ["BOT_READER_HANDOVER_TRACE"]) / f"{self._testMethodName}.json"
            target.write_text(json.dumps(trace, indent=1) + "\n")
        for loop in self.loops.values():
            loop.join(timeout=60)
        for process in self.processes:
            if process.poll() is None:
                process.kill()
            process.communicate()
        self.temporary.cleanup()

    # -- drivers -----------------------------------------------------------

    def env(self, host: Host, **extra: str) -> dict[str, str]:
        env = dict(os.environ)
        env.update(
            COSMON_BOT_READER_TEST_MODE="1",
            COSMON_BOT_READER_TEST_ROOT=str(host.root),
            COSMON_BOT_READER_TEST_ENDPOINT=self.api.url,
        )
        env.update(extra)
        return env

    def run_transfer(self, host: Host, *args: str, **extra: str) -> tuple[int, dict]:
        completed = subprocess.run(
            [sys.executable, str(TRANSFER), *args],
            env=self.env(host, **extra),
            capture_output=True,
            timeout=120,
        )
        return completed.returncode, last_json(completed.stdout)

    def coordinate(self, destination: Host, **extra: str) -> tuple[int, dict]:
        return self.run_transfer(
            self.a,
            "transfer",
            "--source", self.a.peer,
            "--destination", destination.peer,
            "--destination-host-id", destination.name,
            **extra,
        )

    def transferred(self, destination: Host) -> dict:
        status, result = self.coordinate(destination)
        self.assertEqual((status, result.get("outcome")), (0, "transferred"), result)
        return result

    def enroll_source(self, checkpoint: int = 41) -> None:
        (self.a.state / "telegram-offset").write_text(f"{checkpoint}\n")
        status, result = self.run_transfer(self.a, "enroll", "--host-id", self.a.name)
        self.assertEqual((status, result.get("outcome")), (0, "enrolled"), result)

    def start_loops(self, *hosts: Host) -> None:
        for host in hosts:
            loop = ReaderLoop(self.env(host))
            self.loops[host.name] = loop
            loop.start()

    def restart_loop(self, host: Host) -> None:
        self.loops.pop(host.name).stop()
        self.start_loops(host)

    def listen(self, host: Host, **extra: str) -> subprocess.Popen:
        process = subprocess.Popen(
            ["bash", str(LISTENER)],
            env=self.env(host, **extra),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.processes.append(process)
        return process

    def finish(self, process: subprocess.Popen) -> tuple[int, dict]:
        stdout, _ = process.communicate(timeout=60)
        return process.returncode, last_json(stdout)

    def add_filtered(self, update_id: int) -> None:
        self.filtered.add(update_id)
        self.api.add(update_id, chat=FOREIGN_CHAT)

    def polls(self, host: Host) -> list:
        return self.api.polls_by(host.credential)

    def quiet_poll_count(self) -> int:
        """Let any request already in flight finish, then count requests."""
        wait_until(lambda: self.api.active == 0, timeout=5)
        return len(self.api.trace())

    # -- witnesses ---------------------------------------------------------

    def assert_single_reader(self, *hosts: Host) -> None:
        """The server's own trace: one request at a time, every ack backed.

        An acknowledged update must have been durably captured by exactly one
        host or deliberately filtered by all of them; a request never confirms
        an update nobody kept.
        """
        self.assertLessEqual(self.api.max_concurrent, 1, "two polls overlapped at the server")
        delivered = {update for poll in self.api.trace() for update in poll.delivered}
        for update in sorted(delivered):
            keepers = [host.name for host in hosts if update in host.captured()]
            if update in self.filtered:
                self.assertEqual(keepers, [], f"filtered update {update} was captured")
                continue
            if update <= self.api.acknowledged:
                self.assertEqual(len(keepers), 1, f"update {update} acknowledged; kept by {keepers}")
            else:
                self.assertLessEqual(len(keepers), 1, f"update {update} captured twice: {keepers}")

    def assert_handover_order(self, source: Host, destination: Host) -> None:
        """Every source request ended before the destination's first began."""
        source_polls = self.polls(source)
        destination_polls = self.polls(destination)
        self.assertTrue(destination_polls, "destination never polled")
        if source_polls:
            last_source = max(poll.finished or float("inf") for poll in source_polls)
            self.assertLessEqual(last_source, destination_polls[0].started)

    # -- scenarios ---------------------------------------------------------

    def test_handover_moves_one_reader_and_captures_each_update_once(self) -> None:
        n = 100
        self.enroll_source(checkpoint=n - 1)
        # A destination that once ran a reader keeps a stale legacy cursor;
        # only the imported checkpoint may decide where it resumes.
        (self.b.state / "telegram-offset").write_text("7\n")
        self.api.add(n)
        self.start_loops(self.a, self.b)
        self.assertTrue(wait_until(lambda: n in self.a.inbox() and self.a.checkpoint() == n))

        # Hold the source's next request in flight, then fence the source.
        hold = self.api.hold_next()
        self.assertTrue(wait_until(hold.arrived.is_set))
        self.assertEqual(hold.offset, n + 1)
        self.api.add(n + 1)
        fence = subprocess.Popen(
            [sys.executable, str(TRANSFER), "fence"],
            env=self.env(self.a),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.processes.append(fence)
        self.assertTrue(wait_until((self.a.state / "telegram-listen.off").exists))
        time.sleep(0.3)
        self.assertIsNone(fence.poll(), "fence returned while a request was in flight")
        hold.released.set()
        status, result = self.finish(fence)
        self.assertEqual((status, result.get("outcome"), result.get("checkpoint")), (0, "fenced", n + 1))
        self.assertEqual(self.a.inbox(), {n, n + 1})

        # Captured work still in the source inbox blocks the export.
        status, result = self.coordinate(self.b)
        self.assertEqual((status, result.get("step"), result.get("outcome")), (1, "export", "inbox_not_empty"))
        self.assertEqual(self.a.inbox(), {n, n + 1})
        self.assertEqual(self.a.journal()["phase"], "fenced")

        self.a.drain()
        result = self.transferred(self.b)
        self.assertEqual((result["checkpoint"], result["epoch"]), (n + 1, 2))

        self.api.add(n + 2)
        self.add_filtered(n + 3)
        self.api.add(n + 4)
        self.assertTrue(wait_until(lambda: self.b.checkpoint() == n + 4))
        self.assertTrue(wait_until(lambda: self.api.acknowledged >= n + 4))
        for loop in self.loops.values():
            loop.stop()

        trace = self.api.trace()
        self.assertLessEqual(self.api.max_concurrent, 1, "two polls overlapped at the server")
        a_polls, b_polls = self.polls(self.a), self.polls(self.b)
        self.assertEqual(b_polls[0].offset, n + 2, "destination did not resume at N+2")
        self.assertLessEqual({poll.offset for poll in a_polls}, {n, n + 1})
        self.assertEqual(self.b.inbox(), {n + 2, n + 4})
        self.assertEqual(self.a.captured(), {n, n + 1})
        self.assertTrue(any(poll.status == 200 and n + 4 in poll.delivered for poll in b_polls))
        self.assertEqual({poll.credential for poll in trace}, {self.a.credential, self.b.credential})
        self.assert_handover_order(self.a, self.b)
        self.assert_single_reader(self.a, self.b)

    def test_delayed_launches_poll_only_for_the_current_owner(self) -> None:
        self.enroll_source()
        pause_a, pause_b = self.base / "pause-a", self.base / "pause-b"
        late_a = self.listen(self.a, COSMON_BOT_READER_TEST_PAUSE=str(pause_a))
        late_b = self.listen(self.b, COSMON_BOT_READER_TEST_PAUSE=str(pause_b))
        self.assertTrue(wait_until(Path(f"{pause_a}.waiting").exists))
        self.assertTrue(wait_until(Path(f"{pause_b}.waiting").exists))
        # Both launches passed their wrapper's stop check before the transfer.
        self.transferred(self.b)
        pause_a.write_text("release\n")
        pause_b.write_text("release\n")
        self.assertIn(self.finish(late_a)[1].get("outcome"), ("stopped", "transfer_pending"))
        self.assertEqual(self.finish(late_b)[1].get("outcome"), "empty")
        self.assertEqual(self.polls(self.a), [])
        self.assertEqual([poll.offset for poll in self.polls(self.b)], [42])
        self.assert_single_reader(self.a, self.b)

    def test_restarted_source_stays_fenced_during_and_after_transfer(self) -> None:
        self.enroll_source()
        status, result = self.run_transfer(self.b, "prepare", "--host-id", self.b.name)
        self.assertEqual(status, 0, result)
        status, result = self.run_transfer(self.a, "export", "--destination", self.b.name)
        self.assertEqual((status, result.get("outcome")), (0, "offered"), result)

        # A restart that clears the stop file must not bring the source back.
        (self.a.state / "telegram-listen.off").unlink()
        self.start_loops(self.a, self.b)
        time.sleep(QUIET_WINDOW)
        self.assertEqual(self.api.trace(), [])
        self.assertIn("transfer_pending", self.loops[self.a.name].outcomes)

        self.transferred(self.b)
        self.api.add(42)
        self.assertTrue(wait_until(lambda: 42 in self.b.inbox()))
        (self.a.state / "telegram-listen.off").unlink(missing_ok=True)
        self.restart_loop(self.a)
        time.sleep(QUIET_WINDOW)
        self.assertEqual(self.polls(self.a), [])
        self.assertEqual(self.polls(self.b)[0].offset, 42)
        self.assert_single_reader(self.a, self.b)

    def test_restarted_destination_resumes_without_replay(self) -> None:
        self.enroll_source()
        self.start_loops(self.a, self.b)
        status, result = self.coordinate(self.b, COSMON_BOT_READER_TEST_INTERRUPT="activate_before_journal")
        self.assertEqual((status, result.get("step")), (1, "activate"), result)
        settled = self.quiet_poll_count()

        # Crash inside activation, then restart: the destination stays out.
        self.restart_loop(self.b)
        time.sleep(QUIET_WINDOW)
        self.assertEqual(len(self.api.trace()), settled)
        self.assertEqual(self.polls(self.b), [])
        self.assertEqual(self.b.journal()["phase"], "staged")

        self.transferred(self.b)
        self.api.add(42)
        self.assertTrue(wait_until(lambda: self.b.checkpoint() == 42))
        self.restart_loop(self.b)
        self.api.add(43)
        self.assertTrue(wait_until(lambda: self.b.checkpoint() == 43))
        self.assertTrue(wait_until(lambda: self.api.acknowledged >= 43))
        offsets = [poll.offset for poll in self.polls(self.b)]
        self.assertEqual(offsets[0], 42)
        self.assertEqual(offsets, sorted(offsets), "destination replayed after restart")
        self.assertEqual(self.b.inbox(), {42, 43})
        self.assert_handover_order(self.a, self.b)
        self.assert_single_reader(self.a, self.b)

    def test_concurrent_coordinators_hand_ownership_to_one_destination(self) -> None:
        self.enroll_source()
        self.start_loops(self.a, self.b, self.c)
        attempts = {
            host.name: subprocess.Popen(
                [
                    sys.executable, str(TRANSFER), "transfer",
                    "--source", self.a.peer,
                    "--destination", host.peer,
                    "--destination-host-id", host.name,
                ],
                env=self.env(self.a),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            for host in (self.b, self.c)
        }
        self.processes.extend(attempts.values())
        results = {name: self.finish(process) for name, process in attempts.items()}
        winners = [name for name, (status, result) in results.items() if status == 0]
        self.assertEqual(len(winners), 1, results)
        winner, loser = (self.b, self.c) if winners == [self.b.name] else (self.c, self.b)
        self.assertEqual(results[winner.name][1].get("outcome"), "transferred")

        self.api.add(42)
        self.assertTrue(wait_until(lambda: 42 in winner.inbox()))
        time.sleep(QUIET_WINDOW)
        self.assertEqual(self.polls(loser), [])
        self.assertNotEqual(loser.journal()["phase"], "active")
        self.assert_handover_order(self.a, winner)
        self.assert_single_reader(self.a, self.b, self.c)

    def test_api_conflict_neither_captures_nor_advances(self) -> None:
        self.enroll_source()
        self.transferred(self.b)
        self.api.conflict = True
        self.api.add(42)
        status, result = self.finish(self.listen(self.b))
        self.assertEqual((status, result.get("outcome")), (1, "api_conflict"))
        self.assertEqual(self.b.checkpoint(), 41)
        self.assertEqual(self.b.inbox(), set())
        self.assertLess(self.api.acknowledged, 42)
        self.assertEqual([poll.status for poll in self.polls(self.b)], [409])

        self.api.conflict = False
        status, result = self.finish(self.listen(self.b))
        self.assertEqual((status, result.get("outcome")), (0, "captured"))
        self.assertEqual(self.b.inbox(), {42})
        self.assert_single_reader(self.a, self.b)

    @unittest.expectedFailure
    def test_api_conflict_stops_polling_until_reconciled(self) -> None:
        # Plan §2 listener prerequisite 6: a 409 means another consumer holds
        # the credential, so polling stops until an explicit reconciliation.
        # The W1 listener reports the conflict but polls again on the next
        # tick; this records that gap until W1's helper latches the stop.
        self.enroll_source()
        self.transferred(self.b)
        self.api.conflict = True
        self.finish(self.listen(self.b))
        self.api.conflict = False
        self.finish(self.listen(self.b))
        self.assertEqual(len(self.polls(self.b)), 1, "polled again after a 409")

    def test_dropped_commit_reply_leaves_both_stopped_until_resumed(self) -> None:
        self.enroll_source()
        self.start_loops(self.a, self.b)
        status, result = self.coordinate(self.b, COSMON_BOT_READER_TEST_INTERRUPT="after_commit")
        self.assertEqual((status, result.get("step")), (1, "commit"), result)
        settled = self.quiet_poll_count()
        time.sleep(QUIET_WINDOW)
        self.assertEqual(len(self.api.trace()), settled, "a reader polled with the reply lost")
        self.assertEqual((self.a.journal()["phase"], self.b.journal()["phase"]), ("transferred", "staged"))

        self.transferred(self.b)
        self.api.add(42)
        self.assertTrue(wait_until(lambda: 42 in self.b.inbox()))
        self.assertEqual(self.polls(self.b)[0].offset, 42)
        self.assert_handover_order(self.a, self.b)
        self.assert_single_reader(self.a, self.b)

    def test_corrupt_source_state_admits_no_reader(self) -> None:
        self.enroll_source()
        self.start_loops(self.a, self.b)
        self.assertTrue(wait_until(lambda: len(self.polls(self.a)) >= 1))
        (self.a.state / "telegram-reader.json").write_text("{not json")
        settled = self.quiet_poll_count()
        time.sleep(QUIET_WINDOW)
        self.assertEqual(len(self.api.trace()), settled)
        self.assertIn("invalid_state", self.loops[self.a.name].outcomes)

        status, result = self.coordinate(self.b)
        self.assertEqual((status, result.get("step"), result.get("outcome")), (1, "export", "invalid_state"))
        time.sleep(QUIET_WINDOW)
        self.assertEqual(len(self.api.trace()), settled)
        self.assertEqual(self.polls(self.b), [])
        self.assertEqual(self.b.journal()["phase"], "standby")

    def test_unreachable_source_never_promotes_the_destination(self) -> None:
        self.enroll_source()
        shim = self.base / "bin"
        shim.mkdir()
        (shim / "ssh").write_text(SSH_SHIM)
        (shim / "ssh").chmod(0o755)
        down = self.base / f"{self.a.name}.down"
        ssh_env = {"PATH": f"{shim}:{os.environ['PATH']}", "FAKE_SSH_HOSTS": str(self.base)}

        def over_ssh() -> tuple[int, dict]:
            return self.run_transfer(
                self.a,
                "transfer",
                "--source", f"ssh:{self.a.name}",
                "--destination", f"ssh:{self.b.name}",
                "--destination-host-id", self.b.name,
                "--remote-script", str(TRANSFER),
                **ssh_env,
            )

        self.start_loops(self.a, self.b)
        # Unreachable before the fence: the source simply stays the owner.
        down.write_text("export\ncommit\n")
        status, result = over_ssh()
        self.assertEqual((status, result.get("step"), result.get("outcome")), (1, "export", "transport_failure"))
        self.api.add(42)
        self.assertTrue(wait_until(lambda: 42 in self.a.inbox()))
        self.a.drain()

        # Unreachable after the offer: both stay stopped, nobody is promoted.
        down.write_text("commit\n")
        status, result = over_ssh()
        self.assertEqual((status, result.get("step"), result.get("outcome")), (1, "commit", "transport_failure"))
        settled = self.quiet_poll_count()
        time.sleep(QUIET_WINDOW)
        self.assertEqual(len(self.api.trace()), settled)
        self.assertEqual(self.polls(self.b), [])
        self.assertEqual((self.a.journal()["phase"], self.b.journal()["phase"]), ("offered", "staged"))

        down.unlink()
        status, result = over_ssh()
        self.assertEqual((status, result.get("outcome")), (0, "transferred"), result)
        self.api.add(43)
        self.assertTrue(wait_until(lambda: 43 in self.b.inbox()))
        self.assertEqual(self.polls(self.b)[0].offset, 43)
        self.assert_handover_order(self.a, self.b)
        self.assert_single_reader(self.a, self.b)


if __name__ == "__main__":
    unittest.main()
