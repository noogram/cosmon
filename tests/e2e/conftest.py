# SPDX-License-Identifier: AGPL-3.0-only
"""Fixtures for the container-level end-to-end suite.

Read this file first: the fixtures below are where the run's shape is
decided — what is built once, what is destroyed between test sets, and
what the mock IdP does and does not prove.

Running it
----------

::

    pip install -r tests/e2e/requirements.txt
    pytest tests/e2e                                  # the whole scenario
    pytest tests/e2e -k healthz                       # one test, alone
    pytest tests/e2e --junitxml=e2e-report.xml        # a report CI reads
    pytest tests/e2e --pdb                            # break in on failure
    pytest tests/e2e -x --lf                          # rerun the last red

Every request, every response and the compose logs of each set land under
``$RPP_E2E_RUN_DIR/artifacts`` (default ``.rpp-remote-e2e/<stamp>/``),
one file per exchange plus the familiar ``e2e.ndjson`` one-line-per-step
record.

Exit codes: 0 green, 1 a test failed, **2 a prerequisite is missing**.
There is no skip. A harness that prints green when it did not run is
worse than no harness — it converts an absent docker daemon into a
passing nightly.
"""
from __future__ import annotations

import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Optional

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))

from harness.compose import ComposeStack, PrerequisiteMissing, check_prerequisites  # noqa: E402
from harness.config import E2EConfig  # noqa: E402
from harness.expect import Expect  # noqa: E402
from harness.record import Recorder  # noqa: E402
from harness.remote import RemoteCli  # noqa: E402

#: Set when a prerequisite was missing, so the session can exit 2.
_PREREQUISITE_ERROR: Optional[str] = None


def pytest_sessionfinish(session, exitstatus):  # noqa: D401 - pytest hook
    """Map "could not run" onto exit code 2, distinct from a red test.

    A nightly must be able to tell a failing scenario from a runner that
    never started: the first is a finding about the product, the second
    is a finding about the machine.
    """
    if _PREREQUISITE_ERROR is not None:
        session.exitstatus = 2


@pytest.fixture(scope="session")
def repo_root() -> Path:
    """The workspace root, found by walk-up — never hard-coded.

    The suite is invoked from CI, from the repo root and from a worktree;
    a relative path that is right in one of those is wrong in the others.
    """
    here = Path(__file__).resolve()
    for candidate in here.parents:
        if (candidate / "Cargo.toml").is_file() and (candidate / "crates").is_dir():
            return candidate
    raise AssertionError(f"no cosmon workspace root above {here}")


@pytest.fixture(scope="session")
def cfg(repo_root: Path) -> E2EConfig:
    """The run configuration, resolved once (see harness/config.py)."""
    config = E2EConfig.from_env(repo_root)
    config.run_dir.mkdir(parents=True, exist_ok=True)
    return config


@pytest.fixture(scope="session")
def recorder(cfg: E2EConfig) -> Recorder:
    """The on-disk record of every exchange in the session."""
    return Recorder.open(cfg.artifacts)


@pytest.fixture(scope="session")
def remote_binary(cfg: E2EConfig) -> Path:
    """The compiled ``cosmon-remote`` the tenant drives the stack with.

    Built once per session, in release, because that is the binary an
    operator installs. ``COSMON_REMOTE_BIN`` skips the build for a
    developer iterating on the suite itself.
    """
    if cfg.remote_bin:
        binary = Path(cfg.remote_bin)
    else:
        proc = subprocess.run(
            ["cargo", "build", "--release", "--locked", "-p", "cosmon-remote", "--bin", "cosmon-remote"],
            cwd=cfg.repo_root,
            capture_output=True,
            text=True,
        )
        if proc.returncode != 0:
            raise AssertionError(
                "cargo build -p cosmon-remote failed:\n" + proc.stderr[-4000:]
            )
        binary = cfg.repo_root / "target" / "release" / "cosmon-remote"
    if not binary.is_file():
        raise AssertionError(f"cosmon-remote binary not found at {binary}")
    return binary


@pytest.fixture(scope="session")
def compose_stack(cfg: E2EConfig, recorder: Recorder) -> ComposeStack:
    """The staged deploy tree with both images built. Session-scoped.

    Session-scoped because both Dockerfiles do a full
    ``cargo build --release --locked``: minutes of work that must be paid
    once, never per test. What is emphatically *not* session-scoped is
    the containers' state — see :func:`stack`.

    Prerequisites are checked here, and a missing one raises rather than
    skipping: see the module docstring on exit code 2.
    """
    global _PREREQUISITE_ERROR
    try:
        check_prerequisites(cfg)
    except PrerequisiteMissing as exc:
        _PREREQUISITE_ERROR = str(exc)
        pytest.fail(f"prerequisite missing (exit 2): {exc}", pytrace=False)
    stack = ComposeStack(cfg, recorder)
    stack.stage()
    stack.build()
    yield stack
    if cfg.keep:
        print(
            f"\n==> RPP_E2E_KEEP=1: leaving project '{cfg.project}' up; "
            f"down with: docker compose -p {cfg.project} -f {cfg.compose_file} down -v"
        )
    else:
        stack.down()
    print(f"\n==> e2e record: {cfg.artifacts}")


@pytest.fixture(scope="class")
def stack(compose_stack: ComposeStack, cfg: E2EConfig, request) -> ComposeStack:
    """A freshly initialised stack, per test set (one class = one set).

    ``down -v`` + ``up --wait`` + provision, plus a new tenant galaxy
    tree. This is the reviewer's third point, and it is not a nicety: a
    set that inherits the previous set's molecules, inbox and
    rate-limiter buckets is not reproducible, and the first symptom is a
    test that passes alone and fails in the suite.

    Set ``RPP_E2E_REINIT=0`` to disable the boundary. That is a
    falsification switch, not a mode: :mod:`test_reinit` is RED with it
    and GREEN without it, which is what makes the reinit a claim this
    suite can support rather than one it merely asserts.
    """
    if cfg.reinit or compose_stack.generation == 0:
        compose_stack.reinit()
        # Each set's container logs, kept per generation so a failure in
        # set 3 is not read against set 1's output. The generation is
        # captured now, not at teardown: by then the next set may have
        # incremented it and the file would name the wrong set.
        gen = compose_stack.generation

        def _keep_logs() -> None:
            (compose_stack.logs_dir / f"gen-{gen}-adapter.log").write_text(
                compose_stack.adapter_log_tail(1000), encoding="utf-8"
            )

        request.addfinalizer(_keep_logs)
    return compose_stack


@pytest.fixture(scope="class")
def mock_oidc(stack: ComposeStack, cfg: E2EConfig):
    """The containerised mock IdP (``cs-oidc-mock``), asserted reachable.

    **Caveat — this is a mock, and a mock may behave in ways a real
    provider does not.** What the suite proves with it is the *shape* of
    the flow: discovery, an authorization-code redirect, PKCE-S256 on the
    token exchange, a signed JWT whose ``(iss, sub, aud)`` the adapter
    resolves against a nucleon binding. What it does not prove is
    anything that depends on a real provider's policy. Known deviations,
    to be read before treating a green here as production evidence:

    * ``/authorize`` **auto-approves**. There is no login form, no
      consent screen and no MFA, so nothing here exercises a redirect
      chain, a session cookie, or an interactive timeout.
    * ``sub`` is fixed by ``--subject``. A real IdP mints an opaque,
      per-user identifier and may change its shape (or rotate it) without
      notice; a binding pinned to a readable ``sub`` is a mock artefact.
    * **Discovery is minimal.** ``/.well-known/openid-configuration``
      carries what this client reads and no more. A real document
      advertises many further fields (``userinfo_endpoint``,
      ``end_session_endpoint``, ``claims_supported``, several
      ``*_supported`` arrays), and a client that grew to depend on one of
      them would pass here and fail against production.
    * **Token response order and extras.** The mock returns a compact
      JSON object; real providers add fields (``id_token``,
      ``refresh_token``, ``scope``, vendor claims) and give no ordering
      guarantee. Nothing may be asserted positionally.
    * **Keys do not rotate, and expiry is generous.** The JWKS is minted
      at boot and stays; no ``kid`` rollover, no re-fetch on an unknown
      ``kid``, no clock-skew edge.
    * **No refresh, no revocation, no introspection.** Those endpoints do
      not exist, so no test here can cover the paths that use them.

    Running the same tests against a real IdP
    ----------------------------------------

    The suite reads the provider entirely from configuration, so the same
    scenario runs against a real one with no code change::

        RPP_E2E_ISSUER=https://idp.example/realms/cosmon \\
        RPP_E2E_AUDIENCE=<the client_id registered there> \\
        RPP_E2E_IDP_SUB=<the sub that IdP mints for the test principal> \\
        RPP_E2E_EXPECT_SUB=<the same value> \\
        pytest tests/e2e -m stack

    Two things must then be provisioned out of band, exactly as an
    operator would: the JWKS the adapter pins from disk must be the real
    provider's (it is not fetched — see the compose file's
    ``rpp-jwks`` volume), and the redirect URI
    ``http://127.0.0.1:7777/callback`` must be registered on the client.
    A profile that does both is the honest way to promote a green here
    into a claim about production.
    """
    status, doc, body = stack.get_json("/healthz", step="healthz (mock_oidc precondition)")
    if status != 200:
        raise AssertionError(f"the stack is not answering /healthz: HTTP {status} {body[:200]}")
    return cfg.issuer


@pytest.fixture
def expect(recorder: Recorder, request) -> Expect:
    """Assertions that quote the run when they break (harness/expect.py).

    Bound to the live stack when the test has one, so a failure report
    carries the adapter log tail alongside the request and the response.
    """
    stack_obj = request.getfixturevalue("stack") if "stack" in request.fixturenames else None
    tail = (lambda: stack_obj.adapter_log_tail()) if stack_obj is not None else None
    return Expect(recorder, tail)


@pytest.fixture(scope="class")
def tenant(cfg: E2EConfig, remote_binary: Path, recorder: Recorder, stack: ComposeStack, request) -> RemoteCli:
    """A configured ``cosmon-remote`` profile for this test set.

    Its ``$HOME`` is per-set: a credential minted before a reinit is
    signed by a key the restarted IdP no longer has.
    """
    slot = f"{request.cls.__name__ if request.cls else 'module'}-gen{stack.generation}"
    cli = RemoteCli(cfg, remote_binary, recorder, slot=slot)
    cli.configure()
    return cli


@pytest.fixture(scope="class")
def logged_in(tenant: RemoteCli, mock_oidc) -> RemoteCli:
    """A tenant that has completed the authorization-code + PKCE login.

    A fixture rather than a first test so that every later test can be
    selected and run alone (``pytest tests/e2e -k observe``) instead of
    depending on a sibling having run first.
    """
    proc = tenant.login()
    if proc.returncode != 0:
        raise AssertionError(
            "cosmon-remote login failed — the authorization-code + PKCE flow did not "
            f"complete against {tenant.cfg.issuer}:\n{proc.stderr[-2000:]}"
        )
    return tenant


@pytest.fixture(scope="class")
def molecule(logged_in: RemoteCli, cfg: E2EConfig) -> str:
    """One molecule, nucleated over the API, for the tests that read it back."""
    rc, payload, stderr = logged_in.nucleate("task-work", "container-level rpp smoke")
    if rc != 0 or not isinstance(payload, dict):
        raise AssertionError(f"nucleate failed (rc={rc}): {stderr[-2000:]}")
    # `.molecule.id`, spelled once and exactly: a fallback chain would
    # accept a drifted envelope as if nothing had changed, which is the
    # drift this suite exists to notice.
    mol_id = payload.get("molecule", {}).get("id")
    if not mol_id:
        raise AssertionError(f"nucleate returned no molecule id: {str(payload)[:400]}")
    return mol_id


@pytest.fixture(scope="class")
def tackle_attempt(logged_in: RemoteCli, molecule: str):
    """``POST /v1/molecules/:id/tackle``, attempted exactly ONCE per set.

    Both the success path and the falsifier read this one attempt rather
    than each calling the route: a second dispatch of the same molecule
    is a different request with a different answer, and two tests that
    each tackled would be asserting about two different events while
    reading as if they agreed. Returns ``(rc, payload, stderr)``
    unjudged — the judging belongs in the tests.
    """
    return logged_in.tackle(molecule)


@pytest.fixture(scope="class")
def dispatched(tackle_attempt, molecule: str) -> str:
    """The worker session name the image spawned, from that one attempt.

    The assertion is the SESSION NAME, not the HTTP status. A 200
    carrying no session would satisfy "it did not fail" while describing
    a dispatch that spawned nothing, and that is precisely the shape of
    the lie this whole suite exists to catch.

    Never reached in falsifier mode (``RPP_E2E_EXPECT_TACKLE_LABEL``):
    the tests that need it are deselected, because a refused dispatch has
    no worker to wait for and reporting the cascade would say the same
    thing four times.
    """
    rc, payload, stderr = tackle_attempt
    if rc != 0:
        raise AssertionError(
            f"POST /v1/molecules/{molecule}/tackle failed (rc={rc}): {stderr[-2000:]}"
        )
    # `.tackle.worker_session`, spelled once — see the `molecule` fixture
    # for why there is no fallback chain.
    session = (payload or {}).get("tackle", {}).get("worker_session")
    if not session:
        raise AssertionError(
            f"tackle answered 200 with no worker_session: {str(payload)[:400]}"
        )
    return session


@pytest.fixture(scope="class")
def worked(dispatched: str, logged_in: RemoteCli, molecule: str, cfg: E2EConfig,
           stack: ComposeStack) -> str:
    """Wait for the spawned worker to drive its molecule to ``completed``.

    The dummy agent (``tests/fakes/fake-claude`` in ``complete-molecule``
    mode, staged into the e2e image only) reads the briefing the adapter
    pasted into its pane, takes the molecule id out of it, and runs
    ``cs complete``. So this proves three things ``tackle`` alone cannot:
    the BRIEFING reached the pane, the worker's own environment is usable
    (``PATH``, ``COSMON_STATE_DIR`` pinned by the envelope), and the
    tenant store the worker writes is the same one the API reads.

    Polled through the API, not off the disk: the question is what a
    tenant can observe. Returns the terminal status.
    """
    deadline = time.time() + cfg.worker_timeout
    status = ""
    while time.time() < deadline:
        rc, payload, _ = logged_in.observe(molecule)
        status = (payload or {}).get("molecule", {}).get("status", "")
        if status == "completed":
            return status
        time.sleep(2)
    pane = stack.capture_worker_pane(dispatched)
    (stack.logs_dir / "worker-pane.log").write_text(pane, encoding="utf-8")
    raise AssertionError(
        f"molecule {molecule} is '{status or '<unreadable>'}' after {cfg.worker_timeout}s, "
        f"not 'completed'. The worker pane, captured before teardown:\n{pane[-4000:]}"
    )


def pytest_collection_modifyitems(config, items):  # noqa: D401 - pytest hook
    """Deselect the post-dispatch tests when a tackle refusal is pinned.

    ``RPP_E2E_EXPECT_TACKLE_LABEL`` points the suite at an image whose
    dispatch must REFUSE. A worker, a completion and the harvest door's
    effect half are then unreachable by construction. Deselecting them —
    rather than skipping — keeps the module's no-skip contract intact:
    nothing here prints green without having run.
    """
    if not os.environ.get("RPP_E2E_EXPECT_TACKLE_LABEL"):
        return
    kept, removed = [], []
    for item in items:
        (removed if item.get_closest_marker("requires_dispatch") else kept).append(item)
    if removed:
        config.hook.pytest_deselected(items=removed)
        items[:] = kept


def pytest_report_header(config):  # noqa: D401 - pytest hook
    """Name the run's shape in the report header, where CI shows it."""
    return f"cosmon rpp e2e — {time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}"
