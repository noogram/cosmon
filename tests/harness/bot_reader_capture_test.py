#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Exercise durable capture ordering without network or private messages."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


MODULE_PATH = Path(__file__).resolve().parents[2] / "scripts" / "bot_reader.py"
SPEC = importlib.util.spec_from_file_location("bot_reader", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
bot_reader = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bot_reader)


def response(updates: list[dict]) -> bytes:
    return json.dumps({"ok": True, "result": updates}).encode()


def update(update_id: int, chat: int = 100000000, text: str = "synthetic") -> dict:
    return {
        "update_id": update_id,
        "message": {
            "chat": {"id": chat},
            "date": 1,
            "from": {"first_name": "Operator", "is_bot": False},
            "text": text,
        },
    }


class CaptureTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.inbox = self.root / "inbox"
        self.checkpoint = self.root / "offset"

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def capture(self, body: bytes, failpoint: str | None = None) -> dict:
        return bot_reader.capture_response(
            body, "100000000", self.inbox, self.checkpoint, failpoint
        )

    def test_valid_empty_filtered_and_retry(self) -> None:
        result = self.capture(response([update(10)]))
        self.assertEqual(result["outcome"], "captured")
        first = (self.inbox / "10.json").read_bytes()
        self.assertEqual(self.checkpoint.read_text(), "10\n")
        self.capture(response([update(10)]))
        self.assertEqual((self.inbox / "10.json").read_bytes(), first)

        self.assertEqual(self.capture(response([]))["outcome"], "empty")
        filtered = self.capture(response([update(11, chat=9)]))
        self.assertEqual(filtered["outcome"], "filtered")
        self.assertFalse((self.inbox / "11.json").exists())
        self.assertEqual(self.checkpoint.read_text(), "11\n")

    def test_malformed_and_api_failures_do_not_advance(self) -> None:
        self.checkpoint.write_text("7\n")
        cases = [
            (b"not-json", "malformed_response"),
            (b'{"ok":false,"error_code":409}', "api_conflict"),
            (b'{"ok":false,"error_code":401}', "credential_failure"),
        ]
        for body, outcome in cases:
            with self.subTest(outcome=outcome):
                with self.assertRaises(bot_reader.CaptureError) as raised:
                    self.capture(body)
                self.assertEqual(raised.exception.outcome, outcome)
                self.assertEqual(self.checkpoint.read_text(), "7\n")

    def test_interrupted_inbox_publish_leaves_no_partial_record(self) -> None:
        with self.assertRaises(bot_reader.CaptureError):
            self.capture(response([update(8)]), "inbox_before_publish")
        self.assertFalse((self.inbox / "8.json").exists())
        self.assertFalse(self.checkpoint.exists())

    def test_interrupted_checkpoint_preserves_old_value_and_retry_recovers(self) -> None:
        self.checkpoint.write_text("7\n")
        with self.assertRaises(bot_reader.CaptureError):
            self.capture(response([update(8)]), "cursor_before_replace")
        self.assertEqual(self.checkpoint.read_text(), "7\n")
        self.assertTrue((self.inbox / "8.json").exists())
        self.capture(response([update(8)]))
        self.assertEqual(self.checkpoint.read_text(), "8\n")

    def test_conflicting_record_does_not_advance(self) -> None:
        self.inbox.mkdir()
        (self.inbox / "8.json").write_text("conflict")
        self.checkpoint.write_text("7\n")
        with self.assertRaises(bot_reader.CaptureError) as raised:
            self.capture(response([update(8)]))
        self.assertEqual(raised.exception.outcome, "record_conflict")
        self.assertEqual(self.checkpoint.read_text(), "7\n")

    def test_disk_failure_does_not_advance(self) -> None:
        self.inbox.write_text("not a directory")
        self.checkpoint.write_text("7\n")
        with self.assertRaises(OSError):
            self.capture(response([update(8)]))
        self.assertEqual(self.checkpoint.read_text(), "7\n")


if __name__ == "__main__":
    unittest.main()
