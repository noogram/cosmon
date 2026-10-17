#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Optional, one-shot Cosmon reader and explicit Herdr view opener.

Only supported CLI observations supply molecule identity and residence. No
Cosmon state file is parsed; no lifecycle command is sent. Input requires an explicit interactive view.
"""

import argparse
import datetime
import json
import os
import shutil
import signal
import socket
from pathlib import Path
import re
import subprocess
import sys
import time


DELIVERABLES = ("synthesis.md", "frame.md", "outcomes.md", "plan.md", "report.md", "result.md")



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
    value = read_json(cs_argv(args, "observe", molecule_id), args.repo)
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
            "terminal": terminal(args, detail["id"]),
            "worktree": worktree(args, detail), "artifacts": artifacts(detail)}


def execute(args):
    """Dispatch only observations and explicit view creation; there is no close hook."""
    if args.command == "list":
        argv = cs_argv(args, "observe") + (["--all"] if args.all else [])
        values = read_json(argv, args.repo)
        if not isinstance(values, list):
            raise Unavailable("cs observe list JSON must be an array")
        return {"molecules": [row(value) for value in values]}
    detail = observe(args, args.id)
    if args.command == "show":
        return snapshot(args, detail)
    if args.terminal or getattr(args, "interact", False):
        interactive = getattr(args, "interact", False)
        value = terminal(args, args.id, interactive=interactive)
        if value["availability"] != "available":
            raise Unavailable("terminal " + value["availability"] + ": " + value.get("reason", "unknown"))
        argv = view_argv(args, value["expected"], interactive)
        return open_argv(args, argv, "Cosmon view")
    if args.worktree:
        target = worktree(args, detail)
        if target["availability"] != "available":
            raise Unavailable(target["reason"])
        if not args.workspace:
            raise Unavailable("opening a worktree requires a workspace")
        output = read_json(herdr_argv(args, "tab", "create") + [
                            "--workspace", args.workspace, "--cwd", target["path"], "--no-focus"], args.repo)
        return {"id": args.id, "opened": "worktree", "path": target["path"], "herdr": output}
    available = artifacts(detail)
    if available["availability"] != "available":
        raise Unavailable(available["reason"])
    selected = [file for file in available["files"] if file["name"] == args.artifact]
    if len(selected) != 1:
        raise Unavailable("artifact absent or outside the conventional deliverable set")
    if not args.window:
        argv = [sys.executable, str(Path(__file__).resolve()), "--repo", str(args.repo),
                "--cs", args.cs, "read-artifact", args.id, args.artifact]
        return open_argv(args, argv, "Cosmon artifact (read-only)")
    output = read_json([args.herdr_ui, "code", "open", selected[0]["path"] + ":1",
                        "--window", args.window, "--json"], args.repo)
    return {"id": args.id, "opened": "artifact", "path": selected[0]["path"], "herdr_ui": output}


def safe_text(value):
    """Keep terminal control bytes in untrusted display content inert."""
    return "".join(c if c.isprintable() or c in "\n\t" else repr(c)[1:-1] for c in str(value))


def cs_argv(args, *command, json_output=True):
    """Pin the selected galaxy explicitly so inherited state overrides cannot redirect it."""
    return [args.cs, "--config", str(args.repo / ".cosmon" / "state")] + (["--json"] if json_output else []) + list(command)


def herdr_argv(args, *command):
    """The context socket stays inherited; an explicit session is never guessed."""
    return [args.herdr] + (["--session", args.herdr_session] if args.herdr_session else []) + list(command)


def galaxy(path):
    """Validate Git's canonical main checkout, including invocation from its worktrees."""
    candidate = Path(path).resolve(strict=True)
    if not candidate.is_dir():
        raise Unavailable("project context is not a directory")
    records = run(["git", "worktree", "list", "--porcelain", "-z"], candidate).decode().split("\0\0")
    first = dict(f.split(" ", 1) for f in records[0].split("\0") if " " in f)
    root = Path(first.get("worktree", "")).resolve(strict=True)
    if not (root / ".cosmon" / "config.toml").is_file():
        raise Unavailable("project has no Cosmon galaxy configuration")
    return root


def configure(args):
    """Use actual Herdr context fields; never select the plugin source cwd as a galaxy."""
    context = json.loads(os.environ.get("HERDR_PLUGIN_CONTEXT_JSON", "{}"))
    if not isinstance(context, dict):
        raise Unavailable("invalid plugin context")
    explicit = args.repo or os.environ.get("COSMON_CLIENT_REPO")
    if explicit:
        args.repo = galaxy(explicit)
    elif os.environ.get("HERDR_PLUGIN_ID") or context:
        roots = set()
        for key in ("workspace_cwd", "focused_pane_cwd"):
            value = context.get(key)
            if value:
                try:
                    roots.add(galaxy(value))
                except (Unavailable, OSError):
                    continue
        if len(roots) != 1:
            raise Unavailable("plugin project context missing or ambiguous; supply --repo")
        args.repo = roots.pop()
    else:
        args.repo = galaxy(Path.cwd())
    args.cs = shutil.which(args.cs) or args.cs
    env_workspace = os.environ.get("HERDR_WORKSPACE_ID")
    context_workspace = context.get("workspace_id")
    if not args.workspace and env_workspace and context_workspace and env_workspace != context_workspace:
        raise Unavailable("plugin workspace context disagrees")
    args.workspace = args.workspace or env_workspace or context_workspace


def terminal(args, molecule_id, interactive=False):
    """Consume only the versioned public capability; unknown is never an address."""
    try:
        value = read_json(cs_argv(args, "terminal", molecule_id, *(["--mode", "interactive"] if interactive else [])), args.repo)
    except Unavailable:
        return {"availability": "unknown", "reason": "public terminal capability unavailable"}
    if not isinstance(value, dict) or value.get("kind") != ("cosmon.terminal.interactive" if interactive else "cosmon.terminal") or type(value.get("schema_version")) is not int or value["schema_version"] != 1:
        raise Unavailable("unsupported terminal contract")
    if value.get("id") != molecule_id or value.get("project_root") != str(args.repo) or value.get("state_dir") != str((args.repo / ".cosmon" / "state").resolve()):
        raise Unavailable("terminal observation identifies a different molecule or project")
    availability = value.get("availability")
    if availability not in ("available", "unknown", "unavailable"):
        raise Unavailable("invalid terminal availability")
    if availability == "available" and (value.get("capability") != ("interactive_resize" if interactive else "read_only_ignore_size") or not isinstance(value.get("expected"), str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", value["expected"])):
        raise Unavailable("unsupported terminal attachment capability")
    return value


def view_argv(args, expected, interactive=False):
    """Pin a view to this project/dispatch; restore always drops input capability."""
    return [sys.executable, str(Path(__file__).resolve()), "--repo", str(args.repo),
            "--cs", args.cs, "--herdr", args.herdr,
            *(["--herdr-session", args.herdr_session] if getattr(args, "herdr_session", None) else []),
            "view", args.id, "--expect", expected,
            *(["--interact"] if interactive else [])]


def valid_resume(argv):
    """Herdr restore accepts a plain executable name and bounded safe arguments."""
    return (0 < len(argv) <= 64 and re.fullmatch(r"[A-Za-z0-9_.-]+", argv[0]) is not None and not argv[0].startswith("-")
            and sum(len(a.encode()) for a in argv) <= 8192
            and all("'" not in a and not any(ord(c) < 32 or 127 <= ord(c) < 160 for c in a) for a in argv))


def worker_view(args):
    """Report a native Cosmon view and reconnect only an exact existing worker.

    Supported CLI observations do not expose harness semantic state, so every
    report is unknown. Reports fail independently of the worker and view.
    """
    observe(args, args.id)
    value = terminal(args, args.id, interactive=args.interact)
    if value.get("availability") != "available" or value.get("expected") != args.expect:
        raise Unavailable("terminal incarnation token changed or unavailable")
    pane = (os.environ.get("HERDR_PANE_ID") if os.environ.get("HERDR_ENV") == "1"
            and os.environ.get("HERDR_SOCKET_PATH") and os.environ.get("HERDR_BIN_PATH") else None)
    seq = time.time_ns()
    def report(command, *fields):
        nonlocal seq
        seq += 1
        if not pane:
            return False
        try:
            result = subprocess.run(herdr_argv(args, "pane", command, pane, "--source", "custom:cosmon-view",
                                     "--agent", "Cosmon", "--seq", str(seq), *fields),
                           cwd=args.repo, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=2)
            return result.returncode == 0
        except (OSError, subprocess.TimeoutExpired):
            return False
    restore = view_argv(args, args.expect)
    restore[0] = "python3"  # Herdr requires a plain PATH command as argv[0].
    report("report-agent", "--state", "unknown", "--message",
           "Cosmon view (interactive)" if args.interact else "Cosmon view (read-only); worker semantic state unknown")
    registered = report("report-agent-session", "--agent-session-id", args.expect,
                        *(["--", *restore] if valid_resume(restore) else []))
    if pane and (not valid_resume(restore) or not registered):
        print("Cosmon: restore registration unavailable", file=sys.stderr)
    child = None
    def close_view(_signum, _frame):
        if child is not None:
            child.terminate()
    previous = {sig: signal.signal(sig, close_view) for sig in (signal.SIGHUP, signal.SIGTERM)}
    try:
        argv = cs_argv(args, "terminal", args.id, "--attach", "--expect", args.expect,
                       *(["--mode", "interactive"] if args.interact else []), json_output=False)
        child = subprocess.Popen(argv, cwd=args.repo)
        return child.wait()
    finally:
        if child is not None and child.poll() is None:
            child.terminate()
            child.wait()
        report("release-agent")
        for sig, handler in previous.items():
            signal.signal(sig, handler)


def api(args, method, params):
    """Use Herdr's documented newline JSON socket API, not shell command injection."""
    status = read_json(herdr_argv(args, "status", "server", "--json"), args.repo)
    address = status.get("socket")
    if not status.get("running") or not isinstance(address, str):
        raise Unavailable("Herdr server/socket unavailable")
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(20)
            client.connect(address)
            client.sendall(json.dumps({"id": "cosmon-view", "method": method, "params": params}).encode() + b"\n")
            with client.makefile("rb") as stream:
                reply = json.loads(stream.readline(4 * 1024 * 1024))
    except (OSError, ValueError) as error:
        raise Unavailable("Herdr socket request unavailable") from error
    if "error" in reply or "result" not in reply:
        raise Unavailable("Herdr refused " + method)
    return reply.get("result")


def open_argv(args, argv, title):
    """Create a NEW tab with a real argv PTY process; never replace an existing tab."""
    if not args.workspace:
        raise Unavailable("opening a terminal view requires an existing workspace")
    return api(args, "layout.apply", {"workspace_id": args.workspace, "tab_label": title,
        "focus": True, "root": {"type": "pane", "cwd": str(args.repo), "command": argv}})


def select(args):
    """Numbered selector: no opaque id copying, polling, web server or lifecycle controls."""
    while True:
        args.command, args.all = "list", True
        values = execute(args)["molecules"]
        print("\nClient integrations — Cosmon in Herdr\n" + safe_text(args.repo))
        for index, value in enumerate(values, 1):
            print(f"{index}. {safe_text(value['id'])}  {safe_text(value['status'])}")
        choice = input("Molecule number, r refresh, q quit: ").strip()
        if choice.lower() == "q":
            return 0
        if not choice.isdigit() or not 1 <= int(choice) <= len(values):
            continue
        args.id = values[int(choice) - 1]["id"]
        while True:
            try:
                # Re-observe the selected id for each rendering and each action.
                detail = observe(args, args.id)
                view = snapshot(args, detail)
                print(safe_text(json.dumps(view, indent=2)))
                print("t terminal (read-only), i Interagir (input/output), w worktree shell, a artifact, b back, q quit")
                action = input("Action: ").strip().lower()
                if action == "q":
                    return 0
                if action == "b":
                    break
                args.command = "open"
                args.terminal, args.interact, args.worktree, args.artifact, args.window = action == "t", action == "i", action == "w", None, None
                if action == "a":
                    files = view["artifacts"].get("files", [])
                    for index, file in enumerate(files, 1):
                        print(f"{index}. {safe_text(file['name'])}")
                    selected = input("Artifact number: ").strip()
                    if not selected.isdigit() or not 1 <= int(selected) <= len(files):
                        continue
                    args.artifact = files[int(selected) - 1]["name"]
                elif action not in ("t", "i", "w"):
                    continue
                print(safe_text(json.dumps(execute(args))))
            except Unavailable as error:
                print("Unavailable: " + safe_text(error))


def main():
    """Print one JSON response, including explicit unavailable results (exit 1)."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, help="existing galaxy checkout; otherwise validated plugin context")
    parser.add_argument("--cs", default=os.environ.get("COSMON_CLIENT_CS", "cs"), help="Cosmon executable")
    parser.add_argument("--herdr", default=os.environ.get("HERDR_BIN_PATH", "herdr"), help="daemon CLI executable")
    parser.add_argument("--herdr-ui", default="Herdr", help="GPUI executable (distinct from herdr)")
    parser.add_argument("--herdr-session", help="existing local daemon session (or plugin socket context)")
    parser.add_argument("--workspace", help="existing workspace (or plugin context)")
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
    target.add_argument("--interact", action="store_true", help="Interagir: explicitly open human input/output to this incarnation")
    target.add_argument("--terminal", action="store_true", help="open the launch-bound worker terminal read-only")
    opening.add_argument("--herdr-session", default=argparse.SUPPRESS, help="existing local Herdr daemon session")
    opening.add_argument("--workspace", default=argparse.SUPPRESS, help="existing workspace for the new terminal tab")
    opening.add_argument("--window", help="explicit GPUI window id for a read-only code tab")
    viewing = commands.add_parser("view", help="pinned view process; restores read-only, never launches a worker")
    viewing.add_argument("id")
    viewing.add_argument("--expect", required=True)
    viewing.add_argument("--interact", action="store_true", help="explicit human input; omitted on restore")
    commands.add_parser("select", help="interactive numbered molecule and action selector")
    commands.add_parser("launch", help="plugin action: open selector pane in current workspace")
    reading = commands.add_parser("read-artifact", help=argparse.SUPPRESS)
    reading.add_argument("id")
    reading.add_argument("artifact")
    args = parser.parse_args()
    try:
        configure(args)
        if args.command == "view":
            return worker_view(args)
        if args.command == "select":
            return select(args)
        if args.command == "launch":
            if not args.workspace:
                raise Unavailable("plugin action requires a workspace")
            result = read_json(herdr_argv(args, "plugin", "pane", "open", "--plugin", "cosmon.client", "--entrypoint", "selector", "--placement", "tab", "--workspace", args.workspace, "--env", "COSMON_CLIENT_REPO=" + str(args.repo), "--env", "COSMON_CLIENT_CS=" + args.cs, "--focus"), args.repo)
        elif args.command == "read-artifact":
            detail = observe(args, args.id)
            choices = artifacts(detail).get("files", [])
            paths = [f["path"] for f in choices if f["name"] == args.artifact]
            if len(paths) != 1:
                raise Unavailable("artifact unavailable")
            print(safe_text(Path(paths[0]).read_text(errors="replace")))
            input("\nRead-only artifact. Enter to close.")
            return 0
        else:
            result = execute(args)
    except (Unavailable, OSError, ValueError, EOFError, KeyboardInterrupt) as error:
        print(json.dumps({"availability": "unavailable", "reason": str(error)}))
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
