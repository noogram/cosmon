# SPDX-License-Identifier: AGPL-3.0-only
"""Exercise tenant reset permissions without a Docker daemon.

Run with: python3 tests/harness/test_rpp_compose_cleanup.py
"""
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "e2e"))
from harness import compose
from harness.config import E2EConfig


class TenantReset(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        repo = Path(__file__).resolve().parents[2]
        self.cfg = E2EConfig.from_env(repo)
        from dataclasses import replace
        self.cfg = replace(self.cfg, run_dir=Path(self.temp.name), e2e_stage=True)
        self.stack = compose.ComposeStack(self.cfg, None)
        self.fleets = self.cfg.galaxy / ".cosmon/state/fleets"
        (self.fleets / "default").mkdir(parents=True)
        # An empty default fleet still cannot be unlinked from a parent
        # lacking write permission. This is a real OS refusal, not a mock.
        self.fleets.chmod(0o555)
        self.addCleanup(lambda: self.fleets.chmod(0o755) if self.fleets.exists() else None)

    def test_old_host_reset_is_denied(self):
        with self.assertRaises(PermissionError) as caught:
            shutil.rmtree(self.cfg.galaxies_root)
        self.assertEqual(Path(caught.exception.filename).name, "default")

    def test_reset_clears_previous_set_before_staging(self):
        real_run = compose._run
        calls = []

        def injected_clear(cmd, **kwargs):
            if cmd[:2] != ["docker", "run"]:
                return real_run(cmd, **kwargs)
            calls.append(cmd)
            self.assertEqual(kwargs, {"timeout": 60})
            self.assertIn("type=bind,src=" + str(self.cfg.galaxies_root) + ",dst=/tenant", cmd)
            self.assertIn("0:0", cmd)
            self.assertIn("cs-rpp-adapter:e2e", cmd)
            self.assertEqual(cmd[-2:], ["-c", "find /tenant -mindepth 1 -delete"])
            # Inject only the privileged boundary. The actual reset and
            # subsequent git provisioning run normally on the host.
            self.fleets.chmod(0o755)
            for child in self.cfg.galaxies_root.iterdir():
                shutil.rmtree(child)
            return subprocess.CompletedProcess(cmd, 0, "", "")

        with patch.object(compose, "_run", side_effect=injected_clear):
            self.stack.stage_galaxy()
        self.assertEqual(len(calls), 1)
        self.assertFalse(self.fleets.exists(), "the old set's state survived")
        self.assertTrue((self.cfg.galaxy / ".git").is_dir())
        self.assertTrue((self.cfg.galaxy / ".cosmon/config.toml").is_file())

    def test_privileged_failure_names_cause_and_retains_error(self):
        failure = subprocess.CompletedProcess([], 23, "", "mount denied by daemon")
        with patch.object(compose, "_run", return_value=failure):
            with self.assertRaisesRegex(AssertionError, "tenant bind-mount cleanup failed.*23.*mount denied by daemon"):
                self.stack.stage_galaxy()
        self.assertTrue((self.fleets / "default").exists())

    def test_missing_engine_names_cause(self):
        with patch.object(compose, "_run", side_effect=FileNotFoundError("docker unavailable")):
            with self.assertRaisesRegex(AssertionError, "cleanup could not run.*docker unavailable"):
                self.stack.stage_galaxy()

    def test_clean_host_tree_needs_no_container(self):
        self.fleets.chmod(0o755)
        real_run = compose._run
        with patch.object(compose, "_run", wraps=real_run) as run:
            self.stack.stage_galaxy()
        self.assertTrue(all(call.args[0][0] == "git" for call in run.call_args_list))

    def test_timeout_preserves_helper_stderr(self):
        failure = subprocess.TimeoutExpired("docker", 60, stderr="daemon stalled")
        with patch.object(compose, "_run", side_effect=failure):
            with self.assertRaisesRegex(AssertionError, "cleanup could not run.*daemon stalled"):
                self.stack.stage_galaxy()


if __name__ == "__main__":
    unittest.main()
