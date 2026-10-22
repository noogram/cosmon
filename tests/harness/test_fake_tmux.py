# SPDX-License-Identifier: AGPL-3.0-only
"""Replay the witnessed-launch argv that the nightly fake rejected."""
from pathlib import Path
import os
import subprocess
import tempfile
import unittest


FAKE = Path(__file__).resolve().parents[1] / "fakes" / "fake-tmux" / "tmux"
# Captured from the failed nightly's healthy/fake-tmux.log, not imported
# from the implementation. Real kernel process witnesses are absent in a fake.
FORMAT = "\t".join([
    "#{socket_path}", "#{pid}", "#{session_id}", "#{session_created}",
    "#{pane_id}", "#{pane_pid}", "#{pane_dead}", "#{session_windows}",
    "#{window_panes}", "#{version}",
])


class WitnessedLaunch(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.env = dict(os.environ, FAKE_TMUX_DIR=str(self.root))
        for key in list(self.env):
            if key.startswith("FAKE_TMUX_") and key != "FAKE_TMUX_DIR":
                del self.env[key]

    def tmux(self, *args):
        return subprocess.run([str(FAKE), "-L", "project-test", *args],
                              env=self.env, capture_output=True, text=True)

    def launch(self, *extra):
        return self.tmux("new-session", "-d", "-P", "-F", FORMAT,
                         "-s", "matrix-row-healthy", "-c", str(self.root),
                         *extra, "exec claude --permission-mode bypassPermissions")

    def test_captured_argv_creates_session_and_renders_fields(self):
        result = self.launch()
        self.assertEqual(result.returncode, 0, result.stderr)
        fields = result.stdout.rstrip("\n").split("\t")
        self.assertEqual(len(fields), 10, result.stdout)
        self.assertEqual(fields[0], str(self.root / "project-test" / "socket"))
        # Zero process IDs deliberately cannot pass the kernel identity guard.
        self.assertEqual(fields[1:3], ["0", "$0"])
        self.assertGreater(int(fields[3]), 0)
        self.assertEqual(fields[4:], ["%0", "0", "0", "1", "1", "3.4"])
        meta = (self.root / "project-test" / "matrix-row-healthy.meta").read_text()
        self.assertIn("cwd=" + str(self.root) + "\n", meta)
        self.assertIn("cmd=exec claude --permission-mode bypassPermissions\n", meta)
        self.assertEqual(self.tmux("list-sessions", "-F", "#{session_name}").stdout,
                         "matrix-row-healthy\n")
        self.assertEqual(self.tmux("list-panes", "-t", "matrix-row-healthy",
                                   "-F", "#{pane_dead}").stdout, "0\n")
        observed = self.tmux("list-panes", "-a", "-F", "#{session_name}|#{pane_id}")
        self.assertEqual(observed.stdout, "matrix-row-healthy|%0\n")
        captured = self.tmux("capture-pane", "-t", "%0", "-p", "-S", "-")
        self.assertEqual(captured.returncode, 0, captured.stderr)
        self.assertIn("❯ Type your message", captured.stdout)

    def test_pane_selector_cannot_read_another_session(self):
        self.assertEqual(self.launch().returncode, 0)
        self.env["FAKE_TMUX_INIT_OUTPUT"] = "second session"
        second = self.tmux("new-session", "-d", "-P", "-F", "#{pane_id}",
                           "-s", "other", "true")
        self.assertEqual(second.stdout, "%1\n")
        self.assertEqual(self.tmux("capture-pane", "-t", "%1", "-p").stdout,
                         "second session\n")
        self.assertIn("❯ Type your message", self.tmux("capture-pane", "-t", "%0", "-p").stdout)
        self.assertNotEqual(self.tmux("capture-pane", "-t", "%99", "-p").returncode, 0)

    def test_environment_option_is_not_the_shell_command(self):
        result = self.launch("-e", "TOKEN=one two", "-e", "OTHER=value")
        self.assertEqual(result.returncode, 0, result.stderr)
        meta = (self.root / "project-test" / "matrix-row-healthy.meta").read_text()
        self.assertIn("cmd=exec claude --permission-mode bypassPermissions\n", meta)
        self.assertIn("env=TOKEN=one two\n", meta)
        self.assertIn("env=OTHER=value\n", meta)

    def test_fault_injection_still_refuses_launch(self):
        self.env["FAKE_TMUX_NEW_SESSION_EXIT"] = "42"
        result = self.launch()
        self.assertEqual(result.returncode, 42)
        self.assertEqual(result.stdout, "")
        self.assertFalse((self.root / "project-test" / "matrix-row-healthy.meta").exists())

    def test_no_print_flag_remains_silent(self):
        result = self.tmux("new-session", "-d", "-s", "plain", "true")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
