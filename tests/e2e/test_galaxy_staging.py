# SPDX-License-Identifier: AGPL-3.0-only
"""Disposable repositories must stay still while staging changes permissions."""
from __future__ import annotations

import json
import stat
from dataclasses import replace

import pytest

from harness.compose import ComposeStack
from harness.record import Recorder


def test_staging_disables_background_git_maintenance(cfg, tmp_path, monkeypatch):
    """Observe real Git's child processes, including on a second staging.

    The nightly enumerated maintenance.lock and then chmod found it gone.
    No timing-dependent race is needed here: automatic maintenance must never
    be launched in this disposable repository in the first place.
    """
    global_config = tmp_path / "gitconfig"
    global_config.write_text("[maintenance]\n\tauto = true\n[gc]\n\tauto = 1\n")
    trace = tmp_path / "git-trace.jsonl"
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", str(global_config))
    monkeypatch.setenv("GIT_CONFIG_NOSYSTEM", "1")
    monkeypatch.setenv("GIT_TRACE2_EVENT", str(trace))
    config = replace(cfg, run_dir=tmp_path / "run")
    stack = ComposeStack(config, Recorder.open(config.artifacts))

    for _ in range(2):
        stack.stage_galaxy()
        paths = [config.galaxies_root, *config.galaxies_root.rglob("*")]
        assert paths
        assert all(stat.S_IMODE(path.stat().st_mode) == 0o777 for path in paths)
        assert (config.galaxy / ".git" / "HEAD").is_file()
        assert (config.galaxy / "README.md").is_file()

    children = [json.loads(line) for line in trace.read_text().splitlines()]
    automatic = [
        event["argv"] for event in children
        if event.get("event") == "child_start"
        and "--auto" in event.get("argv", [])
        and any(arg in ("maintenance", "gc") for arg in event["argv"])
    ]
    assert automatic == [], f"staging launched background Git writers: {automatic}"


def test_staging_keeps_permission_errors_visible(cfg, tmp_path, monkeypatch):
    """A fix must not turn arbitrary chmod failures into a successful stage."""
    import os

    config = replace(cfg, run_dir=tmp_path / "run")
    stack = ComposeStack(config, Recorder.open(config.artifacts))
    chmod = os.chmod

    def refuse_readme(path, mode, **kwargs):
        if path == config.galaxy / "README.md":
            raise PermissionError("injected permission refusal")
        return chmod(path, mode, **kwargs)

    monkeypatch.setattr(os, "chmod", refuse_readme)
    with pytest.raises(PermissionError, match="injected permission refusal"):
        stack.stage_galaxy()
