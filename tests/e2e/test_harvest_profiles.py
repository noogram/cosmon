# SPDX-License-Identifier: AGPL-3.0-only
"""Acceptance cases for the remote harvest contract (issue #120).

The contract is ``docs/specs/remote-harvest-contract.md``; each case below
names the clause it asserts. The cases are written against commands and
fields that the contract **proposes** and that do not exist yet
(``cosmon-remote harvest ...``, the ``harvest_authorization`` refusal
object, ``[harvest_authority] remote``). They are the specification the
implementation units of #120 turn green, one clause at a time.

Opt-in, by deselection
----------------------

Every case carries the ``contract_pending`` marker, and ``conftest.py``
deselects those unless ``RPP_E2E_CONTRACT_PENDING=1``. Deselection rather
than skip, for the reason the rest of this suite gives: a case that has
not run must never read as green. The nightly container job does not set
the variable, so nothing here sits red in a default gate before the code
it specifies exists. The unit that implements a clause removes the marker
from its cases in the same change::

    RPP_E2E_CONTRACT_PENDING=1 pytest tests/e2e/test_harvest_profiles.py

Prerequisites are not verdicts
------------------------------

Some cases need things the stock harness does not provision yet: the
admin credential of the stack (``RPP_E2E_ADMIN_TOKEN_FILE``), a way to
issue the harvest scope in the tenant's binding (a
``ComposeStack.grant_binding_scope`` hook), and an external signer on the
operator side (``minisign``). Their fixtures fail with a message that says
*prerequisite missing*, never with an assertion about the product. A red
caused by a missing prerequisite is not the intended RED of a case, and
must not be recorded as one.

What the fixtures here do on the host
-------------------------------------

The throwaway galaxy is staged with ``[harvest_authority] required =
true``. The scoped and legacy classes rewrite that file on the host
before any request, because ``required`` is the galaxy's own tracked
local seal policy, which the local operator edits; it is not remote
provisioning. The remote policy itself is only ever selected through the
proposed ``harvest configure`` command, and the tests assert that no
request writes ``remote`` behind the operator's back.
"""
from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

pytestmark = pytest.mark.contract_pending

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
def admin_token_file() -> Path:
    """The stack's admin credential, as the operator holds it (contract §8)."""
    value = os.environ.get("RPP_E2E_ADMIN_TOKEN_FILE", "")
    if not value or not Path(value).is_file():
        _prerequisite(
            "RPP_E2E_ADMIN_TOKEN_FILE must name the admin token the stack was booted "
            "with (COSMON_ADMIN_TOKEN_FILE); the stock e2e stack enables no admin surface"
        )
    return Path(value)


@pytest.fixture(scope="session")
def external_signer() -> str:
    """The operator-side signer the proposed ``harvest init`` drives."""
    path = shutil.which("minisign", path="/usr/bin:/bin:/usr/local/bin")
    if not path:
        _prerequisite("`minisign` must be installed on the operator side")
    return path


@pytest.fixture(scope="class")
def harvest_scope_issued(stack):
    """The administrator grants ``cosmon:molecule:harvest`` in the binding."""
    hook = getattr(stack, "grant_binding_scope", None)
    if hook is None:
        _prerequisite(
            "the harness has no ComposeStack.grant_binding_scope hook to issue "
            f"{HARVEST_SCOPE} through the documented binding path"
        )
    hook(HARVEST_SCOPE)
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
            _refusal(payload).get("reason"), "harvest_policy_conflict",
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
        rc, payload, stderr = logged_in.done(molecule, "closed under the scoped profile")
        expect.equals(rc, 0, f"R4: harvest scope and explicit scoped policy merge: {stderr[-600:]}")
        harvest = (payload or {}).get("harvest", {})
        expect.equals(harvest.get("outcome"), "landed", "the scoped merge lands")
        expect.equals(harvest.get("merged"), True, "`merged` is the integration answer")
        after = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
        expect.truthy(before and after and before != after, "the base branch advanced")

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

    def test_key_stays_on_the_operator_side(self, sealed_policy, cfg, expect):
        keys = list(sealed_policy.glob("*.key"))
        expect.truthy(keys, f"`harvest init` keeps its key under {sealed_policy} (contract §8)")
        leaked = [p for p in cfg.galaxies_root.rglob("*.key")]
        expect.equals(leaked, [], "no private key lands anywhere in the tenant tree")
        expect.truthy(
            (cfg.galaxy / ".cosmon" / "harvest.pub").is_file(),
            "only the public half is installed in the tenant galaxy",
        )

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
        rc, payload, stderr = logged_in.done(molecule, "closed under a production grant")
        expect.equals(rc, 0, f"a production grant admits the merge: {stderr[-600:]}")
        expect.equals((payload or {}).get("harvest", {}).get("outcome"), "landed", "landed")
        after = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
        expect.truthy(before and after and before != after, "the base branch advanced")


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
        self, logged_in, admin_token_file, sealed, molecule, cfg, expect
    ):
        rc, _, stderr = _configure(logged_in, "disabled", admin_token_file)
        if rc != 0:
            raise AssertionError(f"harvest configure --policy disabled failed: {stderr[-800:]}")
        before = _git(cfg.galaxy, "rev-parse", cfg.base_branch)
        rc, payload, _ = logged_in.done(molecule, "disabled despite a grant")
        expect.truthy(rc != 0, "R3 refuses")
        expect.equals(_refusal(payload).get("reason"), "harvest_disabled", "contract §11")
        expect.equals(_git(cfg.galaxy, "rev-parse", cfg.base_branch), before, "nothing merged")
