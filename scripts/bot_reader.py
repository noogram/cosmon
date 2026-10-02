#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Poll and durably capture messages for the scripted bot reader.

The module deliberately keeps transport, parsing, and filesystem commits in
one small standard-library adapter. It never reports response bodies, message
text, or credential-bearing URLs.
"""

from __future__ import annotations

import argparse
import ipaddress
import json
import os
from pathlib import Path
import re
import sys
import tempfile
import time
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode, urlparse
from urllib.request import Request, urlopen


PRODUCTION_ENDPOINT = "https://api.telegram.org"
EXIT_FAILURE = 1
EXIT_CONFIGURATION = 2


class CaptureError(Exception):
    """Describe a bounded capture failure without retaining private input."""

    def __init__(self, outcome: str):
        super().__init__(outcome)
        self.outcome = outcome


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _temporary_text(path: Path, text: str) -> Path:
    descriptor, name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = Path(name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            handle.write(text)
            handle.flush()
            os.fsync(handle.fileno())
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise
    return temporary


def _atomic_replace_text(path: Path, text: str, failpoint: str | None = None) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    temporary = _temporary_text(path, text)
    try:
        if failpoint == "cursor_before_replace":
            raise CaptureError("interrupted")
        os.replace(temporary, path)
        _fsync_directory(path.parent)
    finally:
        temporary.unlink(missing_ok=True)


def _publish_record(path: Path, text: str, failpoint: str | None = None) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    if path.exists():
        try:
            if path.read_text(encoding="utf-8") == text:
                return
        except OSError as error:
            raise CaptureError("disk_failure") from error
        raise CaptureError("record_conflict")

    temporary = _temporary_text(path, text)
    try:
        if failpoint == "inbox_before_publish":
            raise CaptureError("interrupted")
        try:
            os.link(temporary, path)
        except FileExistsError:
            if path.read_text(encoding="utf-8") != text:
                raise CaptureError("record_conflict")
        _fsync_directory(path.parent)
    finally:
        temporary.unlink(missing_ok=True)


def read_checkpoint(path: Path) -> int:
    """Read the last handled update identifier, accepting absence as zero."""
    try:
        raw = path.read_text(encoding="utf-8").strip()
    except FileNotFoundError:
        return 0
    except OSError as error:
        raise CaptureError("invalid_checkpoint") from error
    if not raw or not raw.isascii() or not raw.isdecimal():
        raise CaptureError("invalid_checkpoint")
    value = int(raw)
    if value < 0:
        raise CaptureError("invalid_checkpoint")
    return value


def _validated_updates(document: Any) -> list[dict[str, Any]]:
    if not isinstance(document, dict):
        raise CaptureError("malformed_response")
    if document.get("ok") is not True:
        if document.get("error_code") == 409:
            raise CaptureError("api_conflict")
        if document.get("error_code") in (401, 403):
            raise CaptureError("credential_failure")
        raise CaptureError("api_failure")
    updates = document.get("result")
    if not isinstance(updates, list):
        raise CaptureError("malformed_response")
    for update in updates:
        if not isinstance(update, dict):
            raise CaptureError("malformed_response")
        update_id = update.get("update_id")
        if isinstance(update_id, bool) or not isinstance(update_id, int) or update_id < 0:
            raise CaptureError("malformed_response")
    return updates


def capture_response(
    response: bytes,
    operator_chat: str,
    inbox: Path,
    checkpoint_path: Path,
    failpoint: str | None = None,
) -> dict[str, int | str]:
    """Capture one response and advance only after every record is durable."""
    try:
        document = json.loads(response)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise CaptureError("malformed_response") from error

    updates = _validated_updates(document)
    checkpoint = read_checkpoint(checkpoint_path)
    records: list[tuple[Path, str]] = []
    maximum = checkpoint
    for update in updates:
        update_id = update["update_id"]
        maximum = max(maximum, update_id)
        message = update.get("message")
        if not isinstance(message, dict):
            continue
        chat = message.get("chat")
        sender = message.get("from")
        text = message.get("text")
        if (
            isinstance(chat, dict)
            and str(chat.get("id", "")) == operator_chat
            and isinstance(sender, dict)
            and sender.get("is_bot") is not True
            and isinstance(text, str)
            and bool(text)
        ):
            record = {
                "update_id": update_id,
                "date": message.get("date"),
                "from": sender.get("first_name"),
                "text": text,
            }
            encoded = json.dumps(record, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
            records.append((inbox / f"{update_id}.json", encoded))

    for path, encoded in records:
        _publish_record(path, encoded, failpoint)
    if maximum > checkpoint:
        _atomic_replace_text(checkpoint_path, f"{maximum}\n", failpoint)
    outcome = "captured" if records else ("empty" if not updates else "filtered")
    return {
        "outcome": outcome,
        "updates": len(updates),
        "captured": len(records),
        "checkpoint": maximum,
    }


def _read_token(path: Path) -> str:
    try:
        contents = path.read_text(encoding="utf-8")
    except OSError as error:
        raise CaptureError("credential_failure") from error
    match = re.search(r'^bot_token\s*=\s*"([^"\r\n]+)"\s*$', contents, re.MULTILINE)
    if match is None:
        raise CaptureError("credential_failure")
    return match.group(1)


def _endpoint(endpoint: str | None, token: str) -> str:
    if endpoint is None:
        return PRODUCTION_ENDPOINT
    if os.environ.get("COSMON_BOT_READER_TEST_MODE") != "1":
        raise CaptureError("test_endpoint_refused")
    parsed = urlparse(endpoint)
    try:
        loopback = parsed.hostname is not None and ipaddress.ip_address(parsed.hostname).is_loopback
    except ValueError:
        loopback = False
    if parsed.scheme != "http" or not loopback or parsed.username or parsed.password:
        raise CaptureError("test_endpoint_refused")
    if not token.startswith("synthetic-"):
        raise CaptureError("test_endpoint_refused")
    return endpoint.rstrip("/")


def poll(endpoint: str, token: str, offset: int, timeout_seconds: int = 20) -> bytes:
    """Fetch one bounded long-poll response without exposing its URL."""
    query = urlencode(
        {
            "offset": offset + 1,
            "timeout": timeout_seconds,
            "allowed_updates": '["message"]',
        }
    )
    request = Request(f"{endpoint}/bot{token}/getUpdates?{query}")
    try:
        with urlopen(request, timeout=timeout_seconds + 5) as response:
            return response.read()
    except HTTPError as error:
        if error.code == 409:
            raise CaptureError("api_conflict") from error
        if error.code in (401, 403):
            raise CaptureError("credential_failure") from error
        raise CaptureError("api_failure") from error
    except (OSError, URLError, TimeoutError) as error:
        raise CaptureError("network_failure") from error


def _append_outcome(log_path: Path, result: dict[str, Any]) -> None:
    try:
        log_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        stamp = time.strftime("%FT%TZ", time.gmtime())
        with log_path.open("a", encoding="utf-8") as handle:
            handle.write(f"[{stamp}] bot-reader {json.dumps(result, sort_keys=True)}\n")
    except OSError:
        pass


def _emit(result: dict[str, Any], log_path: Path | None) -> None:
    if log_path is not None:
        _append_outcome(log_path, result)
    print(json.dumps(result, sort_keys=True))


def main() -> int:
    parser = argparse.ArgumentParser(description="Poll and capture one bot-reader batch")
    parser.add_argument("--token-file", type=Path, required=True)
    parser.add_argument("--operator-chat", required=True)
    parser.add_argument("--inbox", type=Path, required=True)
    parser.add_argument("--checkpoint", type=Path, required=True)
    parser.add_argument("--log", type=Path, required=True)
    parser.add_argument("--test-endpoint")
    args = parser.parse_args()

    try:
        token = _read_token(args.token_file)
        endpoint = _endpoint(args.test_endpoint, token)
        checkpoint = read_checkpoint(args.checkpoint)
        response = poll(endpoint, token, checkpoint)
        failpoint = None
        if os.environ.get("COSMON_BOT_READER_TEST_MODE") == "1":
            failpoint = os.environ.get("COSMON_BOT_READER_TEST_INTERRUPT")
        result = capture_response(
            response,
            args.operator_chat,
            args.inbox,
            args.checkpoint,
            failpoint,
        )
    except CaptureError as error:
        result = {"outcome": error.outcome}
        _emit(result, args.log)
        return EXIT_CONFIGURATION if error.outcome == "test_endpoint_refused" else EXIT_FAILURE
    except OSError:
        result = {"outcome": "disk_failure"}
        _emit(result, args.log)
        return EXIT_FAILURE
    _emit(result, args.log)
    return 0


if __name__ == "__main__":
    sys.exit(main())
