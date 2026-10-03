#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Poll and durably capture messages for the scripted bot reader.

The module deliberately keeps transport, parsing, and filesystem commits in
one small standard-library adapter. It never reports response bodies, message
text, or credential-bearing URLs.

Admission is fenced per host: a poll holds a permanent lock file from the
ownership check through the durable checkpoint commit, and only an enrolled
reader whose journal is ``active`` reaches the network. The request runs in
this process, never in a child, so losing the process loses the request too.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import hmac
import ipaddress
import json
import os
from pathlib import Path
import re
import secrets
import stat
import sys
import tempfile
import time
from typing import Any, Iterator
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode, urlparse
from urllib.request import Request, urlopen


PRODUCTION_ENDPOINT = "https://api.telegram.org"
EXIT_FAILURE = 1
EXIT_CONFIGURATION = 2
JOURNAL_SCHEMA = 1
JOURNAL_MAX_BYTES = 4096
HOST_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
BOT_ID = re.compile(r"^[A-Za-z0-9-]{1,64}$")
TRANSFER_ID = re.compile(r"^[0-9a-f]{16}$")
HEX_DIGEST = re.compile(r"^[0-9a-f]{64}$")
MANIFEST_VERSION = 1
MANIFEST_MAX_BYTES = 4096
STOP_MARKER = "fenced by bot-reader-transfer\n"
# Source phases: active -> fenced -> offered -> transferred (or back to active
# on cancel). Destination phases: standby -> staged -> active.
PHASES = ("active", "fenced", "offered", "transferred", "standby", "staged")
OWNER_PHASES = ("active", "fenced", "offered")
TRANSFER_PHASES = ("offered", "transferred", "staged")
MANIFEST_FIELDS = (
    "version",
    "bot_id",
    "source",
    "destination",
    "epoch",
    "transfer_id",
    "checkpoint",
    "commit_hash",
)
TEST_VARIABLES = (
    "COSMON_BOT_READER_TEST_ROOT",
    "COSMON_BOT_READER_TEST_ENDPOINT",
    "COSMON_BOT_READER_TEST_INTERRUPT",
    "COSMON_BOT_READER_TEST_PAUSE",
)


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


def read_checkpoint(path: Path, required: bool = False) -> int:
    """Read the last handled update identifier.

    Absence reads as zero only before enrollment; an enrolled reader passes
    ``required`` so a lost checkpoint is refused instead of replayed from zero.
    """
    try:
        raw = path.read_text(encoding="utf-8").strip()
    except FileNotFoundError:
        if required:
            raise CaptureError("invalid_state") from None
        return 0
    except OSError as error:
        raise CaptureError("invalid_checkpoint") from error
    if not raw or not raw.isascii() or not raw.isdecimal():
        raise CaptureError("invalid_checkpoint")
    value = int(raw)
    if value < 0:
        raise CaptureError("invalid_checkpoint")
    return value


class StatePaths:
    """Name every file of one host's reader state from its canonical root."""

    def __init__(self, root: Path):
        self.root = root
        self.journal = root / "telegram-reader.json"
        self.lock = root / "telegram-listen.lock"
        self.stop = root / "telegram-listen.off"
        self.in_flight = root / "telegram-listen.inflight"
        self.checkpoint = root / "telegram-offset"
        self.inbox = root / "telegram-inbox"
        self.log = root / "logs" / "telegram-listen.log"


def test_mode() -> bool:
    """Report whether the explicit isolated-test configuration is selected."""
    return os.environ.get("COSMON_BOT_READER_TEST_MODE") == "1"


def resolve_state_root() -> Path:
    """Return the host-local state root, or the isolated fixture root in tests."""
    if test_mode():
        root = os.environ.get("COSMON_BOT_READER_TEST_ROOT")
        if not root:
            raise CaptureError("test_endpoint_refused")
        return Path(root) / ".cosmon"
    if any(os.environ.get(name) for name in TEST_VARIABLES):
        raise CaptureError("test_endpoint_refused")
    return Path.home() / ".cosmon"


@contextmanager
def exclusive_lock(paths: StatePaths) -> Iterator[None]:
    """Hold the host's reader lock; the lock file is never unlinked."""
    paths.root.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor = os.open(paths.lock, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        yield
    finally:
        os.close(descriptor)


def lock_state(paths: StatePaths) -> str:
    """Probe the lock without creating, writing, or waiting on anything."""
    try:
        descriptor = os.open(paths.lock, os.O_RDONLY | os.O_NOFOLLOW)
    except FileNotFoundError:
        return "free"
    except OSError:
        return "invalid"
    try:
        fcntl.flock(descriptor, fcntl.LOCK_SH | fcntl.LOCK_NB)
    except BlockingIOError:
        return "busy"
    finally:
        os.close(descriptor)
    return "free"


def _is_count(value: Any) -> bool:
    return not isinstance(value, bool) and isinstance(value, int) and value >= 0


def manifest_digest(manifest: dict[str, Any]) -> str:
    """Digest the bound manifest fields so corruption or edits are detected."""
    bound = {name: manifest.get(name) for name in MANIFEST_FIELDS}
    encoded = json.dumps(bound, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def validated_manifest(document: Any) -> dict[str, Any]:
    """Accept only a well-formed manifest whose digest matches its fields.

    The digest catches a damaged or hand-edited manifest; authority comes
    from the source's commit receipt, never from the manifest itself.
    """
    if not isinstance(document, dict) or set(document) != set(MANIFEST_FIELDS) | {"digest"}:
        raise CaptureError("invalid_manifest")
    checks = (
        document["version"] == MANIFEST_VERSION,
        isinstance(document["bot_id"], str) and BOT_ID.match(document["bot_id"]) is not None,
        isinstance(document["source"], str) and HOST_ID.match(document["source"]) is not None,
        isinstance(document["destination"], str)
        and HOST_ID.match(document["destination"]) is not None,
        document["source"] != document["destination"],
        _is_count(document["epoch"]) and document["epoch"] >= 2,
        isinstance(document["transfer_id"], str)
        and TRANSFER_ID.match(document["transfer_id"]) is not None,
        _is_count(document["checkpoint"]),
        isinstance(document["commit_hash"], str)
        and HEX_DIGEST.match(document["commit_hash"]) is not None,
        isinstance(document["digest"], str)
        and hmac.compare_digest(document["digest"], manifest_digest(document)),
    )
    if not all(checks):
        raise CaptureError("invalid_manifest")
    return document


def _validated_journal(document: Any) -> dict[str, Any]:
    if not isinstance(document, dict) or document.get("schema") != JOURNAL_SCHEMA:
        raise CaptureError("invalid_state")
    host_id = document.get("host_id")
    epoch = document.get("epoch")
    phase = document.get("phase")
    fence = document.get("fence")
    transfer = document.get("transfer")
    if not isinstance(host_id, str) or HOST_ID.match(host_id) is None:
        raise CaptureError("invalid_state")
    if not _is_count(epoch) or phase not in PHASES:
        raise CaptureError("invalid_state")
    if phase in OWNER_PHASES and epoch < 1:
        raise CaptureError("invalid_state")
    if phase == "active" and fence is not None:
        raise CaptureError("invalid_state")
    if phase != "active" and not (
        isinstance(fence, dict) and isinstance(fence.get("stop_preexisting"), bool)
    ):
        raise CaptureError("invalid_state")
    if transfer is None:
        if phase in TRANSFER_PHASES:
            raise CaptureError("invalid_state")
    else:
        if phase not in TRANSFER_PHASES + ("active",) or not isinstance(transfer, dict):
            raise CaptureError("invalid_state")
        try:
            validated_manifest(transfer.get("manifest"))
        except CaptureError:
            raise CaptureError("invalid_state") from None
        secret = transfer.get("commit_secret")
        if phase in ("offered", "transferred") and not (
            isinstance(secret, str) and HEX_DIGEST.match(secret) is not None
        ):
            raise CaptureError("invalid_state")
    return document


def read_journal(paths: StatePaths) -> dict[str, Any] | None:
    """Read the ownership journal; ``None`` means this host never enrolled."""
    try:
        info = os.lstat(paths.journal)
    except FileNotFoundError:
        return None
    except OSError as error:
        raise CaptureError("invalid_state") from error
    if not stat.S_ISREG(info.st_mode) or info.st_size > JOURNAL_MAX_BYTES:
        raise CaptureError("invalid_state")
    try:
        document = json.loads(paths.journal.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise CaptureError("invalid_state") from error
    return _validated_journal(document)


def write_journal(paths: StatePaths, journal: dict[str, Any]) -> None:
    """Durably replace the ownership journal after validating it."""
    _validated_journal(journal)
    _atomic_replace_text(paths.journal, json.dumps(journal, sort_keys=True) + "\n")


def _clear_in_flight(paths: StatePaths) -> None:
    paths.in_flight.unlink(missing_ok=True)
    _fsync_directory(paths.root)


def enroll(paths: StatePaths, host_id: str) -> dict[str, Any]:
    """Make this host the initial owner, validating any legacy checkpoint."""
    if HOST_ID.match(host_id) is None:
        raise CaptureError("invalid_host_id")
    with exclusive_lock(paths):
        if os.path.lexists(paths.journal):
            raise CaptureError("already_enrolled")
        if os.path.lexists(paths.checkpoint):
            if not stat.S_ISREG(os.lstat(paths.checkpoint).st_mode):
                raise CaptureError("invalid_checkpoint")
            checkpoint = read_checkpoint(paths.checkpoint)
        else:
            checkpoint = 0
            _atomic_replace_text(paths.checkpoint, "0\n")
        write_journal(
            paths,
            {"schema": JOURNAL_SCHEMA, "host_id": host_id, "epoch": 1, "phase": "active", "fence": None},
        )
    return {"outcome": "enrolled", "host_id": host_id, "epoch": 1, "checkpoint": checkpoint}


def _place_stop(paths: StatePaths) -> bool:
    """Create the stop file unless present; report whether it pre-existed."""
    paths.root.mkdir(mode=0o700, parents=True, exist_ok=True)
    try:
        descriptor = os.open(paths.stop, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    except FileExistsError:
        return True
    try:
        os.write(descriptor, STOP_MARKER.encode())
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    _fsync_directory(paths.root)
    return False


def fence(paths: StatePaths) -> dict[str, Any]:
    """Stop new polls, drain the admitted one, and record the fence durably.

    The stop file goes first so no further poll is admitted while this waits
    for the lock; the journal phase then outlives any removal of that file.
    An in-flight marker left by a lost request keeps the result indeterminate.
    """
    preexisting = _place_stop(paths)
    with exclusive_lock(paths):
        journal = read_journal(paths)
        if journal is None:
            raise CaptureError("not_enrolled")
        if journal["phase"] not in OWNER_PHASES:
            raise CaptureError("not_owner")
        if journal["phase"] == "active":
            journal = dict(journal, phase="fenced", fence={"stop_preexisting": preexisting}, transfer=None)
            write_journal(paths, journal)
        checkpoint = read_checkpoint(paths.checkpoint, required=True)
        if os.path.lexists(paths.in_flight):
            raise CaptureError("indeterminate_in_flight")
    return {"outcome": "fenced", "epoch": journal["epoch"], "checkpoint": checkpoint}


def status(paths: StatePaths) -> dict[str, Any]:
    """Describe local reader state without network access or any write."""
    result: dict[str, Any] = {
        "outcome": "status",
        "stop": os.path.lexists(paths.stop),
        "in_flight": os.path.lexists(paths.in_flight),
        "lock": lock_state(paths),
    }
    try:
        journal = read_journal(paths)
    except CaptureError:
        result.update(enrolled=True, phase="invalid")
    else:
        if journal is None:
            result.update(enrolled=False, phase=None)
        else:
            result.update(
                enrolled=True,
                host_id=journal["host_id"],
                epoch=journal["epoch"],
                phase=journal["phase"],
            )
            if journal.get("transfer") is not None:
                manifest = journal["transfer"]["manifest"]
                result["transfer"] = {
                    name: manifest[name]
                    for name in ("transfer_id", "source", "destination", "epoch", "checkpoint")
                }
    try:
        result["checkpoint"] = (
            read_checkpoint(paths.checkpoint, required=True)
            if os.path.lexists(paths.checkpoint)
            else None
        )
    except CaptureError:
        result["checkpoint"] = "invalid"
    return result


def resolve_token_file() -> Path:
    """Return the host's credential file, or the fixture's in isolated tests."""
    if test_mode():
        return resolve_state_root().parent / "bot.toml"
    return Path.home() / ".showroom" / "bot.toml"


def bot_identity(token_file: Path) -> str:
    """Name the bot a credential belongs to without a network call.

    The part of a bot token before its colon is the bot's public identifier;
    only that part ever leaves this function.
    """
    prefix, separator, _ = _read_token(token_file).partition(":")
    if not separator or BOT_ID.match(prefix) is None:
        raise CaptureError("credential_failure")
    return prefix


def _test_failpoint() -> str | None:
    return os.environ.get("COSMON_BOT_READER_TEST_INTERRUPT") if test_mode() else None


def _commit_hash(secret: str) -> str:
    return hashlib.sha256(secret.encode()).hexdigest()


def _remove_owned_stop(paths: StatePaths, fence_record: dict[str, Any]) -> None:
    """Remove the stop file only if a transfer placed it and nobody replaced it."""
    if fence_record["stop_preexisting"]:
        return
    try:
        if paths.stop.read_text(encoding="utf-8") != STOP_MARKER:
            return
    except FileNotFoundError:
        return
    except (OSError, UnicodeDecodeError) as error:
        raise CaptureError("invalid_state") from error
    paths.stop.unlink(missing_ok=True)
    _fsync_directory(paths.root)


def _inbox_is_empty(paths: StatePaths) -> bool:
    try:
        entries = os.listdir(paths.inbox)
    except FileNotFoundError:
        return True
    except OSError as error:
        raise CaptureError("invalid_state") from error
    return not [name for name in entries if not name.startswith(".")]


def prepare(paths: StatePaths, host_id: str, token_file: Path) -> dict[str, Any]:
    """Enroll this host as an inactive transfer destination.

    Unlike ``fence`` this reads ownership before touching the stop file, so
    pointing it at the live reader by mistake refuses without stopping it.
    """
    if HOST_ID.match(host_id) is None:
        raise CaptureError("invalid_host_id")
    bot_id = bot_identity(token_file)
    with exclusive_lock(paths):
        journal = read_journal(paths)
        if journal is None:
            preexisting = _place_stop(paths)
            journal = {
                "schema": JOURNAL_SCHEMA,
                "host_id": host_id,
                "epoch": 0,
                "phase": "standby",
                "fence": {"stop_preexisting": preexisting},
                "transfer": None,
            }
            write_journal(paths, journal)
        elif journal["host_id"] != host_id:
            raise CaptureError("host_id_mismatch")
        elif journal["phase"] in OWNER_PHASES:
            raise CaptureError("destination_not_inactive")
    return {
        "outcome": "prepared",
        "host_id": host_id,
        "epoch": journal["epoch"],
        "phase": journal["phase"],
        "bot_id": bot_id,
    }


def export(paths: StatePaths, destination: str, token_file: Path) -> dict[str, Any]:
    """Fence and drain this owner, then offer ownership to one destination.

    The offer is durable before the manifest leaves this host, and repeating
    the call for the same destination returns the same manifest, so a lost
    reply never produces a second, different offer.
    """
    if HOST_ID.match(destination) is None:
        raise CaptureError("invalid_host_id")
    preexisting = _place_stop(paths)
    with exclusive_lock(paths):
        journal = read_journal(paths)
        if journal is None:
            raise CaptureError("not_enrolled")
        if journal["phase"] in ("offered", "transferred"):
            manifest = journal["transfer"]["manifest"]
            if manifest["destination"] != destination:
                raise CaptureError("offered_elsewhere" if journal["phase"] == "offered" else "not_owner")
            return {"outcome": journal["phase"], "manifest": manifest}
        if journal["phase"] not in ("active", "fenced"):
            raise CaptureError("not_owner")
        if journal["phase"] == "active":
            journal = dict(journal, phase="fenced", fence={"stop_preexisting": preexisting}, transfer=None)
            write_journal(paths, journal)
        if destination == journal["host_id"]:
            raise CaptureError("invalid_destination")
        checkpoint = read_checkpoint(paths.checkpoint, required=True)
        if os.path.lexists(paths.in_flight):
            raise CaptureError("indeterminate_in_flight")
        if not _inbox_is_empty(paths):
            raise CaptureError("inbox_not_empty")
        secret = secrets.token_hex(32)
        manifest: dict[str, Any] = {
            "version": MANIFEST_VERSION,
            "bot_id": bot_identity(token_file),
            "source": journal["host_id"],
            "destination": destination,
            "epoch": journal["epoch"] + 1,
            "transfer_id": secrets.token_hex(8),
            "checkpoint": checkpoint,
            "commit_hash": _commit_hash(secret),
        }
        manifest["digest"] = manifest_digest(manifest)
        write_journal(
            paths,
            dict(journal, phase="offered", transfer={"manifest": manifest, "commit_secret": secret}),
        )
    return {"outcome": "offered", "manifest": manifest}


def commit(paths: StatePaths, document: Any) -> dict[str, Any]:
    """Commit this source's outstanding offer and reveal its receipt.

    Commit and cancel take the same lock and compare the whole manifest, so
    exactly one of them decides an offer. The receipt carries the secret
    whose hash the manifest commits to: a manifest alone cannot forge it.
    """
    manifest = validated_manifest(document)
    with exclusive_lock(paths):
        journal = read_journal(paths)
        if journal is None:
            raise CaptureError("not_enrolled")
        transfer = journal.get("transfer")
        if journal["phase"] not in ("offered", "transferred") or transfer["manifest"] != manifest:
            raise CaptureError("offer_mismatch")
        if journal["phase"] == "offered":
            write_journal(paths, dict(journal, phase="transferred", epoch=manifest["epoch"]))
        receipt = {
            "transfer_id": manifest["transfer_id"],
            "epoch": manifest["epoch"],
            "digest": manifest["digest"],
            "commit_secret": transfer["commit_secret"],
        }
        if _test_failpoint() == "after_commit":
            raise CaptureError("interrupted")
    return {"outcome": "committed", "receipt": receipt}


def cancel(paths: StatePaths, transfer_id: str | None) -> dict[str, Any]:
    """Withdraw an uncommitted offer, or a bare fence, and resume this owner."""
    with exclusive_lock(paths):
        journal = read_journal(paths)
        if journal is None:
            raise CaptureError("not_enrolled")
        if journal["phase"] == "transferred":
            raise CaptureError("already_committed")
        if journal["phase"] == "offered":
            if transfer_id != journal["transfer"]["manifest"]["transfer_id"]:
                raise CaptureError("offer_mismatch")
        elif journal["phase"] != "fenced" or transfer_id is not None:
            raise CaptureError("offer_mismatch" if journal["phase"] == "fenced" else "not_owner")
        # The stop file goes before the journal flips, so a crash in between
        # leaves a host the journal still holds stopped.
        _remove_owned_stop(paths, journal["fence"])
        write_journal(paths, dict(journal, phase="active", fence=None, transfer=None))
    return {"outcome": "cancelled", "epoch": journal["epoch"]}


def import_manifest(paths: StatePaths, document: Any, token_file: Path) -> dict[str, Any]:
    """Stage an offer on this destination without admitting any poll.

    The cursor is not written here: a staged host keeps whatever checkpoint
    it had until a committed receipt activates it.
    """
    manifest = validated_manifest(document)
    with exclusive_lock(paths):
        journal = read_journal(paths)
        if journal is None:
            raise CaptureError("not_prepared")
        if manifest["destination"] != journal["host_id"]:
            raise CaptureError("wrong_destination")
        if bot_identity(token_file) != manifest["bot_id"]:
            raise CaptureError("wrong_bot")
        transfer = journal.get("transfer")
        if (
            journal["phase"] in ("staged", "active")
            and transfer is not None
            and transfer["manifest"] == manifest
        ):
            return {"outcome": journal["phase"], "transfer_id": manifest["transfer_id"]}
        if journal["phase"] not in ("standby", "transferred", "staged"):
            raise CaptureError("destination_not_inactive")
        if manifest["epoch"] <= journal["epoch"]:
            raise CaptureError("stale_epoch")
        write_journal(paths, dict(journal, phase="staged", transfer={"manifest": manifest}))
    return {"outcome": "staged", "transfer_id": manifest["transfer_id"]}


def _receipt_matches(manifest: dict[str, Any], receipt: Any) -> bool:
    if not isinstance(receipt, dict) or set(receipt) != {"transfer_id", "epoch", "digest", "commit_secret"}:
        return False
    secret = receipt["commit_secret"]
    if not isinstance(secret, str) or HEX_DIGEST.match(secret) is None:
        return False
    return (
        receipt["transfer_id"] == manifest["transfer_id"]
        and receipt["epoch"] == manifest["epoch"]
        and receipt["digest"] == manifest["digest"]
        and hmac.compare_digest(_commit_hash(secret), manifest["commit_hash"])
    )


def activate(paths: StatePaths, receipt: Any) -> dict[str, Any]:
    """Make this staged destination the owner, given the source's receipt.

    The checkpoint is written and the transfer's own stop marker removed
    before the journal flips to active, so a crash at any point leaves a
    host that still refuses to poll and a retry that completes the step.
    """
    with exclusive_lock(paths):
        journal = read_journal(paths)
        if journal is None:
            raise CaptureError("not_prepared")
        transfer = journal.get("transfer")
        if transfer is None or not _receipt_matches(transfer["manifest"], receipt):
            raise CaptureError("receipt_mismatch")
        manifest = transfer["manifest"]
        if journal["phase"] == "active":
            return {"outcome": "activated", "epoch": journal["epoch"], "checkpoint": manifest["checkpoint"]}
        if journal["phase"] != "staged":
            raise CaptureError("receipt_mismatch")
        _atomic_replace_text(paths.checkpoint, f"{manifest['checkpoint']}\n")
        _remove_owned_stop(paths, journal["fence"])
        if _test_failpoint() == "activate_before_journal":
            raise CaptureError("interrupted")
        write_journal(
            paths,
            dict(journal, phase="active", epoch=manifest["epoch"], fence=None, transfer={"manifest": manifest}),
        )
    return {"outcome": "activated", "epoch": manifest["epoch"], "checkpoint": manifest["checkpoint"]}


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
    if not test_mode():
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


def _pause_for_test() -> None:
    """Hold before admission so a test can fence in that exact window."""
    pause = os.environ.get("COSMON_BOT_READER_TEST_PAUSE") if test_mode() else None
    if not pause:
        return
    Path(f"{pause}.waiting").write_text("", encoding="utf-8")
    deadline = time.monotonic() + 30
    while not Path(pause).exists() and time.monotonic() < deadline:
        time.sleep(0.01)


def poll_once(paths: StatePaths, token_file: Path, operator_chat: str, test_endpoint: str | None) -> dict[str, Any]:
    """Admit and run at most one poll for this host, under its lock."""
    _pause_for_test()
    with exclusive_lock(paths):
        if os.path.lexists(paths.stop):
            return {"outcome": "stopped"}
        journal = read_journal(paths)
        if journal is None:
            raise CaptureError("not_enrolled")
        if journal["phase"] != "active":
            return {"outcome": "transfer_pending"}
        checkpoint = read_checkpoint(paths.checkpoint, required=True)
        token = _read_token(token_file)
        endpoint = _endpoint(test_endpoint, token)
        _atomic_replace_text(
            paths.in_flight,
            json.dumps({"epoch": journal["epoch"], "offset": checkpoint}) + "\n",
        )
        try:
            response = poll(endpoint, token, checkpoint)
        except CaptureError as error:
            # A network failure may hide a request the server still holds; its
            # marker stays until a later completed poll replaces it.
            if error.outcome != "network_failure":
                _clear_in_flight(paths)
            raise
        failpoint = os.environ.get("COSMON_BOT_READER_TEST_INTERRUPT") if test_mode() else None
        try:
            return capture_response(response, operator_chat, paths.inbox, paths.checkpoint, failpoint)
        finally:
            _clear_in_flight(paths)


def main() -> int:
    parser = argparse.ArgumentParser(description="Poll and capture one bot-reader batch")
    parser.add_argument("--state-root", type=Path, required=True)
    parser.add_argument("--token-file", type=Path, required=True)
    parser.add_argument("--operator-chat", required=True)
    parser.add_argument("--test-endpoint")
    args = parser.parse_args()
    paths = StatePaths(args.state_root)

    try:
        result = poll_once(paths, args.token_file, args.operator_chat, args.test_endpoint)
    except CaptureError as error:
        result = {"outcome": error.outcome}
        _emit(result, paths.log)
        return EXIT_CONFIGURATION if error.outcome == "test_endpoint_refused" else EXIT_FAILURE
    except OSError:
        result = {"outcome": "disk_failure"}
        _emit(result, paths.log)
        return EXIT_FAILURE
    _emit(result, paths.log)
    return 0


if __name__ == "__main__":
    sys.exit(main())
