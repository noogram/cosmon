#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Move bot-reader ownership between hosts through a committed offer.

Each host-local verb acts on this host's reader state only and never contacts
the bot API: ``enroll`` makes the host the initial owner, ``fence`` stops and
drains the local reader, ``status`` reads state without writing anything,
``prepare`` enrolls an inactive destination, ``export`` fences this owner and
offers ownership to one destination, ``import`` stages that offer on the
destination, ``commit`` lets the source decide the offer and release its
receipt, ``activate`` makes the destination the owner given that receipt, and
``cancel`` withdraws an uncommitted offer. Manifests and receipts travel on
stdin as JSON, never as command arguments.

``transfer`` is the coordinator: it runs those steps in order against two
peers, each reached as ``local`` (this host), ``ssh:<alias>`` (authenticated
remote execution with host-key checking and no prompts), or ``local:<root>``
(an isolated test root, accepted only in test mode). Every step is idempotent,
so re-running the same transfer after a failure resumes it; it never unfences
the source or promotes the destination on its own.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bot_reader  # noqa: E402


SCRIPT = Path(__file__).resolve()
SSH_ALIAS = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
REMOTE_SCRIPT = re.compile(r"^[A-Za-z0-9._/~-]{1,256}$")
PEER_TIMEOUT_SECONDS = 120
CONFIGURATION_OUTCOMES = (
    "test_endpoint_refused",
    "invalid_host_id",
    "invalid_peer",
    "remote_script_required",
)


class Peer:
    """Run one transfer verb on a host and return its exit status and outcome."""

    def __init__(self, argv: list[str], env: dict[str, str] | None) -> None:
        self.argv = argv
        self.env = env

    def call(self, verb: str, args: list[str], payload: dict[str, Any] | None = None) -> tuple[int, dict[str, Any]]:
        try:
            completed = subprocess.run(
                [*self.argv, verb, *args],
                env=self.env,
                input=json.dumps(payload).encode() if payload is not None else b"",
                capture_output=True,
                timeout=PEER_TIMEOUT_SECONDS,
            )
        except (OSError, subprocess.TimeoutExpired):
            return bot_reader.EXIT_FAILURE, {"outcome": "transport_failure"}
        # Only the structured last line is trusted; stderr is never relayed.
        lines = completed.stdout.decode("utf-8", "replace").strip().splitlines()
        try:
            result = json.loads(lines[-1]) if lines else None
        except json.JSONDecodeError:
            result = None
        if not isinstance(result, dict) or not isinstance(result.get("outcome"), str):
            return bot_reader.EXIT_FAILURE, {"outcome": "transport_failure"}
        return completed.returncode, result


def ssh_argv(alias: str, remote_script: str) -> list[str]:
    """Build the remote invocation; batch mode refuses any interactive prompt."""
    return [
        "ssh",
        "-o", "BatchMode=yes",
        "-o", "StrictHostKeyChecking=yes",
        "-T",
        "--",
        alias,
        "python3",
        remote_script,
    ]


def parse_peer(spec: str, remote_script: str | None) -> Peer:
    """Resolve a peer specification into the command that reaches it."""
    if spec == "local":
        return Peer([sys.executable, str(SCRIPT)], None)
    kind, separator, value = spec.partition(":")
    if separator and kind == "local":
        if not bot_reader.test_mode():
            raise bot_reader.CaptureError("test_endpoint_refused")
        env = dict(os.environ, COSMON_BOT_READER_TEST_ROOT=value)
        return Peer([sys.executable, str(SCRIPT)], env)
    if separator and kind == "ssh" and SSH_ALIAS.match(value):
        if remote_script is None:
            raise bot_reader.CaptureError("remote_script_required")
        if REMOTE_SCRIPT.match(remote_script) is None:
            raise bot_reader.CaptureError("invalid_peer")
        return Peer(ssh_argv(value, remote_script), None)
    raise bot_reader.CaptureError("invalid_peer")


def _already_handed_to(source: Peer, destination_host_id: str) -> bool:
    """Report whether the source already committed ownership to this destination.

    Only then may an owning destination be passed through on a re-run, so that
    repeating a completed transfer resumes it instead of being refused.
    """
    status, result = source.call("status", [])
    transfer = result.get("transfer")
    return (
        status == 0
        and result.get("phase") == "transferred"
        and isinstance(transfer, dict)
        and transfer.get("destination") == destination_host_id
    )


def coordinate(source: Peer, destination: Peer, destination_host_id: str) -> tuple[int, dict[str, Any]]:
    """Run prepare, export, import, commit and activate, stopping at a refusal."""
    if bot_reader.HOST_ID.match(destination_host_id) is None:
        raise bot_reader.CaptureError("invalid_host_id")
    manifest: dict[str, Any] | None = None

    def refused(step: str, result: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        report: dict[str, Any] = {"outcome": result["outcome"], "step": step}
        if manifest is not None:
            report["transfer_id"] = manifest.get("transfer_id")
        return bot_reader.EXIT_FAILURE, report

    status, result = destination.call("prepare", ["--host-id", destination_host_id])
    if status != 0 and not (
        result["outcome"] == "destination_not_inactive"
        and _already_handed_to(source, destination_host_id)
    ):
        return refused("prepare", result)
    status, result = source.call("export", ["--destination", destination_host_id])
    if status != 0 or not isinstance(result.get("manifest"), dict):
        return refused("export", result)
    manifest = result["manifest"]
    status, result = destination.call("import", [], manifest)
    if status != 0:
        return refused("import", result)
    status, result = source.call("commit", [], manifest)
    if status != 0 or not isinstance(result.get("receipt"), dict):
        return refused("commit", result)
    receipt = result["receipt"]
    status, result = destination.call("activate", [], receipt)
    if status != 0:
        return refused("activate", result)
    return 0, {
        "outcome": "transferred",
        "transfer_id": manifest["transfer_id"],
        "source": manifest["source"],
        "destination": manifest["destination"],
        "epoch": manifest["epoch"],
        "checkpoint": manifest["checkpoint"],
        "manifest": manifest,
        "receipt": receipt,
    }


def read_payload() -> Any:
    """Read one bounded JSON document from stdin."""
    raw = sys.stdin.buffer.read(bot_reader.MANIFEST_MAX_BYTES + 1)
    if len(raw) > bot_reader.MANIFEST_MAX_BYTES:
        raise bot_reader.CaptureError("invalid_manifest")
    try:
        return json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError):
        raise bot_reader.CaptureError("invalid_manifest") from None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    verbs = parser.add_subparsers(dest="verb", required=True)
    enroll = verbs.add_parser("enroll", help="make this host the initial reader owner")
    enroll.add_argument("--host-id", required=True, help="stable identity of this host")
    verbs.add_parser("fence", help="stop new polls, drain the admitted one, record the fence")
    verbs.add_parser("status", help="report local reader state; no network, no writes")
    prepare = verbs.add_parser("prepare", help="enroll this host as an inactive destination")
    prepare.add_argument("--host-id", required=True, help="stable identity of this host")
    export = verbs.add_parser("export", help="fence this owner and offer ownership to one destination")
    export.add_argument("--destination", required=True, help="host id of the destination")
    verbs.add_parser("import", help="stage an offer from the manifest on stdin")
    verbs.add_parser("commit", help="commit the offer on stdin and print its receipt")
    verbs.add_parser("activate", help="take ownership given the receipt on stdin")
    cancel = verbs.add_parser("cancel", help="withdraw an uncommitted offer and resume this owner")
    cancel.add_argument("--transfer-id", help="the outstanding offer; omit to lift a bare fence")
    transfer = verbs.add_parser("transfer", help="run the whole transfer between two peers")
    transfer.add_argument("--source", required=True, help="local, ssh:<alias>, or local:<root> in tests")
    transfer.add_argument("--destination", required=True, help="local, ssh:<alias>, or local:<root> in tests")
    transfer.add_argument("--destination-host-id", required=True, help="identity the destination enrolls under")
    transfer.add_argument("--remote-script", help="path of this script on ssh peers")
    args = parser.parse_args()

    status = 0
    try:
        if args.verb == "transfer":
            status, result = coordinate(
                parse_peer(args.source, args.remote_script),
                parse_peer(args.destination, args.remote_script),
                args.destination_host_id,
            )
        else:
            paths = bot_reader.StatePaths(bot_reader.resolve_state_root())
            if args.verb == "enroll":
                result = bot_reader.enroll(paths, args.host_id)
            elif args.verb == "fence":
                result = bot_reader.fence(paths)
            elif args.verb == "status":
                result = bot_reader.status(paths)
            elif args.verb == "prepare":
                result = bot_reader.prepare(paths, args.host_id, bot_reader.resolve_token_file())
            elif args.verb == "export":
                result = bot_reader.export(paths, args.destination, bot_reader.resolve_token_file())
            elif args.verb == "import":
                result = bot_reader.import_manifest(paths, read_payload(), bot_reader.resolve_token_file())
            elif args.verb == "commit":
                result = bot_reader.commit(paths, read_payload())
            elif args.verb == "activate":
                result = bot_reader.activate(paths, read_payload())
            else:
                result = bot_reader.cancel(paths, args.transfer_id)
    except bot_reader.CaptureError as error:
        print(json.dumps({"outcome": error.outcome}, sort_keys=True))
        if error.outcome in CONFIGURATION_OUTCOMES:
            return bot_reader.EXIT_CONFIGURATION
        return bot_reader.EXIT_FAILURE
    except OSError:
        print(json.dumps({"outcome": "disk_failure"}, sort_keys=True))
        return bot_reader.EXIT_FAILURE
    print(json.dumps(result, sort_keys=True))
    return status


if __name__ == "__main__":
    sys.exit(main())
