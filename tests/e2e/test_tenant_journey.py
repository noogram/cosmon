# SPDX-License-Identifier: AGPL-3.0-only
"""The tenant's journey: login → auth me → nucleate → observe → land.

One test set (one class): the stack is reinitialised before it and the
whole journey runs against that clean stack. Each leg is its own test
and reaches the state it needs through fixtures, not through a sibling
test having run first — so `pytest tests/e2e -k observe` is a real,
runnable command and not a broken one.

`tackle` and `done` are deliberately absent. The adapter image is
library-direct (its Dockerfile ships no `cs`) and `POST …/tackle` still
shells out; issue #54 owns making that leg library-direct and owns
adding it here. `land` shells out too, which is why this file pins its
refusal LABEL rather than asserting a harvest.
"""
from __future__ import annotations

import re

import pytest

pytestmark = pytest.mark.stack


class TestTenantJourney:
    """One test set: a clean stack, one tenant, one molecule."""

    def test_login_persists_a_credential(self, logged_in, cfg, expect):
        """The authorization-code + PKCE flow ends in a stored credential.

        The `logged_in` fixture is the flow itself: discovery against the
        issuer, an auto-approving `/authorize`, a PKCE-S256 token
        exchange, a credential written through the `file` backend. What
        is asserted here is the residue — a login that "succeeded"
        without persisting anything would let every later step fail with
        a 401 that names the wrong culprit.

        The mock IdP's deviations from a real provider are enumerated in
        the `mock_oidc` fixture's docstring; read them before promoting a
        green here into a claim about a production IdP.
        """
        creds = list((logged_in.home).rglob("*"))
        expect.truthy(
            [p for p in creds if p.is_file()],
            f"cosmon-remote --profile e2e stores its credential under $HOME ({logged_in.home}) "
            "when COSMON_REMOTE_CRED_BACKEND=file; an empty tree means the flow returned "
            "before persisting",
        )

    def test_auth_me_reports_the_bound_identity(self, logged_in, cfg, expect):
        """`GET /v1/auth/me` — the token as the SERVER sees it.

        This is the step that catches a JWKS hand-off or an audience pin
        that only looks right from the client side. Two fields are
        asserted, for two different reasons:

        * `sub` — the principal the IdP signed. It is compared against
          `RPP_E2E_EXPECT_SUB`, a value provisioned nowhere: overriding
          it turns exactly this test red, which is what makes the green
          evidence rather than a tautology.
        * `noyau` — the tenant axis the *binding* resolved. It comes from
          the nucleon binding's `(iss, sub, aud)` triple (§8j posture
          (b)), not from anything the client sent, so a wrong value here
          means the binding did not resolve and admission fell back to
          nothing.
        """
        rc, me, stderr = logged_in.auth_me()
        expect.equals(rc, 0, "an authenticated `auth me` on a live stack exits 0")
        expect.equals(
            (me or {}).get("sub"),
            cfg.expect_sub,
            "the mock IdP signs in as its --subject when the request carries no login_hint, "
            "and cosmon-remote sends none",
        )
        expect.equals(
            (me or {}).get("noyau"),
            cfg.noyau,
            "§8j posture (b): the noyau comes from the nucleon binding's (iss, sub, aud) "
            "triple, never from the request; a mismatch means the binding did not resolve",
        )

    def test_nucleate_materialises_the_tenant_tree(self, molecule, cfg, expect):
        """`POST /v1/molecules` writes into the bind-mounted tenant tree.

        Library-direct since T-RPP-LIB-DIRECT: no subprocess, no `cs` in
        the image. The assertion is deliberately on the host side of the
        bind-mount — the API answering 200 proves the route, but only the
        directory appearing under `<galaxies_root>/<noyau>/` proves the
        mount is writable and lands where the operator's compose file
        says it does.
        """
        expect.truthy(
            molecule.startswith("task-"),
            "cosmon_core mints molecule ids as `<kind>-<date>-<nonce>`; the API returns the "
            "id it minted, unrewritten",
        )
        path = cfg.galaxy / ".cosmon" / "state" / "fleets" / "default" / "molecules" / molecule
        expect.truthy(
            path.is_dir(),
            f"rpp.toml pins galaxies_root=/cosmon/galaxies and docker-compose.yml bind-mounts "
            f"the host tree there, so a molecule nucleated over the API must materialise at "
            f"{path} on the host",
        )

    def test_observe_reads_the_molecule_back(self, logged_in, molecule, cfg, expect):
        """`GET /v1/molecules/:id` — the molecule reads back over the wire.

        The status is asserted, not merely echoed. A molecule nucleated
        over the API is assigned to nobody, so `cosmon_core::nucleate`
        leaves it `Pending` (it would be `Queued` if assigned) and the
        envelope renders that as the snake_case label. A molecule that
        reads back by id while reporting a status the nucleate route
        never produces is exactly the envelope drift this suite exists to
        notice — which is why there is no fallback chain on the field.
        """
        rc, payload, stderr = logged_in.observe(molecule)
        expect.equals(rc, 0, "observing a molecule the caller's noyau owns exits 0")
        expect.equals(
            (payload or {}).get("molecule", {}).get("id"),
            molecule,
            "the observe envelope carries the molecule under `.molecule`, spelled once and "
            "exactly: a `//` fallback would accept a drifted shape as if nothing had changed",
        )
        expect.equals(
            (payload or {}).get("molecule", {}).get("status"),
            cfg.expect_observe_status,
            "cosmon_core::nucleate: Pending if unassigned, Queued if assigned — an API "
            "nucleation assigns nobody (override with RPP_E2E_EXPECT_STATUS to falsify)",
        )

    def test_land_returns_its_named_refusal(self, logged_in, molecule, cfg, expect):
        """The harvest door refuses, and refuses *by name*.

        The assertion is the NAME of the refusal and the CLI's exit code,
        not merely "it failed": a door that refuses for an unnamed reason
        is the defect ADR-176 exists to prevent, and a 500 would satisfy
        a "non-zero exit" assertion just as well as the right refusal
        does.

        Today the door refuses `subprocess_spawn_failed` in-container
        because `land` still shells out and the library-direct image
        ships no `cs`. When #54 makes the door library-direct the refusal
        becomes a harvest-door one, this test goes red, and that is the
        point: pinning the label is what makes the change announce
        itself here instead of passing silently.
        """
        rc, payload, stderr = logged_in.land(molecule)
        expect.truthy(
            rc != 0,
            "land on a molecule with no operator grant must not succeed; a zero exit here "
            "would mean the harvest door opened without one",
        )
        # The label may arrive on stdout (a JSON error envelope) or, when
        # the CLI reports the refusal on its error stream, in stderr. Both
        # are read; neither is invented — an unnamed refusal must fail.
        label = None
        if isinstance(payload, dict):
            label = payload.get("error") or payload.get("label")
        if not label:
            match = re.search(r'"(?:error|label)"\s*:\s*"([a-z_]+)"', stderr)
            label = match.group(1) if match else None
        expect.equals(
            label,
            cfg.expect_land_label,
            "ADR-176: every refusal carries a label a caller can branch on. If #54 made the "
            "door library-direct, the expected label changes — set RPP_E2E_EXPECT_LAND_LABEL "
            "and update this expectation in the same commit",
        )
