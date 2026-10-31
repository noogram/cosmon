# SPDX-License-Identifier: AGPL-3.0-only
"""Issue #128 increment A: host-created missions and remote refusal effects.

Membership is an operator action on the service host: ``cs nucleate
--blocked-by <root>`` in the tenant galaxy. No membership state is forged.
The root completes before its children are dispatched, because blocked_by
is a progression edge. All harvests and grant imports use the real remote
client. Git refs and the durable service journal are separate witnesses.
"""
from __future__ import annotations

import base64
import json
import shutil
import subprocess
import time

import pytest

from test_harvest_profiles import (
    _git, _merged_branch_witness, _refusal, external_signer,
    harvest_scope_issued, admin_token_file,
)


def _ok(result):
    rc, payload, stderr = result
    assert rc == 0, f"rc={rc}: {payload} {stderr[-1000:]}"
    return payload


def _host(stack, *args, galaxy=None):
    """Documented operator command, inside this run's service host only."""
    root = galaxy or f"/cosmon/galaxies/{stack.cfg.noyau}"
    return stack.compose(
        "exec", "-T", "-w", root, "rpp-adapter", "cs", "--json", *args,
    )


def _nucleate(cli, topic):
    return _ok(cli.nucleate("task-work", topic))["molecule"]["id"]


def _work(cli, molecule, cfg):
    _ok(cli.tackle(molecule))
    deadline = time.monotonic() + cfg.worker_timeout
    while time.monotonic() < deadline:
        if _ok(cli.observe(molecule))["molecule"]["status"] == "completed":
            assert _git(cfg.galaxy, "rev-parse", f"feat/{molecule}")
            return
        time.sleep(2)
    raise AssertionError(f"worker did not complete {molecule}")


def _snapshot(cfg, galaxy=None):
    """All refs plus exact journal/consumption bytes, independently of HTTP."""
    galaxy = galaxy or cfg.galaxy
    state = galaxy / ".cosmon" / "state"
    return {
        "refs": _git(galaxy, "for-each-ref", "--format=%(refname) %(objectname)"),
        "journal": {
            name: (state / "harvest" / name).read_text() if (state / "harvest" / name).exists() else None
            for name in ("operations.jsonl", "consumed.jsonl")
        },
    }


def _refused(cfg, case, operation, cause, *, authorization=True):
    before = _snapshot(cfg)
    rc, payload, stderr = operation()
    after = _snapshot(cfg)
    # Persist observations before judging them, including failed controls.
    with (cfg.artifacts / "refusal-witness.ndjson").open("a") as out:
        out.write(json.dumps({"case": case, "rc": rc, "response": payload,
                              "before": before, "after": after}, sort_keys=True) + "\n")
    assert rc != 0, f"{case} unexpectedly admitted: {payload} {stderr[-500:]}"
    observed = _refusal(payload).get("reason") if authorization else (payload or {}).get("error")
    assert observed == cause, (case, payload, stderr[-500:])
    assert before["refs"], "the Git witness must contain actual refs"
    assert before == after, f"{case} changed Git refs or the service journal"


def _land(cli, cfg, molecule, expect):
    before = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
    branch = _git(cfg.galaxy, "rev-parse", f"feat/{molecule}")
    payload = _ok(cli.done(molecule, "reviewed mission member"))
    assert payload["harvest"]["merged"] is True
    after = _merged_branch_witness(cfg, molecule, before, branch, expect)
    journal = cfg.galaxy / ".cosmon/state/harvest/operations.jsonl"
    raw = journal.read_text()
    (cfg.artifacts / f"journal-{molecule}.jsonl").write_text(raw)
    records = [json.loads(line) for line in raw.splitlines()]
    matching = [row for row in records if row["receipt"]["effect"]["molecule"] == molecule]
    assert [row["stage"] for row in matching] == ["prepared", "integrated", "finalized"]
    assert matching[-1]["merge_oid"] == after
    assert matching[-1]["branch_head"] == branch
    assert matching[0]["pre_merge_base"] == before
    with (cfg.artifacts / "mission-journal.ndjson").open("a") as out:
        for row in matching:
            out.write(json.dumps(row, sort_keys=True) + "\n")


@pytest.mark.stack
@pytest.mark.requires_dispatch
class TestHarvestMatrix:
    """One fresh sealed stack, production signer and completed worker branch."""

    @pytest.fixture(scope="class")
    def authority(self, harvest_scope_issued, logged_in, admin_token_file, external_signer):
        _ok(logged_in.json("harvest", "init", "--admin-token-file", str(admin_token_file)))
        return logged_in

    def test_invalid_signature_and_mismatched_import(self, authority, worked, molecule, cfg):
        challenge = authority.home / "matrix.challenge.json"
        signed = authority.home / "matrix.signed.json"
        _ok(authority.json("harvest", "grant", "--molecule", molecule, "--export", str(challenge)))
        _ok(authority.json("harvest", "grant", "--sign", str(challenge), "--output", str(signed)))
        original = json.loads(signed.read_text())
        damaged = json.loads(signed.read_text())
        # Keep a structurally valid minisign packet and its key ID; flip a
        # signature byte so only cryptographic verification can reject it.
        attestation = damaged["authorization"]["attestation"]
        packet = bytearray(base64.b64decode(attestation["signature"]))
        packet[-1] ^= 1
        attestation["signature"] = base64.b64encode(packet).decode()
        invalid = authority.home / "matrix.invalid.json"
        invalid.write_text(json.dumps(damaged))
        _refused(cfg, "invalid-signature", lambda: authority.json(
            "harvest", "grant", "--import", str(invalid)), "harvest_signature_invalid")
        other = _nucleate(authority, "mismatched grant target")
        original["molecule"] = other
        mismatch = authority.home / "matrix.mismatch.json"
        mismatch.write_text(json.dumps(original))
        _refused(cfg, "mismatched-import", lambda: authority.json(
            "harvest", "grant", "--import", str(mismatch)), "harvest_grant_mismatch")

    def test_malformed_import_stops_on_client(self, authority, worked, molecule, cfg):
        """A bounded client refusal, not a named server-refusal witness."""
        malformed = authority.home / "malformed-grant.json"
        malformed.write_text("{not json")
        before = _snapshot(cfg)
        proc = authority.run("harvest", "grant", "--import", str(malformed))
        after = _snapshot(cfg)
        (cfg.artifacts / "malformed-client-boundary.json").write_text(json.dumps({
            "rc": proc.returncode, "stdout": proc.stdout, "stderr": proc.stderr,
            "before": before, "after": after,
            "limit": "client parse refusal; no named server cause witnessed",
        }, sort_keys=True))
        assert proc.returncode != 0
        assert before == after

    def test_molecule_grant_cannot_harvest_another(self, authority, worked, molecule, cfg):
        _ok(authority.json("harvest", "grant", "--molecule", molecule))
        other = _nucleate(authority, "unrelated completed worker")
        _work(authority, other, cfg)
        _refused(cfg, "mismatched-done", lambda: authority.done(other, "wrong molecule grant"),
                 "harvest_grant_mismatch")

    def test_reserved_target(self, authority, worked, molecule, stack, cfg):
        _host(stack, "tag", molecule, "--add", "hold:human")
        try:
            _refused(cfg, "reserved-target", lambda: authority.done(molecule, "reserved target"),
                     "reservation_requires_seal", authorization=False)
        finally:
            _host(stack, "tag", molecule, "--remove", "hold:human")

    def test_stale_facts_at_effect(self, authority, worked, molecule, stack, cfg):
        _ok(authority.json("harvest", "grant", "--molecule", molecule))
        config = cfg.galaxy / ".cosmon/config.toml"
        original = config.read_bytes()
        hook = cfg.galaxy / "change-harvest-facts.sh"
        marker = cfg.galaxy / "stale-facts.marker"
        hook.write_text(
            '#!/bin/sh\nprintf "observed\\n" > stale-facts.marker\n'
            'printf "\\n# operator changed authority facts during pre_done\\n" >> .cosmon/config.toml\n'
        )
        hook.chmod(0o755)
        try:
            config.write_bytes(original + b'\n[hooks]\npre_done = "./change-harvest-facts.sh"\n')
            _host(stack, "trust")
            _refused(cfg, "stale-facts", lambda: authority.done(molecule, "facts changed during hook"),
                     "harvest_facts_changed")
            assert marker.read_text() == "observed\n", "the pre_done seam must actually run"
            assert config.read_bytes() != original
        finally:
            config.write_bytes(original)

    def test_cross_tenant_target(self, authority, stack, cfg):
        # A separate host-provisioned tenant has real state and refs. The
        # caller remains bound to the original tenant throughout the request.
        foreign = cfg.galaxies_root / "foreign"
        subprocess.run(["git", "clone", "--quiet", "--no-hardlinks", str(cfg.galaxy), str(foreign)], check=True)
        shutil.copytree(cfg.galaxy / ".cosmon/formulas", foreign / ".cosmon/formulas")
        (foreign / ".cosmon/config.toml").write_text('[project]\nproject_id = "foreign"\n')
        for path in [foreign, *foreign.rglob("*")]:
            path.chmod(0o777)
        proc = _host(stack, "nucleate", "task-work", "--var", "topic=foreign target", "--no-parent",
                     galaxy="/cosmon/galaxies/foreign")
        foreign_id = json.loads(proc.stdout)["id"]
        before = _snapshot(cfg, foreign)
        assert before["refs"]
        own_before = _snapshot(cfg)
        foreign_events = (foreign / ".cosmon/state/fleets/default/molecules" / foreign_id / "events.jsonl").read_bytes()
        rc, payload, stderr = authority.done(foreign_id, "cross tenant denied")
        after = _snapshot(cfg, foreign)
        events_after = (foreign / ".cosmon/state/fleets/default/molecules" / foreign_id / "events.jsonl").read_bytes()
        (cfg.artifacts / "cross-tenant-witness.json").write_text(json.dumps({
            "molecule": foreign_id, "rc": rc, "response": payload,
            "foreign_before": before, "foreign_after": after,
            "own_before": own_before, "own_after": _snapshot(cfg),
            "foreign_events_unchanged": foreign_events == events_after,
        }, sort_keys=True))
        assert rc != 0 and (payload or {}).get("error") == "not_found", (payload, stderr)
        assert before == after
        assert own_before == _snapshot(cfg)
        assert foreign_events == events_after



@pytest.mark.stack
@pytest.mark.requires_dispatch
class TestMissionHarvest:
    """Host-side --blocked-by membership; root first, two remote harvests."""

    def test_two_members_and_unrelated_refusal(
        self, harvest_scope_issued, logged_in, admin_token_file, external_signer,
        stack, cfg, expect,
    ):
        cli = logged_in
        _ok(cli.json("harvest", "init", "--admin-token-file", str(admin_token_file)))
        root = _nucleate(cli, "mission root")
        members = []
        for _ in range(2):
            proc = _host(stack, "nucleate", "task-work", "--blocked-by", root,
                         "--var", "topic=host-created mission member", "--no-parent")
            members.append(json.loads(proc.stdout)["id"])
        _work(cli, root, cfg)
        _ok(cli.json("harvest", "grant", "--mission", root))
        _land(cli, cfg, root, expect)
        for member in members:
            _work(cli, member, cfg)
            _land(cli, cfg, member, expect)
        unrelated = _nucleate(cli, "unrelated to the signed mission")
        _work(cli, unrelated, cfg)
        _refused(cfg, "mission-unrelated", lambda: cli.done(unrelated, "outside the mission"),
                 "harvest_grant_mismatch")


@pytest.mark.stack
@pytest.mark.requires_dispatch
class TestGrantLifetime:
    """Expiry and revocation each exercise an otherwise valid installed grant."""

    def test_expiry_then_revocation(
        self, harvest_scope_issued, logged_in, admin_token_file, external_signer,
        stack, worked, molecule, cfg,
    ):
        cli = logged_in
        _ok(cli.json("harvest", "init", "--admin-token-file", str(admin_token_file)))
        _ok(cli.json("harvest", "grant", "--molecule", molecule, "--expires-in", "1m"))
        # The shortest documented client duration is one minute. This wait
        # crosses a real signed deadline; no clock, grant or state is edited.
        time.sleep(62)
        _refused(cfg, "expired-grant", lambda: cli.done(molecule, "expired grant"),
                 "harvest_grant_expired")
        _ok(cli.json("harvest", "grant", "--molecule", molecule, "--no-expiry"))
        status = _ok(cli.json("harvest", "status"))
        _host(stack, "harvest-authority", "configure", "--policy", "sealed",
              "--epoch", str(status["epoch"] + 1))
        _refused(cfg, "revoked-grant", lambda: cli.done(molecule, "revoked grant"),
                 "harvest_grant_revoked")


@pytest.mark.stack
@pytest.mark.requires_dispatch
class TestConfiguredHarvestBinary:
    """An explicit policy refuses a configured executor before spawning it."""

    def test_sentinel_does_not_run(
        self, harvest_scope_issued, logged_in, admin_token_file, worked, molecule, stack, cfg,
    ):
        # Selecting scoped requires removing the fixture's local seal first,
        # the same documented operator config edit as TestScopedProfile.
        config = cfg.galaxy / ".cosmon/config.toml"
        config.write_text(config.read_text().replace("required = true", "required = false"))
        _ok(logged_in.json("harvest", "configure", "--policy", "scoped",
                           "--admin-token-file", str(admin_token_file)))
        sentinel = cfg.galaxy / "harvest-sentinel"
        marker = cfg.galaxy / "harvest-sentinel.marker"
        sentinel.write_text('#!/bin/sh\nprintf \'spawned\\n\' > "$0.marker"\nexit 0\n')
        sentinel.chmod(0o755)
        binary = f"/cosmon/galaxies/{cfg.noyau}/harvest-sentinel"
        # Positive control runs the exact executable in the same container.
        stack.compose("exec", "-T", "rpp-adapter", binary)
        assert marker.read_bytes() == b"spawned\n"
        marker.unlink()
        rpp = cfg.staged_deploy / "rpp.toml"
        original = rpp.read_bytes()
        try:
            rpp.write_bytes(f'harvest_cs_binary = "{binary}"\n'.encode() + original)
            stack.compose("restart", "rpp-adapter")
            stack.compose("up", "-d", "--wait")
            _refused(cfg, "configured-binary", lambda: logged_in.done(molecule, "executor must not run"),
                     "harvest_effect_unsupported")
            assert not marker.exists(), "configured sentinel was invoked"
            (cfg.artifacts / "sentinel-control.json").write_text(json.dumps({
                "positive_control": "spawned\n", "marker_after_refusal": marker.exists(),
            }))
        finally:
            rpp.write_bytes(original)
            stack.compose("restart", "rpp-adapter")
            stack.compose("up", "-d", "--wait")
