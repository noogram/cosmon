# SPDX-License-Identifier: AGPL-3.0-only
"""The tenant's journey: login → auth me → nucleate → observe → tackle →
worker → done.

One test set (one class): the stack is reinitialised before it and the
whole journey runs against that clean stack. Each leg is its own test
and reaches the state it needs through fixtures, not through a sibling
test having run first — so `pytest tests/e2e -k observe` is a real,
runnable command and not a broken one.

The `tackle` leg is what issue #54 U7 added, and it is the reason the
rest of the scenario exists. Until U6 the adapter reached `tackle`, `run`
and the harvest door by shelling out to `cs` — a binary its own Dockerfile has
never shipped — so all three failed against the image an operator
actually deploys, while every in-process suite stayed green. U6 cut
dispatch over to `cosmon_runtime::LibraryExecutor` over the tmux
transport port. Whether that is *true of the image* is not a claim any
in-process test can make, and it is the only claim these two tests make.

The worker is a dummy, and it is **not** in the image you deploy: the
`e2e` Dockerfile stage (selected by `deploy/docker-compose.e2e.yml`) adds
`tests/fakes/fake-claude` and a worker-side `cs` on top of `runtime`.
`test_shipped_image.py` is the other half of that claim.

`done` — one gesture again since issue #51, which withdrew the second
`land` verb — no longer refuses at all, and that change of verdict is
what issues #67 and #68 are. The decision half runs in-process and the
suite ARMS `[harvest_authority]` in the throwaway galaxy, so the
decision admits; the effect half is a library the adapter links
(`cosmon-harvest`) and is the DEFAULT, so it runs on a stock image; and
the suite now also seals the ADR-172 grant that armed galaxy demands,
because a deployment with the switch on and no trust root is the shape a
stock deployment must never be left in. So the journey ends where a
tenant's journey ends: a merge commit on the base branch, carrying the
lineage trailers `cs done` writes.

`v1_done_library_effect.rs` proves the same merge in-process. What this
file adds is the only thing an in-process test cannot say: that it
happens through the image an operator deploys, on a bind-mounted tenant
tree, driven by the real `cosmon-remote`. ADR-176 §12 as amended by
issues #51, #62, #67 and #68.
"""
from __future__ import annotations

import re
import subprocess

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



def _git(galaxy, *args) -> str:
    """Read something out of the tenant repository, from the HOST side.

    The bind-mount is the point. The adapter merged inside a container;
    what an operator has afterwards is this directory, and a merge that
    is only visible from inside the container is not a merge they got.
    Returns stripped stdout, or `""` when git refused — the callers turn
    an empty answer into their own failure message rather than raising a
    `CalledProcessError` that names none of the context.
    """
    proc = subprocess.run(
        ["git", "-C", str(galaxy), *args],
        capture_output=True,
        text=True,
    )
    return proc.stdout.strip() if proc.returncode == 0 else ""


def _git_ok(galaxy, *args) -> bool:
    """Whether a git command succeeded — for the existence probes."""
    return subprocess.run(
        ["git", "-C", str(galaxy), *args],
        capture_output=True,
        text=True,
    ).returncode == 0


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


    def test_done_refuses_a_reason_it_would_have_to_invent(
        self, logged_in, molecule, expect
    ):
        """A harvest with no reason is refused `missing_reason`, named.

        The gap the reporters found in the withdrawn `land`: it invented
        a generic sentence, and a year later an invented sentence is
        indistinguishable from one somebody meant. `cs done` at the
        operator's own terminal may record nothing — the operator authors
        the history — but a requester reaching over §8p does not, and the
        trunk-side reason is the only account a later reader has of why
        someone else's molecule was closed.

        The blank reason is how the CLI expresses this: clap declares
        `--reason` a required `String`, so a body with the field ABSENT is
        unreachable from here and is covered wire-side by the adapter's
        own `v1_done.rs`. Both arrive at the same check —
        `HarvestOptions::validate` trims and refuses — and the check is
        the claim.

        This runs BEFORE anything is integrated and integrates nothing: a
        refusal at step 4 of the route never reaches the molecule, so the
        journey's molecule is as harvestable after this test as before.
        """
        rc, payload, stderr = logged_in.done(molecule, "   ")
        expect.truthy(
            rc != 0,
            "a harvest carrying no reason must be refused, not completed with a "
            "sentence the door made up",
        )
        expect.equals(
            _refusal_label(payload, stderr),
            "missing_reason",
            "ADR-176: the reason is mandatory on this route and never fabricated; "
            "an unnamed 400 here would be a refusal a caller cannot branch on",
        )

    def test_done_refuses_a_strategy_it_does_not_implement(
        self, logged_in, molecule, expect
    ):
        """A `--strategy` outside the closed set is refused, not defaulted.

        The D4 reversal put the full parameter set of `cs done` on the
        wire, and `deny_unknown_fields` plus a token-parsed
        `MergeStrategy` is what keeps that from becoming a silent
        best-effort: a body naming a strategy this build has never heard
        of must be TOLD, because being quietly harvested with `merge`
        after asking for something else is the failure the requester
        cannot see.

        Like the missing reason, this is refused before the molecule is
        loaded, so it consumes nothing.
        """
        rc, payload, stderr = logged_in.done(
            molecule, "a strategy nobody implements", strategy="rebase-and-pray"
        )
        expect.truthy(
            rc != 0,
            "an unimplemented merge strategy must be refused; a 200 here would mean "
            "the door silently harvested with a strategy the requester did not ask for",
        )
        expect.equals(
            _refusal_label(payload, stderr),
            "unsupported_parameter",
            "`MergeStrategy::from_token` knows `merge` and `ff-only` and nothing else; "
            "the route maps the miss onto `400 unsupported_parameter`",
        )

    @pytest.mark.requires_dispatch
    def test_done_merges_the_branch_and_stamps_the_lineage(
        self, logged_in, sealed, worked, cfg, expect
    ):
        """The harvest door MERGES — through the image, onto the trunk.

        The end of the tenant's journey, and the claim issues #67 and #68
        exist to make. Everything upstream has been made true on purpose:
        the molecule is `completed` by a worker the adapter really
        spawned, its branch carries that worker's commit, the galaxy armed
        `[harvest_authority] required` so the decision half admits, and
        the `sealed` fixture pinned the trust root and the one
        molecule-scoped grant the effect half demands inside the trunk
        lock. What is left to answer is the transaction itself.

        It answers by moving the base branch. Three things are asserted
        and none of them is the HTTP status alone:

        * `outcome` is `landed`. Three of the four success outcomes —
          `closed_without_merge`, `no_op`, `already_landed` — put nothing
          on the trunk, and a client reading the 200 alone would believe
          the branch shipped. `merged` is asserted beside it because that
          is the field the envelope publishes for exactly this question.
        * the base branch's tip MOVED, and the worker's file is reachable
          from it. A route that reported `landed` while `main` stood still
          is the defect issue #51 reported, restated.
        * the merge commit carries `Mol-Id:`. The lineage trailers are
          derived from the ledger by `cs done` and stamped on the
          completion merge; a merge commit without them is a merge some
          other code path made.

        `--strategy merge` is sent explicitly rather than left to the
        default: the D4 reversal is the claim that a requester's
        parameters reach the merge, and a test that sent none would pass
        identically if they were dropped on the way.
        """
        molecule = sealed
        base = cfg.base_branch
        before = _git(cfg.galaxy, "rev-parse", base)
        rc, payload, stderr = logged_in.done(
            molecule, "closed by the rpp-remote end-to-end walk", strategy="merge"
        )
        expect.equals(
            rc, 0,
            "an armed AND sealed galaxy must MERGE on a stock deployment: "
            "`harvest_effect_unavailable` means this image lost the library harvest, "
            "`not_authorized` means the seal the fixture wrote did not verify, and "
            f"`harvest_failed` means the transaction ran and lost the NAME of its "
            f"refusal — {stderr[-600:]}",
        )
        harvest = (payload or {}).get("harvest", {})
        expect.equals(
            harvest.get("outcome"),
            cfg.expect_done_outcome,
            "`landed` is the one success outcome that put something on the trunk "
            "(override with RPP_E2E_EXPECT_DONE_OUTCOME to falsify)",
        )
        expect.equals(
            harvest.get("merged"),
            True,
            "the envelope publishes `merged` precisely so a client need not infer "
            "integration from a 200",
        )
        after = _git(cfg.galaxy, "rev-parse", base)
        expect.truthy(
            before and after and before != after,
            f"the base branch `{base}` must have advanced: it was {before or '<none>'} "
            f"and is {after or '<none>'}",
        )
        expect.truthy(
            _git_ok(cfg.galaxy, "cat-file", "-e", f"{base}:worker-output-{molecule}.txt"),
            f"the worker's own file must be reachable from `{base}` after the harvest; "
            "a moved tip with none of the branch's content is a bookkeeping commit, "
            "not a merge",
        )
        message = _git(cfg.galaxy, "log", "-1", "--format=%B", base)
        expect.truthy(
            f"Mol-Id: {molecule}" in message,
            "delib-20260720-cff4: `cs done` stamps the ledger-derived lineage trailers "
            f"on the completion merge; this one reads:\n{message}",
        )


class TestDoneWithoutMerge:
    """A second set: the same journey, closed with `--no-merge`.

    Its own class, so it gets its own reinit, its own tenant and its own
    molecule — the state under test is the base branch, and a set that
    closed a second molecule in the first set's galaxy would be reading a
    trunk the previous set had already moved.

    This is the #62 review fix, and it is the falsifier for the merge
    test above rather than a variation on it: same image, same seal, same
    completed molecule with a commit on its branch, one flag different,
    opposite verdict on the trunk. A build that ignored `no_merge` would
    turn exactly this class red and leave the merge test green.

    The seal is deliberately provisioned and deliberately unspent.
    `--no-merge` takes no trunk lock, so the ADR-172 effect boundary is
    never reached and no authority is consumed — which means the set
    could have run without one. Sealing anyway is what makes the flag the
    ONLY difference between this class and the merge test: drop the seal
    and a reader could not tell which of the two changes moved the
    verdict.
    """

    def test_no_merge_succeeds_and_integrates_nothing(
        self, logged_in, sealed, worked, cfg, expect
    ):
        """`no_merge: true` — a success that must NOT move the trunk.

        `--no-merge` is the one parameter whose correct behaviour looks
        like failure from the status line: the route answers 200, and
        that 200 must be readable as "closed, nothing integrated" rather
        than as a landing. So `merged` is asserted false and the base
        branch is asserted UNCHANGED — the second is what a mis-wired
        flag would break while the first still passed, because a
        transaction that merged and mis-reported would answer `merged`
        from the flag it was given rather than from what it did.

        The `non_integration` tag travels with the reply for the same
        reason: a requester who reads `merged: false` should not have to
        fetch a second route to learn why.
        """
        molecule = sealed
        base = cfg.base_branch
        before = _git(cfg.galaxy, "rev-parse", base)
        rc, payload, stderr = logged_in.done(
            molecule, "closed without integrating, on purpose", no_merge=True
        )
        expect.equals(
            rc, 0,
            f"`--no-merge` mutates no trunk and therefore spends no authority; it must "
            f"succeed — {stderr[-600:]}",
        )
        harvest = (payload or {}).get("harvest", {})
        expect.equals(
            harvest.get("merged"),
            False,
            "a closure that skipped the merge integrated nothing, and the envelope must "
            "say so rather than let a 200 stand for a landing",
        )
        after = _git(cfg.galaxy, "rev-parse", base)
        expect.equals(
            after,
            before,
            f"`{base}` must be exactly where it was: a moved tip under `merged: false` "
            "means the reply is reporting the flag rather than the effect",
        )
