# SPDX-License-Identifier: AGPL-3.0-only
"""Issue #128 increment B: legacy upgrade and crash recovery in containers.

Both cases drive the real compose service with the real ``cosmon-remote``
client and, where a grant is minted, the real external ``minisign``. Verdicts
come from independent observations: tenant Git, and the exact bytes of the
service's ``operations.jsonl`` and ``consumed.jsonl``. The HTTP replies are
only the thing being judged against them.

Crash seams use documented configuration only. ``[gates] integrity_command``
runs after the Git merge and before the journal is advanced to ``integrated``;
``[hooks] post_merge`` runs after ``integrated`` is durable. Each script blocks
on the first call, the harness SIGKILLs the service container inside that
window, restarts it, and retries the documented ``done``. No journal file is
edited to simulate a crash.
"""
from __future__ import annotations

import hashlib
import json
import subprocess
import time
from pathlib import Path

import pytest

from test_harvest_profiles import (
    _git, _merged_branch_witness, _refusal, external_signer,
    harvest_scope_issued, admin_token_file,
)
from test_harvest_matrix import _ok, _host, _snapshot

#: Frozen W1 identities (crates/cosmon-harvest/tests/grant_identity.rs): the
#: molecule-scope permit, and the v1 unsigned-variant alias derived from it for
#: molecule ``task-20260901-6da6``.
FROZEN_PERMIT = "e72edb4db6186d869a6842f62ddf86dd0b69ab0a0bbf43a5f5305332a67030e8"
FROZEN_ALIAS = "a5a87c19d0af1823ac8d0386541033fdab8095025af46871827bb874007ffbcd"
FROZEN_MOLECULE = "task-20260901-6da6"


def legacy_alias(fingerprint: str, molecule: str) -> str:
    """The v1 receipt key of a molecule-scoped grant."""
    return hashlib.sha256(f"{fingerprint}\n{molecule}\n".encode()).hexdigest()


def _rows(cfg, name):
    path = cfg.galaxy / ".cosmon/state/harvest" / name
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def _journal(cfg, molecule):
    return [row for row in _rows(cfg, "operations.jsonl")
            if row["receipt"]["effect"]["molecule"] == molecule]


def _consumed(cfg, molecule):
    return [row for row in _rows(cfg, "consumed.jsonl")
            if row["effect"]["molecule"] == molecule]


def _merges(cfg, molecule):
    out = _git(cfg.galaxy, "log", cfg.base_branch, "--merges", "--format=%H",
               "-F", f"--grep=Mol-Id: {molecule}")
    return [line for line in out.splitlines() if line]


def _cause(payload):
    return _refusal(payload).get("reason") or (payload or {}).get("error")


def _write_note(cfg, name, value):
    (cfg.artifacts / name).write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def _hold_script(cfg, name):
    """A documented hook that blocks on its first call only."""
    script = cfg.galaxy / f"hold-{name}.sh"
    script.write_text(
        "#!/bin/sh\n"
        f'n=$(cat hold-{name}.count 2>/dev/null || echo 0); n=$((n+1)); echo $n > hold-{name}.count\n'
        f'echo $$ > hold-{name}.entered\n'
        'if [ "$n" -eq 1 ]; then i=0\n'
        f'  while [ ! -f hold-{name}.release ] && [ $i -lt 900 ]; do sleep 1; i=$((i+1)); done\n'
        'fi\nexit 0\n'
    )
    script.chmod(0o755)
    return script


def _hook_calls(cfg, name):
    path = cfg.galaxy / f"hold-{name}.count"
    return int(path.read_text()) if path.exists() else 0


def _start_done(cli, molecule, reason):
    cmd = [str(cli.binary), "--profile", "e2e", "--json", "molecule", "done", molecule, "--reason", reason]
    return subprocess.Popen(cmd, env=cli._env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def _wait_for(path: Path, seconds=300):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if path.exists():
            return
        time.sleep(0.5)
    raise AssertionError(f"{path.name} never appeared: the documented hook seam was not reached")


def _crash_at(stack, cfg, cli, molecule, config_toml, name):
    """Run ``done`` into a blocking hook, SIGKILL the service there, restart it."""
    config = cfg.galaxy / ".cosmon/config.toml"
    original = config.read_bytes()
    _hold_script(cfg, name)
    config.write_bytes(original + config_toml.encode())
    _host(stack, "trust")
    before = _snapshot(cfg)
    base_before = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
    proc = _start_done(cli, molecule, "crash window")
    _wait_for(cfg.galaxy / f"hold-{name}.entered")
    at_crash = {
        "base": _git(cfg.galaxy, "rev-parse", cfg.base_branch),
        "journal": [row["stage"] for row in _journal(cfg, molecule)],
        "consumed": len(_consumed(cfg, molecule)),
        "merges": _merges(cfg, molecule),
    }
    stack.compose("kill", "rpp-adapter")
    try:
        out, err = proc.communicate(timeout=120)
    except subprocess.TimeoutExpired:
        proc.kill()
        out, err = proc.communicate()
    stack.compose("up", "-d", "--wait")
    return {"original_config": original, "before": before, "base_before": base_before, "at_crash": at_crash,
            "client_rc": proc.returncode, "client_out": out, "client_err": err[-500:]}


@pytest.mark.stack
@pytest.mark.requires_dispatch
class TestCrashRecovery:
    """SIGKILL the service inside each documented window, then retry ``done``."""

    @pytest.fixture(scope="class")
    def authority(self, harvest_scope_issued, logged_in, admin_token_file, external_signer):
        _ok(logged_in.json("harvest", "init", "--admin-token-file", str(admin_token_file)))
        return logged_in

    def _retry(self, cfg, cli, molecule, crash, label, journal_stages, *, admitted):
        """Retry the documented ``done`` and judge Git plus journal bytes.

        ``admitted`` selects the observed contract for the window: a named
        ``harvest_recovery_required`` refusal, or an idempotent 200
        ``already_landed``. Either way nothing may change and exactly one
        merge may exist.
        """
        status = cli.json("harvest", "status", "--molecule", molecule)[1]
        base_crash = crash["at_crash"]["base"]
        before = _snapshot(cfg)
        rc, payload, stderr = cli.done(molecule, "crash window")
        after = _snapshot(cfg)
        shown = {k: v for k, v in crash.items() if k != "original_config"}
        _write_note(cfg, f"crash-{label}.json", {
            "label": label, "crash": shown, "retry_rc": rc, "retry_response": payload, "retry_stderr": stderr[-1500:],
            "status": status, "snapshot_before_retry": before, "snapshot_after_retry": after,
            "merges": _merges(cfg, molecule), "journal": _journal(cfg, molecule),
            "hook_calls": _hook_calls(cfg, label)})
        assert crash["client_rc"] != 0, "the killed service must fail the in-flight request"
        if admitted:
            assert rc == 0, (payload, stderr[-400:])
            assert payload["harvest"]["outcome"] == "already_landed", payload
            assert payload["harvest"]["merged"] is True, payload
        else:
            assert rc != 0, f"retry unexpectedly succeeded: {payload} {stderr[-400:]}"
            assert _cause(payload) == "harvest_recovery_required", (payload, stderr[-400:])
        assert before == after, "the retry changed Git refs or the harvest journal"
        assert _git(cfg.galaxy, "rev-parse", cfg.base_branch) == base_crash
        assert len(_merges(cfg, molecule)) == 1, "exactly one merge for the molecule"
        assert [row["stage"] for row in _journal(cfg, molecule)] == journal_stages
        assert len(_consumed(cfg, molecule)) == 1, "the molecule receipt is consumed exactly once"
        assert _hook_calls(cfg, label) == 1, "the interrupted hook is never replayed"

    def test_crash_after_merge_before_integrated_checkpoint(
        self, authority, worked, molecule, stack, cfg, expect,
    ):
        cli = authority
        _ok(cli.json("harvest", "grant", "--molecule", molecule))
        crash = _crash_at(
            stack, cfg, cli, molecule,
            '\n[gates]\nintegrity_command = "./hold-gate.sh"\n', "gate",
        )
        assert crash["at_crash"]["journal"] == ["prepared"], crash
        assert len(crash["at_crash"]["merges"]) == 1, "Git already holds the merge"
        assert crash["at_crash"]["base"] != crash["base_before"]
        try:
            self._retry(cfg, cli, molecule, crash, "gate", ["prepared"], admitted=False)
        finally:
            (cfg.galaxy / ".cosmon/config.toml").write_bytes(crash["original_config"])

    def test_crash_after_integrated_checkpoint_before_finalized(
        self, authority, worked, molecule, stack, cfg,
    ):
        cli = authority
        second = _ok(cli.nucleate("task-work", "second crash window"))["molecule"]["id"]
        from test_harvest_matrix import _work
        _work(cli, second, cfg)
        _ok(cli.json("harvest", "grant", "--molecule", second))
        crash = _crash_at(
            stack, cfg, cli, second,
            '\n[hooks]\npost_merge = "./hold-post.sh"\n', "post",
        )
        assert crash["at_crash"]["journal"] == ["prepared", "integrated"], crash
        assert len(crash["at_crash"]["merges"]) == 1
        try:
            self._retry(cfg, cli, second, crash, "post", ["prepared", "integrated"], admitted=True)
        finally:
            (cfg.galaxy / ".cosmon/config.toml").write_bytes(crash["original_config"])


def _binding(cfg) -> Path:
    return cfg.staged_deploy / "state" / "nucleons" / f"nuc-{cfg.noyau}" / "oidc-identity.toml"


def test_frozen_w1_alias_matches_the_v1_derivation():
    """The independent alias formula reproduces W1's recorded identities."""
    assert legacy_alias(FROZEN_PERMIT, FROZEN_MOLECULE) == FROZEN_ALIAS


@pytest.mark.stack
@pytest.mark.requires_dispatch
class TestLegacyDeploymentSurvivesRestart:
    """``required = true`` sealed by the legacy operator path, then a restart."""

    def test_binding_config_and_receipts_are_preserved(
        self, logged_in, sealer_binary, stack, cfg, expect,
    ):
        from test_harvest_matrix import _nucleate, _work
        cli = logged_in
        config = cfg.galaxy / ".cosmon/config.toml"
        config0, binding0 = config.read_bytes(), _binding(cfg).read_bytes()
        assert b"required = true" in config0 and b"remote" not in config0
        first = _nucleate(cli, "legacy sealed before upgrade")
        _work(cli, first, cfg)
        stack.seal_harvest_grant(sealer_binary, first)
        status_before = cli.json("harvest", "status")
        before = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
        branch = _git(cfg.galaxy, "rev-parse", f"feat/{first}")
        payload = _ok(cli.done(first, "legacy sealed harvest"))
        assert payload["harvest"]["outcome"] == "landed", payload
        _merged_branch_witness(cfg, first, before, branch, expect)
        receipts0 = _snapshot(cfg)["journal"]
        # The "upgrade" available here is a service restart on the same image;
        # no older release image is retained, so the pre-upgrade bytes are the
        # ones the same code wrote through the legacy operator path.
        stack.compose("restart", "rpp-adapter")
        stack.compose("up", "-d", "--wait")
        status_after = cli.json("harvest", "status")
        refs = _snapshot(cfg)["refs"]
        again = cli.done(first, "retry after upgrade")
        _write_note(cfg, "legacy-restart.json", {
            "status_before": status_before, "status_after": status_after, "retry": again,
            "receipts_before_restart": receipts0, "receipts_after_retry": _snapshot(cfg)["journal"]})
        assert config.read_bytes() == config0, "local seal policy is byte-for-byte unchanged"
        assert _binding(cfg).read_bytes() == binding0, "the binding is byte-for-byte unchanged"
        assert (status_after[1] or {}).get("provenance") == "legacy", status_after
        assert (status_after[1] or {}).get("policy") == (status_before[1] or {}).get("policy")
        assert again[0] == 0 and again[1]["harvest"]["outcome"] == "already_landed", again
        assert _snapshot(cfg)["refs"] == refs
        assert _snapshot(cfg)["journal"] == receipts0, "receipts survive restart and retry byte-for-byte"
        assert len(_merges(cfg, first)) == 1
        # The legacy path keeps admitting work after the restart.
        second = _nucleate(cli, "legacy sealed after upgrade")
        _work(cli, second, cfg)
        stack.seal_harvest_grant(sealer_binary, second)
        base, head = _git(cfg.galaxy, "rev-parse", cfg.base_branch), _git(cfg.galaxy, "rev-parse", f"feat/{second}")
        landed = _ok(cli.done(second, "legacy sealed harvest after restart"))
        assert landed["harvest"]["outcome"] == "landed"
        _merged_branch_witness(cfg, second, base, head, expect)


@pytest.mark.stack
@pytest.mark.requires_dispatch
class TestLegacyUpgradeExternalSigner:
    """``required = true`` plus the production client and the external signer."""

    def test_external_signer_on_a_required_true_galaxy(
        self, harvest_scope_issued, logged_in, admin_token_file, external_signer,
        stack, cfg, expect,
    ):
        from test_harvest_matrix import _nucleate, _work
        cli = logged_in
        config = cfg.galaxy / ".cosmon/config.toml"
        config0 = config.read_bytes()
        binding_scoped = _binding(cfg).read_bytes()
        status0 = cli.json("harvest", "status")
        assert (status0[1] or {}).get("provenance") == "legacy", status0
        _ok(cli.json("harvest", "init", "--admin-token-file", str(admin_token_file)))
        config1 = config.read_bytes()
        status1 = _ok(cli.json("harvest", "status"))
        first = _nucleate(cli, "required true external signer")
        _work(cli, first, cfg)
        grant = _ok(cli.json("harvest", "grant", "--molecule", first))
        base, head = _git(cfg.galaxy, "rev-parse", cfg.base_branch), _git(cfg.galaxy, "rev-parse", f"feat/{first}")
        landed = _ok(cli.done(first, "required=true under the external signer"))
        assert landed["harvest"]["outcome"] == "landed", landed
        _merged_branch_witness(cfg, first, base, head, expect)
        _write_note(cfg, "legacy-external-signer.json", {
            "status_before_init": status0, "status_after_init": status1, "grant": grant,
            "config_before": config0.decode(), "config_after_init": config1.decode(),
            "config_after_done": config.read_text()})
        assert b"required = true" in config.read_bytes(), "the local seal requirement is never erased"
        assert _binding(cfg).read_bytes() == binding_scoped, "init/grant/done never rewrite the binding"
        assert status1.get("policy") == "sealed" and status1.get("provenance") == "explicit", status1
        assert (status0[1] or {}).get("policy") == "sealed", "legacy required=true already reported sealed"
        assert config1 == config0 + b'remote = "sealed"\n', (
            "init adds exactly the explicit remote selection and keeps `required = true`")

    def test_unspent_v1_receipt_refuses_instead_of_spending_again(
        self, harvest_scope_issued, logged_in, admin_token_file, external_signer,
        stack, cfg, expect,
    ):
        """A v1 receipt, a grant, no journal, no landing: never guess, never merge."""
        from test_harvest_matrix import _nucleate, _work
        cli = logged_in
        _ok(cli.json("harvest", "init", "--admin-token-file", str(admin_token_file)))
        control = _nucleate(cli, "control without a legacy receipt")
        legacy = _nucleate(cli, "molecule with a v1 receipt")
        for molecule in (control, legacy):
            _work(cli, molecule, cfg)
        # Control: the same grant shape and tenant, no historical receipt.
        _ok(cli.json("harvest", "grant", "--molecule", control))
        base, head = _git(cfg.galaxy, "rev-parse", cfg.base_branch), _git(cfg.galaxy, "rev-parse", f"feat/{control}")
        landed = _ok(cli.done(control, "control spends normally"))
        assert landed["harvest"]["outcome"] == "landed"
        _merged_branch_witness(cfg, control, base, head, expect)
        # Fixture: no release retained here can produce a v1 receipt, so the
        # historical row is appended in its recorded wire shape. The permit is
        # the frozen v1 derivation (see test_frozen_w1_alias_matches_...).
        proc = cli.run("--json", "harvest", "grant", "--molecule", legacy, step="harvest grant")
        assert proc.returncode == 0, proc.stdout[-600:]
        grant = json.loads(proc.stdout.strip().splitlines()[-1])
        assert grant["installed"] is True and grant["consumed"] is False, grant
        fingerprint = grant["fingerprint"]
        key_id = _rows(cfg, "consumed.jsonl")[0]["key_id"]
        row = {"permit": legacy_alias(fingerprint, legacy), "grant": fingerprint,
               "effect": {"galaxy": cfg.galaxy_id, "molecule": legacy, "base": cfg.base_branch},
               "key_id": key_id, "invocation_id": "historical-v1"}
        line = json.dumps(row, separators=(",", ":"))
        stack.compose(
            "exec", "-T", "-w", f"/cosmon/galaxies/{cfg.noyau}", "rpp-adapter",
            "sh", "-c", f"printf '%s\\n' '{line}' >> .cosmon/state/harvest/consumed.jsonl")
        assert row["permit"] != fingerprint
        before = _snapshot(cfg)
        rc, payload, stderr = cli.done(legacy, "v1 receipt must not be spent again")
        after = _snapshot(cfg)
        _write_note(cfg, "legacy-v1-receipt.json", {
            "row": row, "rc": rc, "response": payload, "before": before, "after": after,
            "merges": _merges(cfg, legacy)})
        assert rc != 0, (payload, stderr[-400:])
        assert _cause(payload) == "harvest_recovery_required", payload
        assert before == after, "the refusal changed Git refs, receipts or the journal"
        assert _merges(cfg, legacy) == []
