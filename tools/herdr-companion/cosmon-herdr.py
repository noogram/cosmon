#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Optional, one-shot Cosmon reader and explicit Herdr view opener.

Only supported CLI observations supply molecule identity and residence. No
Cosmon state file is parsed; no lifecycle command or terminal input is sent.
"""

import argparse
import datetime
import json
from pathlib import Path
import re
import subprocess
import sys


DELIVERABLES = ("synthesis.md", "frame.md", "outcomes.md", "plan.md", "report.md", "result.md")
TERMINAL_GAP = (
    "cs observe JSON has worker, but no transport backend, socket/session address "
    "or worker incarnation binding. Terminal presence is unknown; attachment is unavailable."
)


class Unavailable(Exception):
    """A missing capability, invalid observation or refused external command."""


def run(argv, cwd):
    """Run bounded argv directly; never evaluate labels or paths as shell text."""
    try:
        result = subprocess.run(argv, cwd=cwd, capture_output=True, timeout=20)
    except FileNotFoundError as error:
        raise Unavailable(f"backend unavailable: {argv[0]}") from error
    except (OSError, subprocess.TimeoutExpired) as error:
        raise Unavailable(f"command unavailable: {argv[0]}") from error
    if result.returncode:
        # Provider output can contain private source text. Report status only.
        raise Unavailable(f"{argv[0]} refused or failed (exit {result.returncode})")
    return result.stdout


def read_json(argv, cwd):
    """Reject a failed or malformed observation instead of guessing its shape."""
    try:
        return json.loads(run(argv, cwd))
    except (ValueError, UnicodeError) as error:
        raise Unavailable(f"invalid JSON observation from {argv[0]}") from error


def row(value):
    """Keep only display fields, excluding source variables and harness data."""
    if not isinstance(value, dict) or not all(
        isinstance(value.get(key), str) for key in ("id", "status")
    ):
        raise Unavailable("cs observe JSON requires string id and status")
    return {key: value.get(key) for key in ("id", "status", "formula", "worker")}


def observe(args, molecule_id):
    """Refuse Cosmon's convenient prefix resolution at this exact-id boundary."""
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]*", molecule_id):
        raise Unavailable("select a complete molecule id, not an option or path")
    value = read_json([args.cs, "--json", "observe", molecule_id], args.repo)
    row(value)
    if value["id"] != molecule_id:
        raise Unavailable("cs resolved a different molecule; complete exact id required")
    return value


def worktree(args, detail):
    """Join the observed branch to Git's existing worktrees, without path guesses."""
    branch = detail.get("branch")
    if not isinstance(branch, str) or not branch:
        return {"availability": "unavailable", "reason": "cs observe JSON has no branch"}
    try:
        output = run(["git", "worktree", "list", "--porcelain", "-z"], args.repo)
        records = output.decode("utf-8").split("\0\0")
        matches = []
        for record in records:
            fields = dict(field.split(" ", 1) for field in record.split("\0") if " " in field)
            if fields.get("branch") == "refs/heads/" + branch and "worktree" in fields:
                matches.append(Path(fields["worktree"]))
        if len(matches) != 1 or not matches[0].is_dir():
            raise Unavailable("observed branch has no unique existing Git worktree")
        return {"availability": "available", "path": str(matches[0].resolve()), "branch": branch}
    except (Unavailable, UnicodeError, OSError) as error:
        return {"availability": "unavailable", "reason": str(error)}


def artifacts(detail):
    """Inspect conventional deliverables only, beneath the CLI-provided residence."""
    directory = detail.get("molecule_dir")
    if not isinstance(directory, str) or not Path(directory).is_absolute():
        return {"availability": "unavailable", "reason": "missing absolute molecule_dir in cs observe JSON"}
    root = Path(directory)
    try:
        if not root.is_dir():
            raise Unavailable("canonical molecule_dir is missing")
        candidates = [root / name for name in DELIVERABLES]
        responses = root / "responses"
        if responses.is_dir() and not responses.is_symlink():
            candidates.extend(sorted(responses.glob("*.md")))
        files = [
            {"name": str(path.relative_to(root)), "path": str(path)}
            for path in candidates
            if not path.is_symlink() and path.is_file()
        ]
        return {"availability": "available", "directory": str(root), "files": files,
                "missing": [name for name in DELIVERABLES if not any(f["name"] == name for f in files)]}
    except (OSError, Unavailable) as error:
        return {"availability": "unavailable", "reason": str(error)}


def snapshot(args, detail):
    """Keep lifecycle status, terminal presence and filesystem observations separate."""
    return {**row(detail), "updated_at": detail.get("updated_at"),
            "observed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "terminal": {"presence": "unknown", "availability": "unavailable", "reason": TERMINAL_GAP},
            "worktree": worktree(args, detail), "artifacts": artifacts(detail)}


def execute(args):
    """Dispatch only observations and explicit view creation; there is no close hook."""
    if args.command == "list":
        argv = [args.cs, "--json", "observe"] + (["--all"] if args.all else [])
        values = read_json(argv, args.repo)
        if not isinstance(values, list):
            raise Unavailable("cs observe list JSON must be an array")
        return {"molecules": [row(value) for value in values]}
    detail = observe(args, args.id)
    if args.command == "show":
        return snapshot(args, detail)
    if args.terminal:
        # No address is fabricated from worker names, pane labels or private files.
        raise Unavailable(TERMINAL_GAP)
    if args.worktree:
        target = worktree(args, detail)
        if target["availability"] != "available":
            raise Unavailable(target["reason"])
        if not args.herdr_session or not args.workspace:
            raise Unavailable("opening a worktree requires --herdr-session and --workspace")
        output = read_json([args.herdr, "--session", args.herdr_session, "tab", "create",
                            "--workspace", args.workspace, "--cwd", target["path"], "--no-focus"], args.repo)
        return {"id": args.id, "opened": "worktree", "path": target["path"], "herdr": output}
    available = artifacts(detail)
    if available["availability"] != "available":
        raise Unavailable(available["reason"])
    selected = [file for file in available["files"] if file["name"] == args.artifact]
    if len(selected) != 1:
        raise Unavailable("artifact absent or outside the conventional deliverable set")
    if not args.window:
        raise Unavailable("opening an artifact requires an explicit GPUI --window")
    output = read_json([args.herdr_ui, "code", "open", selected[0]["path"] + ":1",
                        "--window", args.window, "--json"], args.repo)
    return {"id": args.id, "opened": "artifact", "path": selected[0]["path"], "herdr_ui": output}


def main():
    """Print one JSON response, including explicit unavailable results (exit 1)."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd(), help="existing galaxy checkout")
    parser.add_argument("--cs", default="cs", help="Cosmon executable")
    parser.add_argument("--herdr", default="herdr", help="daemon CLI executable")
    parser.add_argument("--herdr-ui", default="Herdr", help="GPUI executable (distinct from herdr)")
    commands = parser.add_subparsers(dest="command", required=True)
    listing = commands.add_parser("list", help="one-shot cs observe list; never peek propulsion")
    listing.add_argument("--all", action="store_true")
    showing = commands.add_parser("show", help="exact-id status and existing paths")
    showing.add_argument("id")
    opening = commands.add_parser("open", help="explicit view creation; no lifecycle actions")
    opening.add_argument("id")
    target = opening.add_mutually_exclusive_group(required=True)
    target.add_argument("--worktree", action="store_true")
    target.add_argument("--artifact", metavar="NAME")
    target.add_argument("--terminal", action="store_true", help="reports the missing transport capability")
    opening.add_argument("--herdr-session", help="existing local Herdr daemon session")
    opening.add_argument("--workspace", help="existing workspace for the new terminal tab")
    opening.add_argument("--window", help="explicit GPUI window id for a read-only code tab")
    args = parser.parse_args()
    try:
        result = execute(args)
    except Unavailable as error:
        print(json.dumps({"availability": "unavailable", "reason": str(error)}))
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
