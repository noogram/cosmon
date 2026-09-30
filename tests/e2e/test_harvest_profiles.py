# SPDX-License-Identifier: AGPL-3.0-only
"""Acceptance cases for the remote harvest contract (issue #120).

The contract is ``docs/specs/remote-harvest-contract.md``; each case below
names the clause it asserts. The cases are written against commands and
fields that the contract specifies
(``cosmon-remote harvest ...``, the ``harvest_authorization`` refusal
object, ``[harvest_authority] remote``). The container cases exercise the
shipped operator commands against a disposable service and tenant galaxy.

The client-only operator surface needs no stack. The remaining cases run in
the container suite, so the normal runner exercises both policy profiles.

Prerequisites are not verdicts
------------------------------

The harness provisions a disposable admin credential and materialises the
harvest scope in the throwaway binding. Sealed cases also need the external
operator-side ``minisign`` binary. Their fixtures fail with a message that
says *prerequisite missing*, never with an assertion about the product. A red
caused by a missing prerequisite is not the intended RED of a case.

What the fixtures here do on the host
-------------------------------------

The throwaway galaxy is staged with ``[harvest_authority] required =
true``. The scoped and legacy classes rewrite that file on the host
before any request, because ``required`` is the galaxy's own tracked
local seal policy, which the local operator edits; it is not remote
provisioning. The remote policy itself is only ever selected through the
shipped ``harvest configure`` command, and the tests assert that no
request writes ``remote`` behind the operator's back.
"""
from __future__ import annotations

import json
import hashlib
import shutil
import subprocess
import urllib.parse
import urllib.request
from pathlib import Path

import pytest

HARVEST_SCOPE = "cosmon:molecule:harvest"

#: The galaxy config with no local seal: rows R1 and R4 of the contract.
_UNSEALED_CONFIG = (
    "# Throwaway e2e galaxy, local seal policy off.\n"
    "[project]\n"
    'project_id = "{galaxy_id}"\n'
)


# -- helpers --------------------------------------------------------------


def _refusal(payload):
    """The contract's ``harvest_authorization`` object, or ``{}``."""
    if isinstance(payload, dict):
        return payload.get("harvest_authorization") or {}
    return {}


def _git(galaxy: Path, *args: str) -> str:
    proc = subprocess.run(["git", "-C", str(galaxy), *args], capture_output=True, text=True)
    return proc.stdout.strip() if proc.returncode == 0 else ""


def _config(cfg) -> Path:
    return cfg.galaxy / ".cosmon" / "config.toml"


def _merged_branch_witness(cfg, molecule: str, before: str, worker_commit: str, expect) -> str:
    """Read the landed content and lineage from tenant Git, not the reply."""
    after = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
    expect.truthy(before and after and before != after, "the tenant base branch advanced")
    expect.truthy(worker_commit, "the worker branch had a commit before integration")
    ancestor = subprocess.run(
        ["git", "-C", str(cfg.galaxy), "merge-base", "--is-ancestor", worker_commit, after],
        capture_output=True, text=True,
    )
    expect.equals(ancestor.returncode, 0, "the exact worker commit is in the tenant base history")
    worker_content = _git(cfg.galaxy, "show", f"{worker_commit}:worker-output-{molecule}.txt")
    expect.truthy(worker_content, "the disposable worker committed nonempty output")
    expect.truthy(
        _git(cfg.galaxy, "show", f"{cfg.base_branch}:worker-output-{molecule}.txt")
        == worker_content,
        "the disposable worker's committed content landed on the base branch",
    )
    message = _git(cfg.galaxy, "log", "-1", "--format=%B", cfg.base_branch)
    expect.truthy(f"Mol-Id: {molecule}" in message, "the merge has a molecule lineage trailer")
    witness = {
        "molecule": molecule,
        "base_before": before,
        "base_after": after,
        "worker_commit": worker_commit,
        "worker_content_sha256": hashlib.sha256(worker_content.encode()).hexdigest(),
        "merge_message": message,
    }
    with (cfg.artifacts / "git-witness.ndjson").open("a", encoding="utf-8") as out:
        out.write(json.dumps(witness, sort_keys=True) + "\n")
    return after


def _prerequisite(message: str):
    """Fail as a missing prerequisite, never as a product assertion."""
    pytest.fail(f"prerequisite missing (not a contract verdict): {message}", pytrace=False)


def _operator(binary: Path, home: Path, *args: str) -> subprocess.CompletedProcess:
    """Run ``cosmon-remote`` with no profile, no credential, no network."""
    env = {
        "HOME": str(home),
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin",
        "COSMON_REMOTE_CRED_BACKEND": "file",
    }
    return subprocess.run(
        [str(binary), *args], capture_output=True, text=True, env=env, timeout=60
    )


# -- prerequisite fixtures ------------------------------------------------


@pytest.fixture(scope="session")
def admin_token_file(compose_stack) -> Path:
    """The stack's admin credential, as the operator holds it (contract §8)."""
    path = compose_stack.admin_token_file
    if not path.is_file():
        _prerequisite("the staged stack has no disposable admin credential")
    return path


@pytest.fixture(scope="session")
def external_signer() -> str:
    """The operator-side signer the proposed ``harvest init`` drives."""
    path = shutil.which("minisign")
    if not path:
        _prerequisite("`minisign` must be installed on the operator side")
    return path


@pytest.fixture(scope="class")
def harvest_scope_issued(stack):
    """The administrator grants ``cosmon:molecule:harvest`` in the binding."""
    stack.grant_binding_scope(HARVEST_SCOPE)
    return HARVEST_SCOPE


@pytest.fixture(scope="class")
def unsealed_galaxy(stack, cfg) -> Path:
    """Rewrite the staged galaxy config with no ``[harvest_authority]``."""
    path = _config(cfg)
    path.write_text(_UNSEALED_CONFIG.format(galaxy_id=cfg.galaxy_id), encoding="utf-8")
    return path


def _configure(cli, policy: str, admin_token: Path):
    return cli.json(
        "harvest", "configure", "--policy", policy,
        "--admin-token-file", str(admin_token),
        step=f"harvest configure {policy}",
    )


# -- §12: the operator commands exist ------------------------------------


class TestOperatorSurface:
    """Contract §12. Client-only: no stack, no network, no credential."""

    @pytest.mark.parametrize("sub", ["configure", "init", "grant", "status"])
    def test_harvest_subcommand_exists(self, remote_binary, tmp_path, sub):
        proc = _operator(remote_binary, tmp_path, "harvest", sub, "--help")
        assert proc.returncode == 0, (
            f"`cosmon-remote harvest {sub}` is a contract §12 operator command; "
            f"rc={proc.returncode}: {proc.stderr.strip()[:400]}"
        )

    def test_configure_accepts_exactly_the_three_policies(self, remote_binary, tmp_path):
        help_proc = _operator(remote_binary, tmp_path, "harvest", "configure", "--help")
        assert help_proc.returncode == 0, "`harvest configure` must exist before its values are judged"
        proc = _operator(
            remote_binary, tmp_path,
            "harvest", "configure", "--policy", "open", "--admin-token-file", "/dev/null",
        )
        assert proc.returncode != 0, "an unknown policy value must be refused (contract R8)"
        for value in ("disabled", "scoped", "sealed"):
            assert value in proc.stderr, f"the refusal must list `{value}`: {proc.stderr[:400]}"

    def test_offline_grant_modes_are_mutually_exclusive(self, remote_binary, tmp_path):
        help_proc = _operator(remote_binary, tmp_path, "harvest", "grant", "--help")
        assert help_proc.returncode == 0, "`harvest grant` must exist before its modes are judged"
        for flag in ("--export", "--sign", "--import", "--expires-in", "--no-expiry"):
            assert flag in help_proc.stdout, f"`harvest grant` must offer {flag}"
        proc = _operator(
            remote_binary, tmp_path,
            "harvest", "grant", "--export", "a.challenge", "--import", "b.signed",
        )
        assert proc.returncode != 0 and "cannot be used with" in proc.stderr, (
            "offline issuance modes are mutually exclusive (contract §12): "
            f"rc={proc.returncode} {proc.stderr[:400]}"
        )


# -- R5: scoped cannot silently weaken a local seal -----------------------


@pytest.mark.stack
class TestScopedConflict:
    """Contract R5 and §4: ``configure scoped`` on a ``required = true`` galaxy."""

    def test_configure_scoped_refuses_and_writes_nothing(
        self, logged_in, admin_token_file, cfg, expect
    ):
        before = _config(cfg).read_text(encoding="utf-8")
        rc, payload, stderr = _configure(logged_in, "scoped", admin_token_file)
        expect.truthy(rc != 0, f"R5 must refuse, not merge the policies: {stderr[-400:]}")
        expect.equals(
            (payload or {}).get("error"), "harvest_policy_conflict",
            "the refusal names the conflict so the administrator resolves `required`",
        )
        expect.equals(
            _config(cfg).read_text(encoding="utf-8"), before,
            "a refused configure writes nothing, and never turns `required` off",
        )


# -- R4: the scoped profile ------------------------------------------------


@pytest.mark.stack
class TestScopedProfile:
    """Contract R4 and §5: harvest scope, no key, no grant."""

    @pytest.fixture(scope="class")
    def scoped_policy(self, unsealed_galaxy, harvest_scope_issued, logged_in, admin_token_file):
        rc, payload, stderr = _configure(logged_in, "scoped", admin_token_file)
        if rc != 0:
            raise AssertionError(f"harvest configure --policy scoped failed: {stderr[-800:]}")
        return payload

    @pytest.mark.requires_dispatch
    def test_sensitive_option_is_refused(self, scoped_policy, worked, logged_in, molecule, cfg, expect):
        base_before = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
        rc, payload, stderr = logged_in.json(
            "molecule", "done", molecule, "--reason", "contract §5 probe", "--force",
            step="done --force",
        )
        expect.truthy(rc != 0, f"`force` on an explicit profile must be refused: {stderr[-400:]}")
        expect.equals(
            _refusal(payload).get("reason"), "harvest_override_requires_ratification",
            "contract §5: neither the scope nor a v1 grant signs an override",
        )
        expect.equals(
            _git(cfg.galaxy, "rev-parse", cfg.base_branch), base_before,
            "a refused override leaves the base branch where it was",
        )

    @pytest.mark.requires_dispatch
    def test_harvest_scope_merges_without_key_or_grant(
        self, scoped_policy, worked, logged_in, molecule, cfg, expect
    ):
        expect.truthy(
            not (cfg.galaxy / ".cosmon" / "harvest.pub").exists(),
            "the scoped profile is exercised with no trust root installed",
        )
        before = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
        worker_commit = _git(cfg.galaxy, "rev-parse", f"feat/{molecule}")
        rc, payload, stderr = logged_in.done(molecule, "closed under the scoped profile")
        expect.equals(rc, 0, f"R4: harvest scope and explicit scoped policy merge: {stderr[-600:]}")
        harvest = (payload or {}).get("harvest", {})
        expect.equals(harvest.get("outcome"), "landed", "the scoped merge lands")
        expect.equals(harvest.get("merged"), True, "`merged` is the integration answer")
        after = _merged_branch_witness(cfg, molecule, before, worker_commit, expect)
        events = cfg.galaxy / ".cosmon" / "state" / "fleets" / "default" / "molecules" / molecule / "events.jsonl"
        event_bytes = events.read_bytes()
        retry_rc, retry, retry_stderr = logged_in.done(molecule, "retry scoped integration")
        expect.equals(retry_rc, 0, f"a recorded completion is retryable: {retry_stderr[-400:]}")
        expect.equals(_git(cfg.galaxy, "rev-parse", cfg.base_branch), after, "retry adds no merge")
        expect.equals(events.read_bytes(), event_bytes, "retry adds no lifecycle event")
        expect.equals((retry or {}).get("harvest", {}).get("merged"), True, "retry reports recorded integration")

    def test_status_reports_explicit_scoped(self, scoped_policy, logged_in, expect):
        rc, payload, stderr = logged_in.json("harvest", "status", step="harvest status")
        expect.equals(rc, 0, f"`harvest status` reads without writing: {stderr[-400:]}")
        expect.equals((payload or {}).get("policy"), "scoped", "effective policy")
        expect.equals((payload or {}).get("provenance"), "explicit", "policy provenance")


@pytest.mark.stack
class TestScopedWithoutHarvestScope:
    """Contract R4: ``cosmon:molecule:write`` is not sufficient."""

    def test_write_scope_is_not_sufficient(
        self, unsealed_galaxy, logged_in, admin_token_file, molecule, cfg, expect
    ):
        rc, _, stderr = _configure(logged_in, "scoped", admin_token_file)
        if rc != 0:
            raise AssertionError(f"harvest configure --policy scoped failed: {stderr[-800:]}")
        before = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
        rc, payload, stderr = logged_in.done(molecule, "write scope only")
        expect.truthy(rc != 0, "a write-only bearer must not harvest on an explicit profile")
        refusal = _refusal(payload)
        expect.equals(refusal.get("gate"), "scope", "the refusing gate is the scope")
        expect.equals(refusal.get("reason"), "harvest_scope_missing", "contract §11")
        expect.equals(_git(cfg.galaxy, "rev-parse", cfg.base_branch), before, "nothing merged")


@pytest.mark.stack
class TestHarvestOnlyCredential:
    """R4: an identity with only the bound harvest scope can integrate."""

    @pytest.mark.requires_dispatch
    def test_harvest_only_binding_merges(
        self, unsealed_galaxy, harvest_scope_issued, logged_in, admin_token_file,
        worked, molecule, stack, cfg, expect,
    ):
        rc, _, stderr = _configure(logged_in, "scoped", admin_token_file)
        expect.equals(rc, 0, f"operator selects scoped policy: {stderr[-400:]}")
        subject = stack.grant_harvest_only_identity()
        query = urllib.parse.urlencode({
            "sub": subject, "aud": cfg.audience, "scopes": HARVEST_SCOPE,
        })
        request = urllib.request.Request(cfg.issuer + "/issue?" + query, data=b"", method="POST")
        with urllib.request.urlopen(request, timeout=15) as response:
            token = json.load(response)["access_token"]
        rc, me, stderr = logged_in.json("auth", "me", token_override=token, step="harvest-only auth me")
        expect.equals(rc, 0, f"the second bound identity is admitted: {stderr[-400:]}")
        expect.equals((me or {}).get("sub"), subject, "the server resolved the separate identity")
        scopes = (me or {}).get("scopes", [])
        expect.truthy(HARVEST_SCOPE in scopes, "the binding grants harvest")
        expect.truthy("cosmon:molecule:write" not in scopes, "the identity has no write authority")
        before = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
        worker_commit = _git(cfg.galaxy, "rev-parse", f"feat/{molecule}")
        rc, payload, stderr = logged_in.done(
            molecule, "reviewed with harvest-only authority", token_override=token,
        )
        expect.equals(rc, 0, f"harvest alone admits done: {stderr[-500:]}")
        expect.equals((payload or {}).get("harvest", {}).get("merged"), True, "the request integrated")
        _merged_branch_witness(cfg, molecule, before, worker_commit, expect)


@pytest.mark.stack
class TestScopedNoMerge:
    """§6: explicit scoped closure preserves both Git refs."""

    @pytest.mark.requires_dispatch
    def test_no_merge_preserves_branch_and_retries_honestly(
        self, unsealed_galaxy, harvest_scope_issued, logged_in, admin_token_file,
        worked, molecule, cfg, expect,
    ):
        rc, _, stderr = _configure(logged_in, "scoped", admin_token_file)
        expect.equals(rc, 0, f"operator selects scoped policy: {stderr[-400:]}")
        base = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
        branch = _git(cfg.galaxy, "rev-parse", f"feat/{molecule}")
        expect.truthy(branch, "the disposable worker committed on its branch")
        rc, payload, stderr = logged_in.done(molecule, "close without integration", no_merge=True)
        expect.equals(rc, 0, f"scoped closure succeeds: {stderr[-500:]}")
        expect.equals((payload or {}).get("harvest", {}).get("merged"), False, "no merge is reported")
        expect.equals(_git(cfg.galaxy, "rev-parse", cfg.base_branch), base, "base is unchanged")
        expect.equals(_git(cfg.galaxy, "rev-parse", f"feat/{molecule}"), branch, "branch survives")
        retry_rc, retry, retry_stderr = logged_in.done(molecule, "retry no-merge", no_merge=True)
        expect.equals(retry_rc, 0, f"no-merge retry is admitted: {retry_stderr[-400:]}")
        expect.equals((retry or {}).get("harvest", {}).get("merged"), False, "retry reports no integration")
        expect.equals(_git(cfg.galaxy, "rev-parse", cfg.base_branch), base, "retry leaves base unchanged")


# -- R6/R7: the sealed profile, with production tooling -------------------


@pytest.mark.stack
class TestSealedProfile:
    """Contract R6/R7, §8 and §12: production key init, then a grant."""

    @pytest.fixture(scope="class")
    def sealed_policy(self, harvest_scope_issued, logged_in, admin_token_file, external_signer, cfg):
        rc, _, stderr = logged_in.json(
            "harvest", "init", "--admin-token-file", str(admin_token_file), step="harvest init"
        )
        if rc != 0:
            raise AssertionError(f"harvest init failed: {stderr[-800:]}")
        return logged_in.home / ".config" / "cosmon" / "harvest" / "keys" / "e2e"

    def test_key_stays_on_the_operator_side(self, sealed_policy, cfg, stack, expect):
        keys = list(sealed_policy.glob("*.key"))
        expect.truthy(keys, f"`harvest init` keeps its key under {sealed_policy} (contract §8)")
        leaked = [p for p in cfg.galaxies_root.rglob("*.key")]
        expect.equals(leaked, [], "no private key lands anywhere in the tenant tree")
        expect.truthy(
            (cfg.galaxy / ".cosmon" / "harvest.pub").is_file(),
            "only the public half is installed in the tenant galaxy",
        )
        container = stack.compose("ps", "-q", "rpp-adapter").stdout.strip()
        inspected = subprocess.run(
            ["docker", "inspect", "--format", "{{json .Mounts}}", container],
            capture_output=True, text=True, check=True,
        )
        mounts = json.loads(inspected.stdout)
        for key in keys:
            resolved = key.resolve()
            for mount in mounts:
                source = Path(mount["Source"]).resolve()
                expect.truthy(
                    resolved != source and source not in resolved.parents,
                    "the operator key path is absent from service and worker mounts",
                )
            secret_bytes = key.read_bytes()
            for root in (cfg.galaxies_root, cfg.staged_deploy, cfg.artifacts, stack.logs_dir):
                leaked = [p for p in root.rglob("*") if p.is_file() and secret_bytes in p.read_bytes()]
                expect.equals(leaked, [], "disposable key material never reaches tenant or run artifacts")

    @pytest.mark.requires_dispatch
    def test_missing_grant_is_typed(self, sealed_policy, worked, logged_in, molecule, cfg, expect):
        before = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
        rc, payload, stderr = logged_in.done(molecule, "no grant yet")
        expect.truthy(rc != 0, f"a sealed galaxy with no grant refuses: {stderr[-400:]}")
        refusal = _refusal(payload)
        expect.equals(refusal.get("reason"), "harvest_grant_missing", "contract §11")
        expect.equals(refusal.get("action"), "mint_grant", "one next gesture")
        expect.equals(_git(cfg.galaxy, "rev-parse", cfg.base_branch), before, "nothing merged")

    @pytest.mark.requires_dispatch
    def test_production_grant_then_done_merges(
        self, sealed_policy, worked, logged_in, molecule, cfg, expect
    ):
        rc, payload, stderr = logged_in.json(
            "harvest", "grant", "--molecule", molecule, step="harvest grant"
        )
        expect.equals(rc, 0, f"`harvest grant` installs a grant: {stderr[-600:]}")
        expect.truthy(
            "merged" not in (payload or {}),
            "installing a grant reports an installation receipt, never a merge",
        )
        before = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
        worker_commit = _git(cfg.galaxy, "rev-parse", f"feat/{molecule}")
        rc, payload, stderr = logged_in.done(molecule, "closed under a production grant")
        expect.equals(rc, 0, f"a production grant admits the merge: {stderr[-600:]}")
        expect.equals((payload or {}).get("harvest", {}).get("outcome"), "landed", "landed")
        _merged_branch_witness(cfg, molecule, before, worker_commit, expect)


@pytest.mark.stack
class TestSealedMissingRoot:
    """R6: selecting sealed without installing a root grants nothing."""

    @pytest.mark.requires_dispatch
    def test_done_refuses_missing_public_root(
        self, unsealed_galaxy, harvest_scope_issued, logged_in, admin_token_file,
        worked, molecule, cfg, expect,
    ):
        rc, _, stderr = _configure(logged_in, "sealed", admin_token_file)
        expect.equals(rc, 0, f"the operator can select sealed before key installation: {stderr[-400:]}")
        expect.truthy(
            not (cfg.galaxy / ".cosmon" / "harvest.pub").exists(),
            "no public root was installed in the disposable galaxy",
        )
        base = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
        branch = _git(cfg.galaxy, "rev-parse", f"feat/{molecule}")
        rc, payload, stderr = logged_in.done(molecule, "sealed without a public root")
        expect.truthy(rc != 0, f"an unkeyed sealed profile must refuse: {stderr[-400:]}")
        expect.equals(_refusal(payload).get("reason"), "harvest_key_missing", "the key gate refuses")
        expect.equals(_git(cfg.galaxy, "rev-parse", cfg.base_branch), base, "base is unchanged")
        expect.equals(_git(cfg.galaxy, "rev-parse", f"feat/{molecule}"), branch, "branch is unchanged")


# -- R1 and §4: upgrade preserves ------------------------------------------


@pytest.mark.stack
class TestLegacyUpgrade:
    """Contract R1 and §4. R2 (legacy sealed) is ``test_tenant_journey.py``."""

    def test_absent_policy_stays_disabled(self, unsealed_galaxy, logged_in, molecule, cfg, expect):
        before = unsealed_galaxy.read_text(encoding="utf-8")
        rc, payload, stderr = logged_in.json("harvest", "status", step="harvest status")
        expect.equals(rc, 0, f"`harvest status` answers on a legacy galaxy: {stderr[-400:]}")
        expect.equals((payload or {}).get("policy"), "disabled", "R1 stays disabled")
        expect.equals((payload or {}).get("provenance"), "legacy", "and says it is legacy")
        rc, payload, _ = logged_in.done(molecule, "legacy galaxy")
        expect.truthy(rc != 0, "R1 refuses remote done, as before the upgrade")
        expect.equals(_refusal(payload).get("reason"), "harvest_disabled", "contract §11")
        expect.equals(
            unsealed_galaxy.read_text(encoding="utf-8"), before,
            "no request writes `remote`: upgrade never selects a profile",
        )


# -- R3: disabled overrides a valid grant ----------------------------------


@pytest.mark.stack
class TestDisabledProfile:
    """Contract R3: an explicit ``disabled`` refuses even with a valid grant."""

    def test_valid_grant_does_not_enable(
        self, harvest_scope_issued, logged_in, admin_token_file, external_signer,
        worked, molecule, cfg, expect,
    ):
        rc, _, stderr = logged_in.json(
            "harvest", "init", "--admin-token-file", str(admin_token_file), step="harvest init"
        )
        expect.equals(rc, 0, f"production operator key init succeeds: {stderr[-400:]}")
        rc, _, stderr = logged_in.json(
            "harvest", "grant", "--molecule", molecule, step="harvest grant"
        )
        expect.equals(rc, 0, f"production grant is installed before disabling: {stderr[-400:]}")
        rc, _, stderr = _configure(logged_in, "disabled", admin_token_file)
        if rc != 0:
            raise AssertionError(f"harvest configure --policy disabled failed: {stderr[-800:]}")
        before = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
        rc, payload, _ = logged_in.done(molecule, "disabled despite a grant")
        expect.truthy(rc != 0, "R3 refuses")
        expect.equals(_refusal(payload).get("reason"), "harvest_disabled", "contract §11")
        expect.equals(_git(cfg.galaxy, "rev-parse", cfg.base_branch), before, "nothing merged")
