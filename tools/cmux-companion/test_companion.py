#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Independent consumer contract tests; mocked commands are not live acceptance."""

import argparse
import copy
import importlib.util
import io
import json
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("companion", Path(__file__).with_name("cosmon-cmux.py"))
client = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(client)
ID = "task-20990101-a001"


class CompanionTests(unittest.TestCase):
    """Check refusal and exact launch arguments rather than nearby UI proxies."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cmux-contract-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        subprocess.run(["git", "-C", str(self.root), "-c", "user.name=Fixture", "-c",
                        "user.email=fixture@example.invalid", "commit", "--allow-empty", "-qm", "fixture"], check=True)
        (self.root / ".cosmon").mkdir()
        (self.root / ".cosmon/config.toml").write_text('[project]\nproject_id="example-project"\n')
        self.state = self.root / ".cosmon/state"
        self.directory = self.state / "fleets/default/molecules" / ID
        self.directory.mkdir(parents=True)
        (self.directory / "synthesis.md").write_text("artifact\x1b[31m\n")
        self.args = argparse.Namespace(repo=self.root, state=self.state, project_id="example-project", fleet="default",
            cs="cs", cmux="cmux", socket=None, id=ID, command="open", terminal=True, worktree=False, artifact=None)
        branch = subprocess.check_output(["git", "-C", str(self.root), "symbolic-ref", "--short", "HEAD"]).decode().strip()
        self.detail = {"id": ID, "status": "running", "branch": branch, "molecule_dir": str(self.directory)}
        self.view = {"schema_version": 1, "kind": "cosmon.terminal", "id": ID, "project_id": "example-project",
            "fleet_id": "default", "project_root": str(self.root), "state_dir": str(self.state),
            "availability": "available", "capability": "read_only_ignore_size", "identity": {"opaque": True},
            "expected": "sha256:" + "a" * 64, "future": "ignored"}
        self.calls = []

    def provider(self, argv, cwd):
        self.calls.append((argv, cwd))
        if argv[0] == "git":
            return subprocess.check_output(argv, cwd=cwd)
        if argv[0] == "cs":
            self.assertEqual(argv[1:4], ["--config", str(self.state), "--json"])
            if "observe" in argv:
                return json.dumps(self.detail).encode()
            if "terminal" in argv:
                return json.dumps(self.view).encode()
            return json.dumps({"schema_version": 1, "molecule_states": [
                {"id": ID, "status": "running", "fleet": "default"},
                {"id": "task-20990101-a002", "status": "completed", "fleet": "default"},
                {"id": "task-20990101-a003", "status": "running", "fleet": "other"}]}).encode()
        if "system.capabilities" in argv:
            return json.dumps({"protocol": "cmux-socket", "version": 2, "methods": ["workspace.create"]}).encode()
        return json.dumps({"workspace_id": "owned-workspace", "surface_id": "owned-surface"}).encode()

    def test_fresh_public_attach_only_and_no_addresses(self):
        with patch.object(client, "run", self.provider):
            client.execute(self.args)
        self.assertEqual([c[0][4:6] for c in self.calls[:2]], [["observe", ID], ["terminal", ID]])
        params = json.loads(self.calls[-1][0][-1])
        self.assertEqual(shlex.split(params["initial_command"]), ["exec"] +
            client.self_argv(self.args, "view-terminal", ID, self.view["expected"]))
        self.assertEqual(params["cwd"], str(self.root))
        self.assertFalse(params["focus"])
        self.assertNotIn("workspace_id", params)
        self.assertEqual(self.calls[-1][0][-2], "workspace.create")

    def test_every_open_reobserves_and_passes_new_token(self):
        with patch.object(client, "run", self.provider):
            client.execute(self.args)
            self.view["expected"] = "sha256:" + "b" * 64
            client.execute(self.args)
        self.assertEqual(sum("terminal" in call[0] for call in self.calls), 2)
        self.assertIn(self.view["expected"], json.loads(self.calls[-1][0][-1])["initial_command"])

    def test_terminal_presenter_inherits_pty_and_runs_only_public_attach_argv(self):
        self.args.command, self.args.expected = "view-terminal", self.view["expected"]
        with patch.object(client.subprocess, "call", return_value=1) as call, patch("builtins.input", return_value=""), patch("sys.stdout", io.StringIO()):
            self.assertEqual(client.execute(self.args), {"attachment_exit": 1})
        call.assert_called_once_with(["cs", "--config", str(self.state), "terminal", ID,
            "--attach", "--expect", self.view["expected"]], cwd=self.root)

    def test_contract_and_identity_negatives_never_create_views(self):
        original = copy.deepcopy(self.view)
        for field, value in [("schema_version", 2), ("schema_version", True), ("schema_version", None),
            ("kind", "other"), ("id", ID + "x"), ("project_id", "other"), ("fleet_id", "other"),
            ("project_root", "/different"), ("state_dir", "/different"), ("availability", "new"),
            ("capability", "write"), ("expected", "unbound"), ("identity", None)]:
            with self.subTest(field=field, value=value):
                self.view = {**original, field: value}
                self.calls.clear()
                with patch.object(client, "run", self.provider), self.assertRaises(client.Unavailable):
                    client.execute(self.args)
                self.assertFalse(any(argv[0] == "cmux" for argv, _ in self.calls))

    def test_unknown_and_unavailable_stay_distinct_and_cannot_attach(self):
        for availability, reason in [("unknown", "identity_unknown"), ("unavailable", "identity_changed")]:
            self.view = {k: v for k, v in self.view.items() if k not in ("identity", "capability", "expected")}
            self.view.update(availability=availability, reason=reason)
            with patch.object(client, "run", self.provider):
                self.assertEqual(client.terminal(self.args, ID)["availability"], availability)
                with self.assertRaisesRegex(client.Unavailable, availability):
                    client.execute(self.args)

    def test_backend_unavailable_does_not_imply_death(self):
        with patch.object(client, "read_json", side_effect=client.Unavailable("missing")):
            self.assertEqual(client.terminal(self.args, ID)["availability"], "unknown")

    def test_prefix_resolution_refused(self):
        self.args.id = "task-2099"
        with patch.object(client, "run", self.provider), self.assertRaises(client.Unavailable):
            client.execute(self.args)
        self.assertEqual(len(self.calls), 1)

    def test_cmux_protocol_and_method_refused(self):
        for value in [{"protocol": "other", "version": 2, "methods": ["workspace.create"]},
                      {"protocol": "cmux-socket", "version": 3, "methods": ["workspace.create"]},
                      {"protocol": "cmux-socket", "version": 2, "methods": []}]:
            with patch.object(client, "cmux_rpc", return_value=value) as rpc, self.assertRaises(client.Unavailable):
                client.open_view(self.args, ["cs"], self.root, "label")
            self.assertEqual(rpc.call_count, 1)

    def test_shell_metacharacters_stay_literal_argv(self):
        argv = ["/a path/cs", "--expect", "$(touch /tmp/unwanted)`bad`'\ntext"]
        with patch.object(client, "run", self.provider):
            client.open_view(self.args, argv, self.root, "$(label)")
        params = json.loads(self.calls[-1][0][-1])
        self.assertEqual(shlex.split(params["initial_command"]), ["exec"] + argv)
        self.assertEqual(params["title"], "$(label)")

    def test_existing_worktree_and_canonical_artifacts(self):
        with patch.object(client, "run", self.provider):
            self.assertEqual(client.worktree(self.args, self.detail)["path"], str(self.root))
            self.assertEqual([f["name"] for f in client.artifacts(self.args, self.detail)["files"]], ["synthesis.md"])
            self.detail["branch"] = "missing"
            self.assertEqual(client.worktree(self.args, self.detail)["availability"], "unavailable")
        (self.directory / "report.md").symlink_to(self.directory / "synthesis.md")
        self.assertEqual(len(client.artifacts(self.args, self.detail)["files"]), 1)
        self.detail["molecule_dir"] = str(self.root)
        self.assertEqual(client.artifacts(self.args, self.detail)["availability"], "unavailable")

    def test_artifact_is_escaped_and_no_arbitrary_file_reader(self):
        self.assertEqual(client.safe_text("a\x1b[31m\x07"), "a\\x1b[31m\\x07")
        self.args.terminal, self.args.artifact = False, "../../config.toml"
        with patch.object(client, "run", self.provider), self.assertRaises(client.Unavailable):
            client.execute(self.args)

    def test_artifact_view_revalidates_and_reads_only_escaped_text(self):
        self.args.command, self.args.artifact = "read-artifact", "synthesis.md"
        self.args.terminal = False
        output = io.StringIO()
        with patch.object(client, "run", self.provider), patch("builtins.input", return_value=""), patch("sys.stdout", output):
            self.assertEqual(client.execute(self.args), {"read": "synthesis.md"})
        self.assertIn("artifact\\x1b[31m", output.getvalue())
        self.assertEqual(len(self.calls), 2)

    def test_nonavailable_cannot_smuggle_attachment_fields(self):
        self.view.update(availability="unknown", reason="identity_unknown")
        with patch.object(client, "run", self.provider), self.assertRaises(client.Unavailable):
            client.terminal(self.args, ID)

    def test_active_first_fleet_filtered_list(self):
        with patch.object(client, "run", self.provider):
            self.assertEqual(len(client.molecules(self.args)), 1)
            self.assertEqual(len(client.molecules(self.args, True)), 2)

    def test_canonical_project_even_with_inherited_overrides(self):
        with patch.dict("os.environ", {"COSMON_STATE_DIR": "/unselected/state"}):
            client.configure(self.args)
        self.assertEqual(self.args.repo, self.root)
        self.assertEqual(self.args.state, self.state)
        self.assertEqual(client.cs(self.args, "terminal", ID)[1:3], ["--config", str(self.state)])


if __name__ == "__main__":
    unittest.main()
