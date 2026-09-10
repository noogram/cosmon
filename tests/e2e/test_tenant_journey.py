# SPDX-License-Identifier: AGPL-3.0-only
"""The tenant's journey: login → auth me → nucleate → observe → tackle →
worker → land.

One test set (one class): the stack is reinitialised before it and the
whole journey runs against that clean stack. Each leg is its own test
and reaches the state it needs through fixtures, not through a sibling
test having run first — so `pytest tests/e2e -k observe` is a real,
runnable command and not a broken one.

The `tackle` leg is what issue #54 U7 added, and it is the reason the
rest of the scenario exists. Until U6 the adapter reached `tackle`, `run`
and `land` by shelling out to `cs` — a binary its own Dockerfile has
never shipped — so all three failed against the image an operator
actually deploys, while every in-process suite stayed green. U6 cut
dispatch over to `cosmon_runtime::LibraryExecutor` over the tmux
transport port. Whether that is *true of the image* is not a claim any
in-process test can make, and it is the only claim these two tests make.

The worker is a dummy, and it is **not** in the image you deploy: the
`e2e` Dockerfile stage (selected by `deploy/docker-compose.e2e.yml`) adds
`tests/fakes/fake-claude` and a worker-side `cs` on top of `runtime`.
`test_shipped_image.py` is the other half of that claim.

`land` is still asserted as a NAMED refusal, but no longer because a
binary is missing: the door's decision half runs in-process and the
suite ARMS it in the throwaway galaxy, so the decision admits and the
refusal comes from the effect half — `501 land_effect_unavailable`,
ADR-176 §12.
"""
from __future__ import annotations

import re

import pytest

pytestmark = pytest.mark.stack


def _refusal_label(payload, stderr):
    """The refusal's name, read from wherever the CLI put it.

    It may arrive on stdout (a JSON error envelope) or, when the CLI
    reports the refusal on its error stream, in stderr. Both are read;
    neither is invented — an unnamed refusal returns None and fails the
    assertion that asked for a name.
    """
    if isinstance(payload, dict):
        label = payload.get("error") or payload.get("label")
        if label:
            return label
    match = re.search(r'"(?:error|label)"\s*:\s*"([a-z_]+)"', stderr)
    return match.group(1) if match else None


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

    @pytest.mark.requires_dispatch
    def test_tackle_spawns_a_real_worker_from_the_image(
        self, dispatched, molecule, cfg, expect
    ):
        """`POST /v1/molecules/:id/tackle` — dispatch, in the container.

        What has to be true for this to pass, and was not true before U6:
        the adapter resolves the molecule and its formula in-process, cuts
        a git worktree, writes the dispatch ledger entry BEFORE the spawn,
        opens a tmux session running the resolved adapter command under
        the worker envelope's `env -i`, and pastes the briefing into it —
        all with no `cs` binary anywhere in its own image.

        The `dispatched` fixture already refused a 200 that carried no
        session name. What is asserted here is the OTHER half of the
        dispatch: the worktree, on the bind mount, where the host can see
        it. A session name with no worktree beside it would mean the
        ledger and the pane disagree about what was dispatched.
        """
        worktree = cfg.galaxy / ".worktrees" / molecule
        expect.truthy(
            worktree.is_dir(),
            f"the library tackle executor cuts the worker's worktree with `git worktree add` "
            f"inside the tenant tree, so a dispatch that really happened leaves one at "
            f"{worktree} on the host; a session without one means the spawn and the ledger "
            "describe different things",
        )

    def test_tackle_refuses_by_name_when_the_image_cannot_dispatch(
        self, tackle_attempt, cfg, expect
    ):
        """The falsifier: an image whose `tackle` must refuse, named.

        Runs only when `RPP_E2E_EXPECT_TACKLE_LABEL` is set — that is how
        the pre-U6 claim is falsified without a second harness: build the
        images from a checkout that predates the library cut-over
        (`RPP_E2E_BUILD_ROOT`, `RPP_E2E_E2E_STAGE=0`) and pin
        `tackle_unavailable`. Everything downstream is deselected, since
        a refused dispatch has no worker to wait for.

        With no label pinned (the default) this test asserts the
        complement, on the same single attempt the journey uses: dispatch
        must SUCCEED. That keeps the falsifier from being a branch
        nothing exercises on an ordinary run — and reading the shared
        `tackle_attempt` rather than tackling again is what makes the two
        branches statements about the same event.
        """
        rc, payload, stderr = tackle_attempt
        if not cfg.expect_tackle_label:
            expect.equals(
                rc, 0,
                "with no RPP_E2E_EXPECT_TACKLE_LABEL pinned, dispatch against the e2e image "
                "must succeed — issue #54 U6 made the route library-direct",
            )
            return
        expect.truthy(
            rc != 0,
            f"tackle SUCCEEDED, but RPP_E2E_EXPECT_TACKLE_LABEL pinned the refusal "
            f"'{cfg.expect_tackle_label}'",
        )
        expect.equals(
            _refusal_label(payload, stderr),
            cfg.expect_tackle_label,
            "ADR-176: every refusal carries a label a caller can branch on; a dispatch that "
            "fails for an unnamed reason is indistinguishable from a crash",
        )

    @pytest.mark.requires_dispatch
    def test_the_spawned_worker_completes_the_molecule(self, worked, expect):
        """The worker reads its briefing and drives the molecule home.

        Proves what `tackle` alone cannot: the briefing reached the pane,
        the worker's environment is usable, and the tenant store the
        worker writes is the one the API reads. The `worked` fixture does
        the polling — through the API, because the question is what a
        tenant can observe — and captures the pane before teardown when
        it does not arrive.
        """
        expect.equals(
            worked,
            "completed",
            "the dummy agent runs `cs complete` off the briefing it was handed; any other "
            "terminal status means the pane got something it could not act on",
        )

    @pytest.mark.requires_dispatch
    def test_land_returns_its_named_refusal(self, logged_in, molecule, worked, cfg, expect):
        """The harvest door refuses, and refuses *by name*.

        The assertion is the NAME of the refusal and the CLI's exit code,
        not merely "it failed": a door that refuses for an unnamed reason
        is the defect ADR-176 exists to prevent, and a 500 would satisfy
        a "non-zero exit" assertion just as well as the right refusal
        does.

        This runs on a molecule that is genuinely harvestable: completed
        by a real worker, in a galaxy whose operator armed
        `[harvest_authority] required` at stage time. That is what makes
        it reach the EFFECT half. Every pre-effect refusal —
        `not_authorized`, `not_completed`, `reservation_requires_seal`,
        `backlog_full` — has been made inapplicable on purpose, so the
        only thing left to answer is the transaction itself, and it
        answers `501 land_effect_unavailable`: the sealed `cs done` path
        has exactly one implementation and it is not callable as a
        library yet (ADR-176 §12). The refusal is the CONTRACT here, not
        a defect to route around.
        """
        rc, payload, stderr = logged_in.land(molecule)
        expect.truthy(
            rc != 0,
            "the sealed effect half has no library implementation, so a zero exit here would "
            "mean the door integrated nothing and said otherwise",
        )
        expect.equals(
            _refusal_label(payload, stderr),
            cfg.expect_land_label,
            "ADR-176 §12: the effect half refuses `land_effect_unavailable` until "
            "SealedHarvestEffect grows a library implementation. This is where that day "
            "announces itself — set RPP_E2E_EXPECT_LAND_LABEL and update this expectation "
            "in the same commit",
        )
