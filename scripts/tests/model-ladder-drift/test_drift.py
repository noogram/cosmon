# SPDX-License-Identifier: AGPL-3.0-only
"""Hermetic CLI tests: synthetic catalog evidence, no provider or dispatch calls."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

FIXTURES = Path(__file__).parent
SCRIPT = Path(os.environ.get("MODEL_DRIFT_SCRIPT", FIXTURES.parents[1] / "model-ladder-drift.py"))


class DriftTests(unittest.TestCase):
    """Exercise status, evidence and input immutability through the public CLI."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.cache = self.root / "codex.json"
        shutil.copyfile(FIXTURES / "codex.json", self.cache)
        self.config = self.root / "account"
        self.catalog = self.config / "cache/model-catalog/new-cc.json"
        self.catalog.parent.mkdir(parents=True)
        data = json.loads((FIXTURES / "claude-cc.json").read_text())
        # A clean account fixture; tests explicitly introduce plan gating.
        data["catalog"]["config"]["models"] = data["catalog"]["config"]["models"][:1]
        self.catalog.write_text(json.dumps(data))
        self.consumer = self.root / "consumer.txt"
        shutil.copyfile(FIXTURES / "current.txt", self.consumer)
        self.proposal = self.root / "ladder.proposed.json"

    def run_audit(self, expected, **options):
        command = [sys.executable, str(SCRIPT), "--codex-cache", str(self.cache),
                   "--claude-config-dir", str(self.config), "--consumer", str(self.consumer),
                   "--proposal", str(options.get("proposal", self.proposal)),
                   "--max-age-hours", "24", "--now", options.get("now", "2026-01-02T12:00:00Z")]
        before = self.consumer.read_bytes()
        result = subprocess.run(command, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        self.assertEqual(self.consumer.read_bytes(), before, "consumer changed")
        return json.loads(result.stdout) if result.stdout else None

    def test_current_pin_passes(self):
        report = self.run_audit(0)
        self.assertEqual(report["measurement"], "clean")
        self.assertEqual(report["consumers"][0]["classification"], "current")
        self.assertEqual(report["providers"]["codex"][0]["priority"], 0)

    def test_stale_consumer_pin_detected(self):
        shutil.copyfile(FIXTURES / "previous.txt", self.consumer)
        report = self.run_audit(1)
        self.assertEqual(report["consumers"][0]["classification"], "older")

    def test_missing_cache_is_unmeasured(self):
        self.cache.unlink()
        report = self.run_audit(2)
        self.assertEqual(report["measurement"], "unmeasured")
        self.assertTrue(report["unmeasured"])

    def test_stale_fetch_time_is_unmeasured(self):
        self.assertEqual(self.run_audit(2, now="2026-01-03T12:00:00Z")["measurement"], "unmeasured")

    def test_overflow_and_disabled_have_fetch_evidence(self):
        shutil.copyfile(FIXTURES / "claude-cc.json", self.catalog)
        report = self.run_audit(1)
        finding = report["findings"][0]
        self.assertEqual(finding["finding"], "plan-gated per catalog")
        self.assertEqual(finding["fetched_at"], "2026-01-02T10:00:00Z")
        self.assertEqual(finding["source"], str(self.catalog))
        self.assertEqual(finding["section"], "overflow")
        self.assertTrue(finding["disabled"])
        data = json.loads(self.catalog.read_text())
        data["catalog"]["config"]["models"][1]["section"] = "main"
        self.catalog.write_text(json.dumps(data))
        self.assertTrue(self.run_audit(1)["findings"][0]["plan_gated"])

    def test_frontier_default_requires_exact_strong_coverage(self):
        self.consumer.write_text('[adapters.claude]\ndefault_model = "claude-opus-0"\n')
        self.assertIn("frontier-looking", self.run_audit(1)["findings"][0]["finding"])
        self.consumer.write_text(self.consumer.read_text() + 'strong = [\n "claude-opus-0"\n]\n')
        self.assertFalse(self.run_audit(1)["findings"])  # unknown id still drifts

    def test_commented_strong_does_not_cover_default(self):
        self.consumer.write_text('default_model = "claude-opus-0"\nstrong = [\n # "claude-opus-0"\n]\n')
        self.assertTrue(self.run_audit(1)["findings"])

    def test_consumer_punctuation_is_not_part_of_id(self):
        self.consumer.write_text('Use gpt-0-current.\n')
        self.assertEqual(self.run_audit(0)["consumers"][0]["id"], "gpt-0-current")

    def test_separate_accounts_preserve_plan_state(self):
        other = self.root / "other"
        target = other / "cache/model-catalog/new-cc.json"
        target.parent.mkdir(parents=True)
        shutil.copyfile(FIXTURES / "claude-cc.json", target)
        result = subprocess.run([
            sys.executable, str(SCRIPT), "--codex-cache", str(self.cache),
            "--claude-config-dir", str(self.config), "--claude-config-dir", str(other),
            "--consumer", str(self.consumer), "--proposal", str(self.proposal),
            "--max-age-hours", "24", "--now", "2026-01-02T12:00:00Z"],
            capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 1, result.stderr)
        sources = {m["source"] for m in json.loads(result.stdout)["providers"]["claude"]}
        self.assertEqual(sources, {str(self.catalog), str(target)})

    def test_unknown_is_drift(self):
        self.consumer.write_text('model = "gpt-0-unknown"\n')
        self.assertEqual(self.run_audit(1)["consumers"][0]["classification"], "unknown")

    def test_newest_per_config_uses_fetch_time(self):
        old = json.loads(self.catalog.read_text())
        old["fetchedAt"] = "2026-01-01T10:00:00Z"
        (self.catalog.parent / "zzz-cc.json").write_text(json.dumps(old))
        self.run_audit(0)

    def test_missing_claude_catalog_and_malformed_cache(self):
        self.catalog.unlink()
        self.run_audit(2)
        self.cache.write_text('{}')
        self.run_audit(2)

    def test_no_input_can_be_proposal(self):
        self.run_audit(2, proposal=self.consumer)
        self.run_audit(2, proposal=self.cache)
        alias = self.root / "alias"
        alias.hardlink_to(self.consumer)
        self.run_audit(2, proposal=alias)

    def test_stale_at_and_future_fetch_are_unmeasured(self):
        data = json.loads(self.catalog.read_text())
        data["staleAt"] = "2026-01-02T11:00:00Z"
        self.catalog.write_text(json.dumps(data))
        self.run_audit(2)
        data["staleAt"] = "2026-01-04T10:00:00Z"
        data["fetchedAt"] = "2026-01-02T13:00:00Z"
        self.catalog.write_text(json.dumps(data))
        self.run_audit(2)


if __name__ == "__main__":
    unittest.main()
