# SPDX-License-Identifier: AGPL-3.0-only
"""Run configuration: every knob the suite reads, resolved once.

Two kinds of value live here and the distinction is the point of the
whole harness. Some values are *provisioned* — the audience, the issuer,
the subject — and the run writes them into the IdP, the nucleon binding
and the client together. Others are *expected* — what a step must
observe. Overriding an expectation alone turns exactly one test red,
which is what makes a green run evidence rather than a tautology:
turning a provisioning knob and watching everything stay green proves
only that the world is consistent with itself.
"""
from __future__ import annotations

import os
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Tuple

#: `cosmon-remote login` binds its OAuth redirect catcher on this fixed
#: loopback port (``oidc::loopback::DEFAULT_REDIRECT_PORT``). It is not
#: configurable from here, so it is checked like a published port.
REDIRECT_CATCHER_PORT = 7777


def _env(name: str, default: str) -> str:
    value = os.environ.get(name)
    return default if value is None or value == "" else value


@dataclass(frozen=True)
class E2EConfig:
    """Everything a run needs, resolved from the environment exactly once."""

    repo_root: Path
    run_dir: Path
    stamp: str
    project: str
    rpp_port: int
    oidc_port: int
    audience: str
    idp_sub: str
    noyau: str
    #: Falsifier: what ``/v1/auth/me`` must report as ``sub``.
    expect_sub: str
    #: Falsifier: the audience the CLIENT asks for (server-side pin is
    #: :attr:`audience`; moving them apart must break ``login``).
    client_audience: str
    #: Falsifier: the lifecycle status ``observe`` must report.
    expect_observe_status: str
    #: Falsifier: the named refusal the ``land`` door must return.
    expect_land_label: str
    #: Falsifier: the named refusal ``tackle`` must return. Empty (the
    #: default) means tackle must SUCCEED. Naming a label here points the
    #: suite at an image whose dispatch cannot work — how the pre-U6
    #: claim is falsified without a second harness: build from a checkout
    #: that predates the library cut-over (:attr:`build_root`) and pin
    #: ``tackle_unavailable``.
    expect_tackle_label: str
    #: The workspace the two images are BUILT from. Defaults to
    #: :attr:`repo_root`. The staged compose files' build context is
    #: rewritten to this absolute path, so the compose file can come from
    #: this checkout while the source tree comes from another one.
    build_root: Path
    #: Layer ``deploy/docker-compose.e2e.yml`` on top of the deployment
    #: compose file: the adapter image is then built from the
    #: Dockerfile's ``e2e`` target, which adds the dummy agent and the
    #: worker-side ``cs``. Set False for a build root with no such stage.
    e2e_stage: bool
    #: Seconds to wait for a spawned worker to drive its molecule to
    #: ``completed``.
    worker_timeout: int
    #: Whether the stack is torn down (``down -v``) and reprovisioned
    #: between test sets. Only a falsification run sets this to False.
    reinit: bool
    #: Leave the stack up after the session (operator debugging).
    keep: bool
    #: Pre-built binary, if the caller does not want the cargo build.
    remote_bin: str
    #: Real-IdP profile: when set, the client fetches discovery from this
    #: issuer instead of the containerised mock. See the `mock_oidc`
    #: fixture's docstring for what else must then be provisioned.
    issuer_override: str

    @property
    def issuer(self) -> str:
        """Where the CLIENT fetches `/.well-known/openid-configuration`.

        In the reference deployment this is the compose service name; the
        tenant CLI here runs on the host, so it is the published loopback
        port. The adapter is indifferent: it pins the JWKS from disk and
        never dials the issuer.
        """
        return self.issuer_override or f"http://127.0.0.1:{self.oidc_port}"

    @property
    def host_url(self) -> str:
        """The adapter's base URL as seen from the host."""
        return f"http://127.0.0.1:{self.rpp_port}"

    @property
    def deploy_src(self) -> Path:
        """The tracked deploy tree. Read only — never written to."""
        return self.repo_root / "crates" / "cosmon-rpp-adapter" / "deploy"

    @property
    def staged_deploy(self) -> Path:
        """The throwaway copy of ``deploy/`` this run drives."""
        return self.run_dir / "deploy"

    @property
    def compose_file(self) -> Path:
        return self.staged_deploy / "docker-compose.yml"

    @property
    def compose_e2e_file(self) -> Path:
        """The test-only override layered on when :attr:`e2e_stage`.

        Kept a SEPARATE file rather than a flag on the deployment one, so
        the compose configuration an operator reads renders identically
        whether or not a smoke ever ran.
        """
        return self.staged_deploy / "docker-compose.e2e.yml"

    @property
    def compose_files(self) -> Tuple[Path, ...]:
        """The ``-f`` list, in order. ``up`` and ``down`` MUST see the same set.

        A teardown that forgot the override would leave the e2e-tagged
        image's containers behind, and the next run would meet them as
        "port already allocated".
        """
        if self.e2e_stage:
            return (self.compose_file, self.compose_e2e_file)
        return (self.compose_file,)

    @property
    def galaxies_root(self) -> Path:
        """Host side of the ``/cosmon/galaxies`` bind-mount."""
        return self.run_dir / "galaxies"

    @property
    def galaxy(self) -> Path:
        """The single throwaway tenant tree this run nucleates into."""
        return self.galaxies_root / self.noyau

    @property
    def artifacts(self) -> Path:
        """Where every request, response and container log is written."""
        return self.run_dir / "artifacts"

    @classmethod
    def from_env(cls, repo_root: Path) -> "E2EConfig":
        stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        # The run dir MUST sit under a path the container engine can
        # bind-mount: colima and Docker Desktop share $HOME, not
        # /var/folders — hence a default under the repo, not a tmpdir.
        run_dir = Path(_env("RPP_E2E_RUN_DIR", str(repo_root / ".rpp-remote-e2e" / stamp)))
        if not run_dir.is_absolute():
            run_dir = (repo_root / run_dir).resolve()
        audience = _env("RPP_E2E_AUDIENCE", "cosmon-rpp-tenant-demo")
        # The mock IdP's `--subject` default: `/authorize` signs in as
        # this when the request carries no `login_hint`, and
        # cosmon-remote sends none.
        idp_sub = _env("RPP_E2E_IDP_SUB", "cs-oidc-mock-user")
        build_root = Path(_env("RPP_E2E_BUILD_ROOT", str(repo_root))).resolve()
        return cls(
            repo_root=repo_root,
            run_dir=run_dir,
            stamp=stamp,
            project=_env("RPP_E2E_PROJECT", f"cs-rpp-e2e-{os.getpid()}"),
            rpp_port=int(_env("RPP_E2E_RPP_PORT", "18443")),
            oidc_port=int(_env("RPP_E2E_OIDC_PORT", "18444")),
            audience=audience,
            idp_sub=idp_sub,
            noyau=_env("RPP_E2E_NOYAU", "e2e-noyau"),
            expect_sub=_env("RPP_E2E_EXPECT_SUB", idp_sub),
            client_audience=_env("RPP_E2E_CLIENT_AUDIENCE", audience),
            # A molecule nucleated over the API is assigned to nobody, so
            # `cosmon_core::nucleate` leaves it `Pending` (it would be
            # `Queued` if assigned) and observe renders the snake_case
            # label.
            expect_observe_status=_env("RPP_E2E_EXPECT_STATUS", "pending"),
            # `land` no longer refuses for want of a binary. The door's
            # decision half runs in-process (issue #54 U3) and this suite
            # ARMS it in the throwaway galaxy, so the decision ADMITS and
            # the refusal is the effect half's: `501
            # land_effect_unavailable` (ADR-176 §12). The label is
            # deliberately outside the closed seven-refusal set — it
            # names a missing implementation, not a verdict about this
            # molecule.
            expect_land_label=_env("RPP_E2E_EXPECT_LAND_LABEL", "land_effect_unavailable"),
            expect_tackle_label=os.environ.get("RPP_E2E_EXPECT_TACKLE_LABEL", ""),
            build_root=build_root,
            e2e_stage=_env("RPP_E2E_E2E_STAGE", "1") != "0",
            worker_timeout=int(_env("RPP_E2E_WORKER_TIMEOUT", "120")),
            reinit=_env("RPP_E2E_REINIT", "1") != "0",
            keep=_env("RPP_E2E_KEEP", "0") == "1",
            remote_bin=os.environ.get("COSMON_REMOTE_BIN", ""),
            issuer_override=os.environ.get("RPP_E2E_ISSUER", ""),
        )
