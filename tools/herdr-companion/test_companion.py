# SPDX-License-Identifier: AGPL-3.0-only
"""Subprocess witnesses: real Git worktrees, recording Cosmon/Herdr executables."""

import json
import importlib.util
from types import SimpleNamespace
from unittest.mock import patch
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


COMPANION = Path(__file__).with_name("cosmon-herdr.py")
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("companion", COMPANION)
companion = importlib.util.module_from_spec(spec)
spec.loader.exec_module(companion)


class CompanionTest(unittest.TestCase):
    """Keep dangerous near-matches and missing capabilities outside the open path."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.repo = self.root / "repo"
        self.repo.mkdir()
        (self.repo / ".cosmon").mkdir()
        (self.repo / ".cosmon" / "config.toml").write_text('[project]\nproject_id="fixture"\n')
        self.git("init", "-q")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "commit", "--allow-empty", "-qm", "fixture")
        # A filename and branch carrying shell syntax must stay literal argv.
        self.checkout = self.root / "checkout spaces;$(touch SENTINEL)"
        self.branch = "feat/task-example-0001"
        self.git("worktree", "add", "-qb", self.branch, str(self.checkout))
        self.artifact_dir = self.root / "canonical spaces;$(touch SENTINEL)"
        self.artifact_dir.mkdir()
        (self.artifact_dir / "synthesis.md").write_text("fixture result\n")
        (self.artifact_dir / "state.json").write_text("never exposed\n")
        (self.artifact_dir / "responses").mkdir()
        (self.artifact_dir / "responses" / "review.md").write_text("fixture review\n")
        (self.artifact_dir / "result.md").symlink_to(self.repo / ".git" / "HEAD")
        self.detail = {"id": "task-example-0001", "status": "completed", "worker": "stale label",
                       "branch": self.branch, "molecule_dir": str(self.artifact_dir),
                       "variables": {"topic": "PRIVATE-SOURCE-DO-NOT-DISPLAY"}}
        self.fixture = self.root / "fixture.json"
        self.log = self.root / "calls.jsonl"
        self.cs = self.root / "cs"
        self.herdr = self.root / "herdr"
        script = '''#!PYTHON
import json, os, pathlib, sys
args = sys.argv[1:]
with open(os.environ["COMPANION_CALL_LOG"], "a") as log:
    log.write(json.dumps([pathlib.Path(sys.argv[0]).name, args]) + "\\n")
data = json.loads(pathlib.Path(os.environ["COMPANION_FIXTURE"]).read_text())
if pathlib.Path(sys.argv[0]).name == "cs":
    assert args[:2] == ["--config", str(pathlib.Path.cwd() / ".cosmon" / "state")]
    args = args[2:]
    if args == ["--json", "observe"] or args == ["--json", "observe", "--all"]:
        print(json.dumps([data]))
    elif args == ["--json", "observe", "task-example-0001"]:
        print(json.dumps(data))
    elif args == ["--json", "terminal", "task-example-0001"] and "_terminal" in data:
        print(json.dumps(data["_terminal"]))
    else:
        sys.exit(91)  # Witness refuses every mutation, peek, or unexpected target.
else:
    print(json.dumps({"tab_id": "wTEST:t1", "pane_id": "wTEST:p1"}))
'''.replace("PYTHON", sys.executable)
        for binary in (self.cs, self.herdr):
            binary.write_text(script)
            binary.chmod(0o755)
        self.env = {**os.environ, "COMPANION_CALL_LOG": str(self.log),
                    "COMPANION_FIXTURE": str(self.fixture)}

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.repo), *args], check=True,
                              capture_output=True).stdout

    def invoke(self, *args, expected=0, herdr=None):
        self.fixture.write_text(json.dumps(self.detail))
        p = subprocess.run([sys.executable, str(COMPANION), "--repo", str(self.repo),
                            "--cs", str(self.cs), "--herdr", str(herdr or self.herdr),
                            "--herdr-ui", str(herdr or self.herdr), *args],
                           capture_output=True, text=True, env=self.env, timeout=30)
        self.assertEqual(p.returncode, expected, p.stderr + p.stdout)
        self.assertNotIn("PRIVATE-SOURCE-DO-NOT-DISPLAY", p.stdout)
        self.assertFalse((self.repo / "SENTINEL").exists())
        return json.loads(p.stdout)

    def calls(self):
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        # The executable itself asserts explicit project pinning above.
        return [[name, argv[2:] if name == "cs" else argv] for name, argv in calls]

    def test_observations_never_propel_or_invoke_herdr_and_leave_files_unchanged(self):
        before = {p: p.read_bytes() for p in self.artifact_dir.rglob("*") if p.is_file()}
        self.assertEqual(self.invoke("list")["molecules"][0]["status"], "completed")
        view = self.invoke("show", self.detail["id"])
        self.assertEqual(view["status"], "completed")
        self.assertEqual(view["terminal"]["availability"], "unknown")
        self.assertEqual(view["worktree"]["path"], str(self.checkout))
        self.assertEqual([f["name"] for f in view["artifacts"]["files"]],
                         ["synthesis.md", "responses/review.md"])
        self.assertEqual(before, {p: p.read_bytes() for p in before})
        self.assertEqual(self.calls(), [["cs", ["--json", "observe"]],
                                       ["cs", ["--json", "observe", self.detail["id"]]],
                                       ["cs", ["--json", "terminal", self.detail["id"]]]])

    def test_prefix_or_different_identity_cannot_open_a_view(self):
        self.detail["id"] = "task-example-0001-longer"
        result = self.invoke("open", "task-example-0001", "--worktree",
                             "--herdr-session", "fixture", "--workspace", "wTEST", expected=1)
        self.assertIn("different molecule", result["reason"])
        self.assertEqual(len(self.calls()), 1)

    def test_branch_join_is_exact_and_missing_worktree_never_created(self):
        self.detail["branch"] += "-neighbor"
        self.assertEqual(self.invoke("show", self.detail["id"])["worktree"]["availability"], "unavailable")
        self.invoke("open", self.detail["id"], "--worktree", expected=1)
        self.assertTrue(all(call[0] == "cs" for call in self.calls()))

    def test_missing_branch_and_molecule_directory_stay_explicit(self):
        del self.detail["branch"]
        del self.detail["molecule_dir"]
        view = self.invoke("show", self.detail["id"])
        self.assertIn("branch", view["worktree"]["reason"])
        self.assertIn("molecule_dir", view["artifacts"]["reason"])
        self.detail["molecule_dir"] = str(self.root / "absent")
        self.assertIn("missing", self.invoke("show", self.detail["id"])["artifacts"]["reason"])

    def test_terminal_is_unavailable_even_with_live_status_and_bait_addresses(self):
        self.detail.update(status="running", session_name="stale", transport={"backend": "tmux"})
        result = self.invoke("open", self.detail["id"], "--terminal", expected=1)
        self.assertIn("capability", result["reason"])
        self.assertEqual(len(self.calls()), 2)

    def test_worktree_open_uses_exact_argv_and_no_close_or_worker_command(self):
        self.invoke("open", self.detail["id"], "--worktree", "--herdr-session", "fixture",
                    "--workspace", "wTEST")
        self.assertEqual(self.calls()[-1], ["herdr", ["--session", "fixture", "tab", "create",
                          "--workspace", "wTEST", "--cwd", str(self.checkout), "--no-focus"]])

    def test_artifact_open_is_read_only_argv_and_requires_explicit_window(self):
        self.invoke("open", self.detail["id"], "--artifact", "synthesis.md", expected=1)
        self.assertEqual(len(self.calls()), 1)
        self.invoke("open", self.detail["id"], "--artifact", "synthesis.md", "--window", "42")
        self.assertEqual(self.calls()[-1], ["herdr", ["code", "open",
                          str(self.artifact_dir / "synthesis.md") + ":1", "--window", "42", "--json"]])

    def test_traversal_private_files_and_symlinks_cannot_be_opened(self):
        for name in ("../repo/.git/HEAD", "state.json", "result.md"):
            self.invoke("open", self.detail["id"], "--artifact", name, "--window", "42", expected=1)
        self.assertTrue(all(call[0] == "cs" for call in self.calls()))

    def test_backend_absence_and_refusal_are_reported_without_state_change(self):
        result = self.invoke("open", self.detail["id"], "--artifact", "synthesis.md",
                             "--window", "42", herdr=self.root / "absent", expected=1)
        self.assertIn("backend unavailable", result["reason"])
        self.herdr.write_text("#!/bin/sh\nexit 3\n")
        result = self.invoke("open", self.detail["id"], "--artifact", "synthesis.md",
                             "--window", "42", expected=1)
        self.assertIn("exit 3", result["reason"])

    def public_terminal(self):
        return {"schema_version": 1, "kind": "cosmon.terminal", "id": self.detail["id"],
                "project_root": str(self.repo), "state_dir": str(self.repo / ".cosmon" / "state"),
                "availability": "available", "capability": "read_only_ignore_size",
                "expected": "sha256:" + "a" * 64, "additional_future_field": {"ignored": True}}

    def test_public_terminal_contract_additive_fields_and_exact_attach_argv(self):
        self.detail["_terminal"] = self.public_terminal()
        self.fixture.write_text(json.dumps(self.detail))
        args = SimpleNamespace(cs=str(self.cs), repo=self.repo, command="open", id=self.detail["id"],
                               terminal=True, workspace="wTEST", herdr=str(self.herdr))
        with patch.dict(os.environ, self.env), patch.object(companion, "open_argv", return_value={"created": True}) as opening:
            self.assertEqual(companion.execute(args), {"created": True})
        argv = opening.call_args.args[1]
        self.assertEqual(argv, [sys.executable, str(COMPANION), "--repo", str(self.repo),
                         "--cs", str(self.cs), "--herdr", str(self.herdr),
                         "view", self.detail["id"], "--expect", "sha256:" + "a" * 64])
        self.assertNotIn("--interact", argv)
        self.assertEqual(self.calls(), [["cs", ["--json", "observe", self.detail["id"]]],
                                       ["cs", ["--json", "terminal", self.detail["id"]]]])

    def test_restore_drops_input_and_never_launches_a_harness(self):
        args = SimpleNamespace(repo=self.repo, cs=str(self.cs), herdr=str(self.herdr),
                               id=self.detail["id"], herdr_session=None)
        explicit = companion.view_argv(args, "sha256:" + "a" * 64, True)
        restore = companion.view_argv(args, "sha256:" + "a" * 64)
        self.assertIn("--interact", explicit)
        self.assertNotIn("--interact", restore)
        self.assertEqual(restore, explicit[:-1])
        self.assertNotIn("start", restore)
        self.assertNotIn("resume", restore)
        restore[0] = "python3"
        self.assertTrue(companion.valid_resume(restore))
        for bad in (["/bin/python3"], ["python3", "bad'path"], ["python3", "bad\npath"],
                    ["python3"] + ["a"] * 64, ["python3", "x" * 8193], ["-python3"], ["python3", "bad\x85path"]):
            self.assertFalse(companion.valid_resume(bad))

    def test_native_reports_are_unknown_and_release_and_restore_is_readonly(self):
        args = SimpleNamespace(repo=self.repo, cs=str(self.cs), herdr=str(self.herdr),
                               id=self.detail["id"], herdr_session=None, interact=True,
                               expect="sha256:" + "a" * 64)
        value = {"availability": "available", "expected": args.expect}
        with patch.object(companion, "observe"), patch.object(companion, "terminal", return_value=value), \
                patch.dict(os.environ, {"HERDR_ENV": "1", "HERDR_PANE_ID": "wTEST:p1",
                                       "HERDR_SOCKET_PATH": "fixture", "HERDR_BIN_PATH": str(self.herdr)}), \
                patch.object(companion.subprocess, "run") as reports, \
                patch.object(companion.subprocess, "Popen") as launching:
            reports.return_value.returncode = 0
            launching.return_value.wait.return_value = 0
            launching.return_value.poll.return_value = 0
            self.assertEqual(companion.worker_view(args), 0)
        calls = [call.args[0] for call in reports.call_args_list]
        self.assertEqual([call[2] for call in calls],
                         ["report-agent", "report-agent-session", "release-agent"])
        self.assertIn("unknown", calls[0])
        self.assertNotIn("working", calls[0])
        self.assertTrue(all("Cosmon" in call for call in calls))
        sequences = [int(call[call.index("--seq") + 1]) for call in calls]
        self.assertEqual(sequences, sorted(set(sequences)))
        resume = calls[1][calls[1].index("--") + 1:]
        self.assertEqual(resume[0], "python3")
        self.assertIn(args.expect, resume)
        self.assertNotIn("--interact", resume)
        self.assertEqual(launching.call_args.args[0][-2:], ["--mode", "interactive"])

    def test_stale_view_never_registers_or_launches(self):
        args = SimpleNamespace(id=self.detail["id"], interact=True, expect="sha256:" + "a" * 64)
        with patch.object(companion, "observe"), patch.object(companion, "terminal", return_value={
                "availability": "available", "expected": "sha256:" + "b" * 64}), \
                patch.object(companion.subprocess, "Popen") as opening:
            with self.assertRaises(companion.Unavailable):
                companion.worker_view(args)
            opening.assert_not_called()

    def test_terminal_version_capability_identity_project_and_token_negatives(self):
        for key, value in [("schema_version", 2), ("schema_version", True), ("kind", "other"), ("capability", "write"),
                           ("id", "other-id"), ("project_root", str(self.checkout)),
                           ("state_dir", str(self.root)), ("expected", "$(touch SENTINEL)")]:
            with self.subTest(key=key):
                terminal = self.public_terminal()
                terminal[key] = value
                self.detail["_terminal"] = terminal
                self.invoke("open", self.detail["id"], "--terminal", "--workspace", "wTEST", expected=1)
        self.assertTrue(all(call[0] == "cs" for call in self.calls()))

    def test_unknown_and_unavailable_never_open_even_with_bait_address(self):
        for availability in ("unknown", "unavailable"):
            terminal = self.public_terminal()
            terminal.update(availability=availability, reason="identity_unknown", identity={"session": "bait"})
            self.detail["_terminal"] = terminal
            result = self.invoke("open", self.detail["id"], "--terminal", "--workspace", "wTEST", expected=1)
            self.assertIn(availability, result["reason"])
        self.assertTrue(all(call[0] == "cs" for call in self.calls()))

    def test_plugin_context_never_uses_plugin_cwd_and_rejects_ambiguity(self):
        base = dict(repo=None, cs=str(self.cs), workspace=None)
        with patch.dict(os.environ, {"HERDR_PLUGIN_ID": "cosmon.client"}, clear=True):
            with self.assertRaises(companion.Unavailable):
                companion.configure(SimpleNamespace(**base))
        context = {"workspace_id": "wTEST", "workspace_cwd": str(self.checkout)}
        with patch.dict(os.environ, {"HERDR_PLUGIN_CONTEXT_JSON": json.dumps(context)}, clear=True):
            args = SimpleNamespace(**base)
            companion.configure(args)
            self.assertEqual(args.repo, self.repo)
            self.assertEqual(args.workspace, "wTEST")
        other = self.root / "other-project"
        other.mkdir()
        subprocess.run(["git", "init", "-q", str(other)], check=True)
        (other / ".cosmon").mkdir()
        (other / ".cosmon" / "config.toml").write_text("[project]\nproject_id='other'\n")
        context["focused_pane_cwd"] = str(other)
        with patch.dict(os.environ, {"HERDR_PLUGIN_CONTEXT_JSON": json.dumps(context)}, clear=True):
            with self.assertRaises(companion.Unavailable):
                companion.configure(SimpleNamespace(**base))

    def test_herdr_new_tab_uses_only_workspace_and_literal_argv(self):
        args = SimpleNamespace(repo=self.repo, workspace="wTEST")
        argv = ["cs binary;literal", "--config", str(self.repo), "terminal", self.detail["id"],
                "--attach", "--expect", "sha256:" + "a" * 64]
        with patch.object(companion, "api", return_value={}) as api:
            companion.open_argv(args, argv, "Cosmon worker (read-only)")
        method, payload = api.call_args.args[1:]
        self.assertEqual(method, "layout.apply")
        self.assertEqual(payload["workspace_id"], "wTEST")
        self.assertNotIn("tab_id", payload)  # Never replace a pre-existing pane.
        self.assertEqual(payload["root"]["command"], argv)
        self.assertEqual(payload["root"]["cwd"], str(self.repo))


if __name__ == "__main__":
    unittest.main()
