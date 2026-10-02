#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Local ownership primitives for moving the bot reader between hosts.

Each verb acts on this host's reader state only and never contacts the bot
API: ``enroll`` makes the host the initial owner, ``fence`` stops and drains
the local reader and records that durably, and ``status`` reads state without
writing anything. Cross-host offer, import, and activation build on these.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bot_reader  # noqa: E402


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    verbs = parser.add_subparsers(dest="verb", required=True)
    enroll = verbs.add_parser("enroll", help="make this host the initial reader owner")
    enroll.add_argument("--host-id", required=True, help="stable identity of this host")
    verbs.add_parser("fence", help="stop new polls, drain the admitted one, record the fence")
    verbs.add_parser("status", help="report local reader state; no network, no writes")
    args = parser.parse_args()

    try:
        paths = bot_reader.StatePaths(bot_reader.resolve_state_root())
        if args.verb == "enroll":
            result = bot_reader.enroll(paths, args.host_id)
        elif args.verb == "fence":
            result = bot_reader.fence(paths)
        else:
            result = bot_reader.status(paths)
    except bot_reader.CaptureError as error:
        print(json.dumps({"outcome": error.outcome}, sort_keys=True))
        refused = ("test_endpoint_refused", "invalid_host_id")
        return bot_reader.EXIT_CONFIGURATION if error.outcome in refused else bot_reader.EXIT_FAILURE
    except OSError:
        print(json.dumps({"outcome": "disk_failure"}, sort_keys=True))
        return bot_reader.EXIT_FAILURE
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
