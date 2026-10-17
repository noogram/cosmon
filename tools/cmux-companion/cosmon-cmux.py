#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Optional standalone Cosmon client: observations and fresh cmux views only."""

import argparse
import datetime
import json
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys


DELIVERABLES = ("synthesis.md", "frame.md", "outcomes.md", "plan.md", "report.md", "result.md")


class Unavailable(Exception):
    """A missing backend or observation that cannot safely identify a view."""


def run(argv, cwd):
    """Execute bounded argv without exposing private provider output on failure."""
    try:
        result = subprocess.run(argv, cwd=cwd, capture_output=True, timeout=20)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise Unavailable("command unavailable: " + str(argv[0])) from error
    if result.returncode:
        raise Unavailable(f"command refused: {argv[0]} (exit {result.returncode})")
    return result.stdout


def read_json(argv, cwd):
    """Refuse malformed JSON; never interpret prose as an observation."""
    try:
        return json.loads(run(argv, cwd))
    except (ValueError, UnicodeError) as error:
        raise Unavailable("invalid JSON observation") from error


def cs(args, *command, json_output=True):
    """Pin the selected canonical state directory despite inherited overrides."""
    return [args.cs, "--config", str(args.state)] + (["--json"] if json_output else []) + list(command)


def exact_id(value):
    """Admit the public molecule-id alphabet but never an option or traversal."""
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_.:/@+\-]{1,128}", value) or value.startswith(("-", "/")):
        raise Unavailable("complete molecule id required")
    return value


def configure(args):
    """Resolve Git's canonical checkout, never the companion's source directory."""
    candidate = Path(args.repo).resolve(strict=True)
    output = run(["git", "worktree", "list", "--porcelain", "-z"], candidate).decode()
    first = dict(f.split(" ", 1) for f in output.split("\0\0")[0].split("\0") if " " in f)
    args.repo = Path(first["worktree"]).resolve(strict=True)
    if not (args.repo / ".cosmon" / "config.toml").is_file():
        raise Unavailable("selected checkout has no Cosmon configuration")
    args.state = (args.repo / ".cosmon" / "state").resolve(strict=True)
    if not re.fullmatch(r"[A-Za-z0-9_-]+", args.fleet) or not args.project_id:
        raise Unavailable("explicit project id and simple fleet id required")
    args.cs = shutil.which(args.cs) or args.cs
    args.cmux = shutil.which(args.cmux) or args.cmux


def observe(args, molecule_id):
    """Require the full requested id even if observe accepts prefix resolution."""
    exact_id(molecule_id)
    detail = read_json(cs(args, "observe", molecule_id), args.repo)
    if not isinstance(detail, dict) or detail.get("id") != molecule_id or not isinstance(detail.get("status"), str):
        raise Unavailable("observe did not return the exact selected molecule")
    return detail


def terminal(args, molecule_id):
    """Independently validate the public v1 contract; addresses remain opaque."""
    exact_id(molecule_id)
    try:
        value = read_json(cs(args, "terminal", molecule_id), args.repo)
    except Unavailable:
        return {"availability": "unknown", "reason": "public terminal observation unavailable"}
    if not isinstance(value, dict) or type(value.get("schema_version")) is not int or value["schema_version"] != 1 or value.get("kind") != "cosmon.terminal":
        raise Unavailable("unsupported terminal contract")
    if any(value.get(key) != expected for key, expected in {
        "id": molecule_id, "project_root": str(args.repo), "state_dir": str(args.state),
        "project_id": args.project_id, "fleet_id": args.fleet,
    }.items()):
        raise Unavailable("terminal identifies a different molecule, project or fleet")
    availability = value.get("availability")
    if availability not in ("available", "unknown", "unavailable"):
        raise Unavailable("invalid terminal availability")
    if availability == "available":
        if value.get("capability") != "read_only_ignore_size" or not isinstance(value.get("identity"), dict) or not isinstance(value.get("expected"), str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", value["expected"]):
            raise Unavailable("unsupported terminal capability")
    elif any(key in value for key in ("capability", "identity", "expected")) or not isinstance(value.get("reason"), str):
        raise Unavailable("invalid non-available terminal observation")
    # Do not disclose backend addresses in the selector or reconstruct a token.
    return {key: value[key] for key in ("availability", "reason", "expected") if key in value}


def molecules(args, all_rows=False):
    """Consume the versioned ensemble list without propulsion or state-file reads."""
    value = read_json(cs(args, "ensemble"), args.repo)
    if not isinstance(value, dict) or type(value.get("schema_version")) is not int or value["schema_version"] != 1 or not isinstance(value.get("molecule_states"), list):
        raise Unavailable("unsupported ensemble contract")
    rows = []
    for item in value["molecule_states"]:
        if not isinstance(item, dict) or not isinstance(item.get("status"), str):
            raise Unavailable("invalid ensemble row")
        exact_id(item.get("id"))
        if item.get("fleet") != args.fleet:
            continue
        if all_rows or item["status"] in ("running", "pending", "frozen"):
            rows.append({key: item.get(key) for key in ("id", "status", "phase", "updated_at")})
    return rows


def worktree(args, detail):
    """Join an observed branch to a unique existing Git worktree, with no guesses."""
    branch = detail.get("branch")
    if not isinstance(branch, str) or not branch:
        return {"availability": "unavailable", "reason": "no observed branch"}
    records = run(["git", "worktree", "list", "--porcelain", "-z"], args.repo).decode().split("\0\0")
    matches = []
    for record in records:
        fields = dict(f.split(" ", 1) for f in record.split("\0") if " " in f)
        if fields.get("branch") == "refs/heads/" + branch and "worktree" in fields:
            path = Path(fields["worktree"])
            if path.is_dir():
                matches.append(str(path.resolve()))
    if len(matches) != 1:
        return {"availability": "unavailable", "reason": "no unique existing worktree"}
    return {"availability": "available", "path": matches[0]}


def artifacts(args, detail):
    """Offer conventional Markdown files only under the CLI-provided residence."""
    directory = detail.get("molecule_dir")
    if not isinstance(directory, str) or not Path(directory).is_absolute():
        return {"availability": "unavailable", "reason": "no absolute molecule_dir"}
    root = Path(directory)
    # Canonical local residence must belong to the validated state/fleet/id.
    expected = args.state / "fleets" / args.fleet / "molecules" / detail["id"]
    if root != expected or root.is_symlink() or not root.is_dir() or root.resolve() != expected:
        return {"availability": "unavailable", "reason": "canonical molecule residence unavailable"}
    candidates = [root / name for name in DELIVERABLES]
    responses = root / "responses"
    if responses.is_dir() and not responses.is_symlink():
        candidates.extend(sorted(responses.glob("*.md")))
    files = [{"name": str(p.relative_to(root)), "path": str(p)} for p in candidates if p.is_file() and not p.is_symlink()]
    return {"availability": "available", "files": files}


def cmux_rpc(args, method, params):
    """Use cmux's documented JSON RPC CLI, inheriting its configured socket/auth."""
    argv = [args.cmux] + (["--socket", args.socket] if args.socket else [])
    return read_json(argv + ["--json", "rpc", method, json.dumps(params)], args.repo)


def open_view(args, argv, cwd, title):
    """Only create fresh workspaces; quote argv for cmux's initial shell command."""
    capability = cmux_rpc(args, "system.capabilities", {})
    if not isinstance(capability, dict) or capability.get("protocol") != "cmux-socket" or type(capability.get("version")) is not int or capability["version"] != 2 or not isinstance(capability.get("methods"), list) or "workspace.create" not in capability["methods"]:
        raise Unavailable("unsupported cmux workspace capability")
    # cmux has a command string, not an argv PTY API. Never interpolate labels,
    # paths or opaque tokens unquoted; never send text to an existing surface.
    params = {"cwd": str(cwd), "title": title, "initial_command": "exec " + shlex.join([str(x) for x in argv]), "focus": False}
    value = cmux_rpc(args, "workspace.create", params)
    if not isinstance(value, dict) or not isinstance(value.get("workspace_id"), str) or not isinstance(value.get("surface_id"), str):
        raise Unavailable("cmux did not identify the created workspace/surface")
    return {"opened": title, "cmux": value}


def self_argv(args, *command):
    """Carry explicit project and executable selection into the new local view."""
    argv = [sys.executable, str(Path(__file__).resolve()), "--repo", str(args.repo),
            "--project-id", args.project_id, "--fleet", args.fleet, "--cs", args.cs, "--cmux", args.cmux]
    return argv + (["--socket", args.socket] if args.socket else []) + list(command)


def execute(args):
    """Expose only read observations and explicit creation of independent views."""
    if args.command == "view-terminal":
        exact_id(args.id)
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", args.expected):
            raise Unavailable("invalid expected token")
        # Inherit this new cmux PTY unchanged. The public renderer owns its input
        # handling; no captured stream, key forwarding or private address lookup.
        status = subprocess.call(cs(args, "terminal", args.id, "--attach", "--expect", args.expected, json_output=False), cwd=args.repo)
        if status:
            print(f"Terminal attachment refused (exit {status}). Reopen for a fresh observation.", flush=True)
            input("Enter to close: ")
        return {"attachment_exit": status}
    if args.command == "list":
        return {"molecules": molecules(args, args.all)}
    if args.command == "launch":
        return open_view(args, self_argv(args, "select"), args.repo, "Cosmon selector")
    detail = observe(args, args.id)
    observed_terminal = terminal(args, args.id)
    if args.command == "show":
        return {"id": args.id, "status": detail["status"], "observed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                "terminal": observed_terminal, "worktree": worktree(args, detail), "artifacts": artifacts(args, detail)}
    if getattr(args, "terminal", False):
        if observed_terminal["availability"] != "available":
            raise Unavailable("terminal " + observed_terminal["availability"] + ": " + observed_terminal["reason"])
        argv = self_argv(args, "view-terminal", args.id, observed_terminal["expected"])
        return open_view(args, argv, args.repo, "Cosmon worker (read-only)")
    if getattr(args, "worktree", False):
        target = worktree(args, detail)
        if target["availability"] != "available":
            raise Unavailable(target["reason"])
        return open_view(args, ["/bin/sh", "-i"], target["path"], "Cosmon worktree shell")
    files = artifacts(args, detail).get("files", [])
    paths = [f["path"] for f in files if f["name"] == args.artifact]
    if len(paths) != 1:
        raise Unavailable("artifact absent or outside conventional deliverables")
    if args.command == "read-artifact":
        print(safe_text(Path(paths[0]).read_text(errors="replace")))
        input("\nRead-only artifact. Enter to close: ")
        return {"read": args.artifact}
    return open_view(args, self_argv(args, "read-artifact", args.id, args.artifact), args.repo, "Cosmon artifact (read-only)")


def safe_text(value):
    """Render control bytes from untrusted labels or artifacts as inert text."""
    return "".join(c if c.isprintable() or c in "\n\t" else repr(c)[1:-1] for c in str(value))


def select(args):
    """Numbered active-first selector; refresh only on the user's action."""
    all_rows = False
    while True:
        rows = molecules(args, all_rows)
        print("\nCosmon in cmux — " + safe_text(args.repo))
        for number, item in enumerate(rows, 1):
            print(f"{number}. {safe_text(item['id'])}  {safe_text(item['status'])}")
        choice = input("Molecule number, r refresh, h toggle history, q quit: ").strip().lower()
        if choice == "q":
            return
        if choice == "h":
            all_rows = not all_rows
        if not choice.isdigit() or not 1 <= int(choice) <= len(rows):
            continue
        args.id = rows[int(choice) - 1]["id"]
        while True:
            try:
                args.command = "show"
                value = execute(args)
                print(safe_text(args.id) + "  " + safe_text(value["status"]))
                print("Terminal: " + safe_text(value["terminal"]["availability"]) + " " + safe_text(value["terminal"].get("reason", "")))
                print("Worktree: " + safe_text(value["worktree"].get("path", value["worktree"].get("reason"))))
                files = value["artifacts"].get("files", [])
                for number, file in enumerate(files, 1):
                    print(f"  {number}. {safe_text(file['name'])}")
                action = input("t read-only worker, w worktree shell, a artifact, b back, q quit: ").strip().lower()
                if action == "q":
                    return
                if action == "b":
                    break
                args.command, args.terminal, args.worktree, args.artifact = "open", action == "t", action == "w", None
                if action == "a":
                    number = input("Artifact number: ").strip()
                    if not number.isdigit() or not 1 <= int(number) <= len(files):
                        continue
                    args.artifact = files[int(number) - 1]["name"]
                elif action not in ("t", "w"):
                    continue
                print(safe_text(json.dumps(execute(args))))
            except Unavailable as error:
                print("Unavailable: " + safe_text(error))


def main():
    """Require explicit project identity and return JSON for one-shot commands."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, type=Path)
    parser.add_argument("--project-id", required=True, help="expected [project].project_id from the galaxy config")
    parser.add_argument("--fleet", default="default")
    parser.add_argument("--cs", default="cs")
    parser.add_argument("--cmux", default="cmux")
    parser.add_argument("--socket", help="optional explicit cmux socket; otherwise cmux discovery/auth")
    commands = parser.add_subparsers(dest="command", required=True)
    listing = commands.add_parser("list")
    listing.add_argument("--all", action="store_true")
    showing = commands.add_parser("show")
    showing.add_argument("id")
    opening = commands.add_parser("open")
    opening.add_argument("id")
    target = opening.add_mutually_exclusive_group(required=True)
    target.add_argument("--terminal", action="store_true")
    target.add_argument("--worktree", action="store_true")
    target.add_argument("--artifact")
    commands.add_parser("launch")
    commands.add_parser("select")
    viewing = commands.add_parser("view-terminal", help=argparse.SUPPRESS)
    viewing.add_argument("id")
    viewing.add_argument("expected")
    reading = commands.add_parser("read-artifact", help=argparse.SUPPRESS)
    reading.add_argument("id")
    reading.add_argument("artifact")
    args = parser.parse_args()
    try:
        configure(args)
        if args.command == "select":
            select(args)
            return 0
        print(json.dumps(execute(args), indent=2))
        return 0
    except (Unavailable, OSError, ValueError, KeyError, UnicodeError, EOFError, KeyboardInterrupt) as error:
        print(json.dumps({"availability": "unavailable", "reason": str(error)}))
        return 1


if __name__ == "__main__":
    sys.exit(main())
