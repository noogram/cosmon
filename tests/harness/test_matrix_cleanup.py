# SPDX-License-Identifier: AGPL-3.0-only
"""A scratch observer is identified by its full dispatch, never a process name."""
import unittest
from pathlib import Path
import subprocess
import sys
import tempfile

from matrix_cleanup import cleanup, owned_observer


class OwnedObserver(unittest.TestCase):
    def test_only_the_exact_scratch_dispatch_is_owned(self):
        command = "/bin/cs realized-watch task-1 --cwd /tmp/owned/.worktrees/task-1 --config /tmp/owned/.cosmon/state"
        def owns(value):
            return owned_observer(value, "/bin/cs", "task-1", "/tmp/owned")
        self.assertTrue(owns(command))
        self.assertTrue(owns(command + " --claude-config-dir /tmp/owned/claude-config"))
        backstop = "/bin/cs briefing-backstop --state-dir /tmp/owned/.cosmon/state/fleets/default/molecules/task-1"
        self.assertTrue(owns(backstop))
        self.assertFalse(owns(backstop.replace("task-1", "task-2")))
        self.assertFalse(owns(backstop.replace("/tmp/owned", "/tmp/foreign")))
        for foreign in [command.replace("/bin/cs", "/other/cs"),
                        command.replace("task-1", "task-2"),
                        command.replace("/tmp/owned", "/tmp/foreign"),
                        command.replace("realized-watch", "tackle"),
                        command + "-foreign",
                        "sh -c " + command]:
            self.assertFalse(owns(foreign), foreign)

    def test_cleanup_stops_owned_observer_and_preserves_foreign_dispatch(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = str(Path(directory).resolve())
            Path(directory, "realized-watch").write_text("import time; time.sleep(30)\n")
            binary = str(Path(sys.executable).resolve())
            def launch(molecule):
                return subprocess.Popen([binary, "realized-watch", molecule,
                    "--cwd", f"{scratch}/.worktrees/{molecule}",
                    "--config", f"{scratch}/.cosmon/state"], cwd=scratch)
            owned, foreign = launch("task-1"), launch("task-2")
            try:
                cleanup(binary, "task-1", scratch)
                self.assertEqual(owned.wait(timeout=5), -15)
                self.assertIsNone(foreign.poll())
            finally:
                for process in (owned, foreign):
                    if process.poll() is None:
                        process.terminate()
                    process.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
