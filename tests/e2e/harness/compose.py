# SPDX-License-Identifier: AGPL-3.0-only
"""The compose stack: staged, built once, reinitialised between test sets.

Three lifetimes, deliberately different:

* **the staged tree and the images** — once per session. Both Dockerfiles
  do a full ``cargo build --release --locked``; that is minutes, and
  paying it per test would make the suite unusable.
* **the containers and their volumes** — once per *test set*
  (:meth:`ComposeStack.reinit`, ``down -v`` then ``up --wait`` then
  provision). This is the reviewer's third point: a set that inherits the
  molecules, the inbox and the rate-limiter buckets of the set before it
  is not reproducible, and the first symptom is a test that passes alone
  and fails in the suite.
* **the tenant galaxy tree** — with the containers, for the same reason:
  it is state the previous set wrote, and ``down -v`` does not reach it
  (it is a bind-mount of a host directory, not a named volume).

There is no SKIP anywhere in this module. A missing prerequisite raises
:class:`PrerequisiteMissing`, which the session hook turns into exit
code 2 — the same contract the shell harness had, for the same reason: a
harness that prints green when it did not run converts an absent
prerequisite into a passing nightly.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import socket
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any, Dict, Optional, Tuple

from .config import REDIRECT_CATCHER_PORT, E2EConfig
from .record import Recorder


class PrerequisiteMissing(Exception):
    """A tool, daemon or port the suite needs is not available.

    Raised, never swallowed into a skip. :func:`conftest.pytest_sessionfinish`
    maps a session that hit one onto exit code 2, distinct from a red
    test (1) — "could not run" and "ran and failed" are different
    verdicts and a nightly must be able to tell them apart.
    """


def check_prerequisites(cfg: E2EConfig) -> None:
    """Fail loudly for everything the run needs and does not have."""
    for tool in ("docker",):
        if shutil.which(tool) is None:
            raise PrerequisiteMissing(
                f"`{tool}` is required by this suite and is not on PATH. "
                "This harness refuses to report success without running."
            )
    if _run(["docker", "compose", "version"]).returncode != 0:
        raise PrerequisiteMissing(
            "`docker compose` (v2) is required; the legacy docker-compose is not enough."
        )
    if _run(["docker", "info"]).returncode != 0:
        raise PrerequisiteMissing(
            "the docker daemon is not reachable (`docker info` failed). "
            "Start it (colima start / Docker Desktop) and re-run."
        )
    # Three ports must be free before anything is built: the two the
    # stack publishes and the fixed one `cosmon-remote login` binds its
    # redirect catcher on. Learning this from a compose failure ten
    # minutes into an image build — or from a five-minute login timeout —
    # is not a diagnosis. A previous run whose teardown did not complete
    # is the usual cause.
    for port, role in (
        (cfg.rpp_port, "the rpp-adapter"),
        (cfg.oidc_port, "the mock IdP"),
        (REDIRECT_CATCHER_PORT, "the OAuth redirect catcher"),
    ):
        if not _port_free(port):
            raise PrerequisiteMissing(
                f"TCP port {port} is already in use, and {role} needs it. "
                "A leftover stack? `docker ps` — then `docker compose -p <project> down -v`. "
                "Or point this run elsewhere: RPP_E2E_RPP_PORT / RPP_E2E_OIDC_PORT."
            )


def _port_free(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        return sock.connect_ex(("127.0.0.1", port)) != 0


def _run(cmd, **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, **kwargs)


#: Keys the loader (`HabilitationMap::load`) reads, as regexes over the
#: materialised binding. A template that has dropped one yields a file
#: that resolves no noyau and fails closed — which must be a red test
#: here, not a mystery at admission time.
def binding_required_patterns(cfg: E2EConfig) -> Tuple[Tuple[str, str], ...]:
    """(regex, why) pairs the materialised nucleon binding must satisfy."""
    return (
        (rf'^nucleon_id = "nuc-{re.escape(cfg.noyau)}"$',
         "the habilitation id must equal the directory name under nucleons/"),
        (r'^phase = "',
         "ADR-063 cognitive-substrate label; the loader reads it and has no default"),
        (rf'^noyau = "{re.escape(cfg.noyau)}"$',
         "the tenant axis: state materialises at <galaxies_root>/<noyau>/"),
        (r"^\[oidc\]$",
         "the (iss, sub, aud) triple lives in its own table"),
        (rf'^issuer = "{re.escape(cfg.issuer)}"$',
         "wiring contract A: byte-for-byte equal to the IdP's issuer"),
        (rf'^sub = "{re.escape(cfg.idp_sub)}"$',
         "the principal as the IdP signs it"),
        (rf'^audience = "{re.escape(cfg.audience)}"$',
         "pinned to this RPP instance; also the OAuth client_id"),
        (r"^\[scopes\]$",
         "T23 binding-granted scopes live in their own table"),
        (r"^allowed = \[",
         "T23: the binding closes the gap when the IdP cannot mint cosmon:* scopes"),
    )


def materialise_binding(cfg: E2EConfig, template: Path, target: Path) -> str:
    """Render the tracked ``.example`` into a real nucleon binding.

    The binding is materialised from the tracked template — never from an
    inline heredoc. The ``.example`` IS the artefact a fresh operator
    provisions from, so the suite must exercise exactly it: a template
    that has lost a key the loader reads produces a file that resolves no
    noyau, and this goes red instead of the run passing on a private copy
    the operator will never have.

    Returns the rendered text (also written to ``target``).
    """
    text = template.read_text(encoding="utf-8")
    for placeholder, value in (
        ("REPLACE_ME_NUCLEON_ID", f"nuc-{cfg.noyau}"),
        ("REPLACE_ME_NOYAU", cfg.noyau),
        ("REPLACE_ME_ISSUER", cfg.issuer),
        ("REPLACE_ME_SUB", cfg.idp_sub),
        ("REPLACE_ME_AUDIENCE", cfg.audience),
        ("REPLACE_ME_SEALED_AT", cfg.stamp),
    ):
        text = text.replace(placeholder, value)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(text, encoding="utf-8")
    return text


def unsubstituted_placeholders(rendered: str) -> Tuple[str, ...]:
    """Placeholders the template still carries outside its comments.

    Comment lines are excluded on purpose: the template's own header
    names the placeholder token to tell the operator what to replace, and
    a scan that could not tell that sentence from an unsubstituted value
    would make the header unwritable.
    """
    body = [line for line in rendered.splitlines() if not line.lstrip().startswith("#")]
    return tuple(sorted(set(re.findall(r"REPLACE_ME_[A-Z_]*", "\n".join(body)))))


class ComposeStack:
    """The staged deploy tree and the containers it brings up."""

    def __init__(self, cfg: E2EConfig, recorder: Recorder) -> None:
        self.cfg = cfg
        self.recorder = recorder
        self.logs_dir = cfg.run_dir / "logs"
        self.logs_dir.mkdir(parents=True, exist_ok=True)
        #: Incremented on every reinit; the reinit test reads it to prove
        #: the boundary it relies on actually happened.
        self.generation = 0

    # -- staging ------------------------------------------------------

    def stage(self) -> None:
        """Copy ``deploy/`` and provision what a fresh operator provisions.

        The tracked deploy tree is never written to: it ships the nucleon
        binding only as ``oidc-identity.toml.example`` (the real file is
        gitignored, as it must be), so a clean checkout cannot boot the
        stack without materialising one — and that materialisation is the
        operator gesture this method performs, in a copy.
        """
        cfg = self.cfg
        cfg.staged_deploy.mkdir(parents=True, exist_ok=True)
        for name in ("docker-compose.yml", "rpp.toml"):
            shutil.copy(cfg.deploy_src / name, cfg.staged_deploy / name)
        if cfg.e2e_stage:
            shutil.copy(
                cfg.deploy_src / "docker-compose.e2e.yml",
                cfg.staged_deploy / "docker-compose.e2e.yml",
            )
        self._pin_build_context()

        template = (
            cfg.deploy_src
            / "state"
            / "nucleons"
            / "nuc-tenant-demo"
            / "oidc-identity.toml.example"
        )
        if not template.is_file():
            raise AssertionError(f"the tracked binding template is missing at {template}")
        target = cfg.staged_deploy / "state" / "nucleons" / f"nuc-{cfg.noyau}" / "oidc-identity.toml"
        rendered = materialise_binding(cfg, template, target)
        left = unsubstituted_placeholders(rendered)
        if left:
            raise AssertionError(
                f"unsubstituted placeholder(s) left by the template: {' '.join(left)}"
            )
        for pattern, why in binding_required_patterns(cfg):
            if not re.search(pattern, rendered, re.M):
                raise AssertionError(
                    f"the binding template does not yield a loadable binding: "
                    f"nothing matches /{pattern}/ in {template.name} — {why}"
                )
        # Asserted against the tracked template FIRST (above), then
        # extended in the copy: the `tackle` leg needs the third grant.
        self.grant_worker_spawn(target)

        # The OAuth client registry the adapter publishes at
        # /.well-known/cosmon-oauth-clients: what `login` reads to learn
        # its own client_id. Copied into the state volume after `up` (the
        # volume does not exist before then).
        (cfg.run_dir / "oauth-clients.toml").write_text(
            "\n".join(
                [
                    "schema_version = 2",
                    f'issuer = "{cfg.issuer}"',
                    "",
                    "[[clients]]",
                    f'audience = "{cfg.audience}"',
                    f'client_id = "{cfg.audience}"',
                    'scopes = ["openid", "cosmon:molecule:read", '
                    '"cosmon:molecule:write", "cosmon:worker:spawn"]',
                    "",
                ]
            ),
            encoding="utf-8",
        )
        # The adapter Dockerfile COPYs the dist-binaries directory, which
        # is gitignored and absent on a clean checkout. An empty one is a
        # valid (and honest) input: the /dist route 404s with its own hint.
        # cfg.build_root, not repo_root: the images are built from that
        # tree, and creating the directory here would satisfy the COPY in
        # the wrong one.
        (cfg.build_root / "crates" / "cosmon-rpp-adapter" / "assets" / "binaries").mkdir(
            parents=True, exist_ok=True
        )
        self.stage_galaxy()

    def _pin_build_context(self) -> None:
        """Rewrite ``context: ../../..`` to an absolute path in the copies.

        The tracked files say ``context: ../../..``, which is right where
        they live and wrong everywhere else — it resolved correctly only
        because the run dir defaulted to exactly two levels under the
        repo, a single ``RPP_E2E_RUN_DIR`` away from silently building
        the wrong tree. Rewriting it here makes the source tree an
        explicit input, which is also what lets a falsification run build
        the images from a pre-U6 checkout while using this checkout's
        compose files.
        """
        cfg = self.cfg
        for path in cfg.compose_files:
            if not path.is_file():
                continue
            text = path.read_text(encoding="utf-8")
            path.write_text(
                text.replace("context: ../../..", f"context: {cfg.build_root}"),
                encoding="utf-8",
            )
        pinned = cfg.compose_file.read_text(encoding="utf-8")
        if f"context: {cfg.build_root}" not in pinned:
            raise AssertionError(
                f"could not pin the build context to {cfg.build_root} in "
                f"{cfg.compose_file}; the tracked compose file no longer says "
                "`context: ../../..`"
            )

    @staticmethod
    def grant_worker_spawn(binding: Path) -> None:
        """Add ``cosmon:worker:spawn`` to a materialised binding's scopes.

        A THIRD grant, not a synonym for the write scope: ``tackle``
        requires the pair by composition (AND), so a tenant holding only
        ``:write`` cannot burn the operator's model budget by dispatching
        workers. The tracked template deliberately does NOT hand it out —
        an operator who copied it without trimming would grant dispatch
        to every tenant — so the suite performs that grant itself, on its
        own throwaway binding. That IS the operator gesture the ``tackle``
        leg depends on; its absence is a 403 that looks nothing like a
        scope problem from the client side.

        The pre-state is asserted first: a template whose ``allowed`` line
        this does not recognise must fail here rather than silently keep
        the two-scope list and fail four steps later as a forbidden
        ``tackle``.
        """
        least_privilege = 'allowed = ["cosmon:molecule:read", "cosmon:molecule:write"]'
        text = binding.read_text(encoding="utf-8")
        if least_privilege not in text.splitlines() and least_privilege not in text:
            raise AssertionError(
                "the template's [scopes] allowed line is not the least-privilege pair this "
                "suite knows how to extend; cannot add cosmon:worker:spawn"
            )
        text = text.replace(
            least_privilege,
            'allowed = ["cosmon:molecule:read", "cosmon:molecule:write", '
            '"cosmon:worker:spawn"]',
        )
        binding.write_text(text, encoding="utf-8")
        if "cosmon:worker:spawn" not in binding.read_text(encoding="utf-8"):
            raise AssertionError(f"the operator grant cosmon:worker:spawn did not land in {binding}")

    def stage_galaxy(self) -> None:
        """(Re)create the throwaway tenant tree the adapter writes into.

        Called on every reinit as well as on staging: the molecules a set
        nucleates land here, on a host bind-mount that ``down -v`` does
        not reach.
        """
        cfg = self.cfg
        if cfg.galaxies_root.exists():
            shutil.rmtree(cfg.galaxies_root)
        (cfg.galaxy / ".cosmon" / "state").mkdir(parents=True, exist_ok=True)
        (cfg.galaxy / ".cosmon" / "formulas").mkdir(parents=True, exist_ok=True)
        shutil.copy(
            cfg.repo_root / ".cosmon" / "formulas" / "task-work.formula.toml",
            cfg.galaxy / ".cosmon" / "formulas" / "task-work.formula.toml",
        )
        # Arm the harvest door — BOTH halves of ADR-172 §D1.
        #
        # `harvest_door::decide` fails closed on a galaxy that has not
        # armed `[harvest_authority] required`: it refuses
        # `not_authorized` before it has even loaded the molecule. Arming
        # it is the operator gesture the tenant cannot make, which is the
        # point of the second key — so it is done here, on the host, in a
        # galaxy that lives for one test set.
        #
        # Arming alone is not enough, and the difference is the whole
        # reason this galaxy is provisioned rather than merely created.
        # Once armed, the *effect* half demands an operator-sealed grant,
        # verified inside the trunk lock; a galaxy with the switch on and
        # no trust root refuses `not_authorized` a second time, from the
        # other half. That is the shape a stock deployment must never be
        # left in, so `seal_harvest_grant` pins a trust root and one
        # molecule-scoped grant per molecule under test — see its
        # docstring for why the signer is a `publish = false` binary and
        # not a shipped verb.
        #
        # `[project] project_id` is not decoration: the grant seals the
        # galaxy identity and the transaction re-derives it inside the
        # lock, so a galaxy with no id cannot be the galaxy a grant names.
        (cfg.galaxy / ".cosmon" / "config.toml").write_text(
            "# Throwaway e2e galaxy. Arms the ADR-176 harvest door and names\n"
            "# itself, so the ADR-172 grant this run seals can be verified\n"
            "# against a galaxy identity re-derived inside the trunk lock.\n"
            "[project]\n"
            f'project_id = "{cfg.galaxy_id}"\n'
            "\n"
            "[harvest_authority]\n"
            "required = true\n",
            encoding="utf-8",
        )
        # The tenant root must be a git repository: the library tackle
        # executor resolves the repo root from it and cuts the worker's
        # worktree with `git worktree add`, and the harvest transaction
        # merges that worktree's branch back into the base branch here.
        #
        # `-b main` rather than whatever this machine's `init.defaultBranch`
        # happens to be: the sealed grant names the base branch it covers,
        # and a grant signed for `main` against a repository whose trunk is
        # `master` is refused for a reason that reads like a signature
        # failure. The branch name is a fact of the fixture, so it is
        # pinned by the fixture.
        init = _run(["git", "init", "-q", "-b", cfg.base_branch, str(cfg.galaxy)])
        if init.returncode != 0:
            raise AssertionError(
                f"git init of the throwaway galaxy failed: {init.stderr.strip()[:400]}"
            )
        # An identity, in the REPOSITORY's own config rather than the
        # user's: the worker commits inside the container as uid 10000,
        # whose `$HOME` holds no identity at all, and `git commit` with no
        # `user.email` fails with a message about `git config` that says
        # nothing about a harvest. Signing is turned off for the same
        # reason — the image ships no key, and an inherited
        # `commit.gpgsign = true` would abort the merge commit.
        for key, value in (
            ("user.email", "e2e-operator@example.invalid"),
            ("user.name", "Cosmon E2E Operator"),
            ("commit.gpgsign", "false"),
        ):
            cfgset = _run(["git", "-C", str(cfg.galaxy), "config", key, value])
            if cfgset.returncode != 0:
                raise AssertionError(
                    f"could not set {key} on the throwaway galaxy: "
                    f"{cfgset.stderr.strip()[:200]}"
                )
        # `.cosmon/` and `.worktrees/` are runtime state, not content. A
        # merge that swept them onto the base branch would commit the
        # molecule store into the tree whose history the harvest is
        # writing, and the next `cs done` would read its own commits.
        (cfg.galaxy / ".gitignore").write_text(
            ".cosmon/\n.worktrees/\n", encoding="utf-8"
        )
        (cfg.galaxy / "README.md").write_text(
            "Throwaway tenant galaxy for the cosmon RPP container e2e.\n",
            encoding="utf-8",
        )
        # A base commit, so `<base>..<branch>` is a range and not an
        # error. `cs done` can bootstrap a commit-less repository, but the
        # merge assertion needs a base revision recorded BEFORE the
        # harvest to compare against, and there is none until something
        # is committed.
        add = _run(["git", "-C", str(cfg.galaxy), "add", ".gitignore", "README.md"])
        commit = _run(["git", "-C", str(cfg.galaxy), "commit", "-q", "-m", "base"])
        if add.returncode != 0 or commit.returncode != 0:
            raise AssertionError(
                "could not write the throwaway galaxy's base commit: "
                f"{(add.stderr + commit.stderr).strip()[:400]}"
            )
        # The adapter runs as uid 10000; on a Linux runner the
        # bind-mounted tree is owned by the runner's uid and nucleate
        # needs to write into it. The worker's worktree and its branch
        # are cut inside this tree too, so the permission has to survive
        # the `.git` directory `git init` just made.
        for path in [cfg.galaxies_root, *cfg.galaxies_root.rglob("*")]:
            os.chmod(path, 0o777)

    def seal_harvest_grant(self, sealer: Path, molecule: str) -> Path:
        """Pin the ADR-172 trust root and seal one grant for ``molecule``.

        The operator gesture the tenant cannot make, and the one the
        container cannot make either: cosmon verifies operator signatures
        and ships no code that produces one, so the signer is
        ``cs-e2e-harvest-seal`` — a binary of the ``publish = false``
        ``cosmon-minisign-testkit`` crate, which appears only in
        ``[dev-dependencies]`` and is therefore in no shipped closure.
        It writes the same two artefacts an operator would place by hand:
        ``.cosmon/harvest.pub`` and one molecule-scoped ratified grant.

        Provisioning it here rather than weakening the assertion is the
        point. The alternative — assert that a stock stack *refuses* —
        was true and is no longer: since issue #67 the effect half is a
        library the adapter links, so a galaxy that is armed **and**
        sealed merges. What the container adds over
        ``v1_done_library_effect.rs``, which proves the same thing
        in-process, is that it merges through the image an operator
        deploys, on a bind-mounted tenant tree, driven by the real
        ``cosmon-remote``.

        Written on the HOST side of the bind-mount, so the grant is in
        place before the request; the adapter reads it from inside the
        container by the same path.
        """
        proc = _run(
            [
                str(sealer),
                str(self.cfg.galaxy),
                self.cfg.galaxy_id,
                molecule,
                self.cfg.base_branch,
            ]
        )
        if proc.returncode != 0:
            raise AssertionError(
                f"could not seal a harvest grant for {molecule}: "
                f"{proc.stderr.strip()[:800]}"
            )
        grant = (
            self.cfg.galaxy
            / ".cosmon"
            / "state"
            / "harvest"
            / "grants"
            / f"{molecule}.json"
        )
        pubkey = self.cfg.galaxy / ".cosmon" / "harvest.pub"
        for path in (grant, pubkey):
            if not path.is_file():
                raise AssertionError(
                    f"the sealer exited 0 but {path} does not exist"
                )
            os.chmod(path, 0o666)
        os.chmod(grant.parent, 0o777)
        os.chmod(grant.parent.parent, 0o777)
        return grant

    # -- compose ------------------------------------------------------

    def _compose_env(self) -> Dict[str, str]:
        cfg = self.cfg
        env = dict(os.environ)
        env.update(
            {
                "COSMON_GALAXIES_HOST": str(cfg.galaxies_root),
                "COSMON_RPP_AUDIENCE": cfg.audience,
                "COSMON_RPP_ISSUER": cfg.issuer,
                "COSMON_RPP_HOST_PORT": str(cfg.rpp_port),
                "COSMON_OIDC_HOST_PORT": str(cfg.oidc_port),
                "COSMON_RPP_NAME_SUFFIX": f"-e2e-{os.getpid()}",
            }
        )
        return env

    def _compose_file_args(self) -> list:
        """The ``-f`` flags, built from :attr:`E2EConfig.compose_files`."""
        args = []
        for path in self.cfg.compose_files:
            args += ["-f", str(path)]
        return args

    def compose(self, *args: str, check: bool = True, timeout: int = 3600) -> subprocess.CompletedProcess:
        """Run ``docker compose`` against the staged file, recorded."""
        cmd = [
            "docker", "compose",
            "-p", self.cfg.project,
            *self._compose_file_args(),
            *args,
        ]
        started = time.time()
        proc = _run(cmd, env=self._compose_env(), timeout=timeout)
        self.recorder.exchange(
            step="compose " + args[0] if args else "compose",
            request=" ".join(cmd),
            response=proc.stdout,
            rc=proc.returncode,
            started=started,
            stderr=proc.stderr,
        )
        with (self.logs_dir / "compose.log").open("a", encoding="utf-8") as fh:
            fh.write(f"$ {' '.join(cmd)}\n{proc.stdout}\n{proc.stderr}\n")
        if check and proc.returncode != 0:
            raise AssertionError(
                f"`docker compose {' '.join(args)}` failed (rc={proc.returncode})\n"
                f"stdout: {proc.stdout[-2000:]}\nstderr: {proc.stderr[-2000:]}"
            )
        return proc

    def build(self) -> None:
        """Build both images. Session-scoped: this is the expensive part."""
        self.compose("build")

    def up(self) -> None:
        """``up -d --wait`` — both declared healthchecks must pass.

        ``--wait`` is the whole point: the compose file declares the two
        probes and ``depends_on: service_healthy``, so a stack that
        answers here has passed its own liveness contract before any
        scenario starts.
        """
        self.compose("up", "-d", "--wait")

    def down(self) -> None:
        """``down -v`` — containers and named volumes, not just containers.

        Without ``-v`` the state volume survives and the next set starts
        on the previous set's inbox, rate-limiter buckets and JWKS.
        """
        self.compose("down", "-v", "--remove-orphans", check=False)

    def provision(self) -> None:
        """Publish the OAuth client registry into the live state volume.

        The operator does this out of band in a real deployment: it is
        the ``client_id`` publication, not a request-time concern. It has
        to happen after ``up`` because the volume does not exist before.
        """
        cid = self.compose("ps", "-q", "rpp-adapter").stdout.strip().splitlines()
        if not cid or not cid[0]:
            raise AssertionError("could not resolve the rpp-adapter container id")
        proc = _run(
            [
                "docker", "cp",
                str(self.cfg.run_dir / "oauth-clients.toml"),
                f"{cid[0]}:/cosmon/.cosmon/state/security/oauth-clients.toml",
            ]
        )
        if proc.returncode != 0:
            raise AssertionError(
                f"docker cp of oauth-clients.toml into the state volume failed: {proc.stderr}"
            )

    def reinit(self) -> None:
        """Tear the stack down to nothing and bring it back provisioned.

        This is the per-test-set boundary. Everything a set could have
        written is destroyed: the named volumes (``down -v``), the
        containers, and the bind-mounted tenant galaxy tree.
        """
        self.down()
        self.stage_galaxy()
        self.up()
        self.provision()
        self.generation += 1

    # -- observation --------------------------------------------------

    def adapter_log_tail(self, lines: int = 40) -> str:
        """The adapter's own log, for a failure report."""
        proc = _run(
            [
                "docker", "compose",
                "-p", self.cfg.project,
                *self._compose_file_args(),
                "logs", "--tail", str(lines), "rpp-adapter",
            ],
            env=self._compose_env(),
        )
        return proc.stdout or proc.stderr

    def capture_worker_pane(self, session: str) -> str:
        """``tmux capture-pane`` inside the adapter container.

        The pane is the diagnosis when a spawned worker does not finish,
        and it dies with the stack — so it is captured while it still
        exists, never after teardown.
        """
        proc = self.compose(
            "exec", "-T", "rpp-adapter", "tmux", "capture-pane", "-p", "-t", session,
            check=False,
        )
        return proc.stdout or proc.stderr

    def get_json(self, path: str, step: Optional[str] = None) -> Tuple[int, Any, str]:
        """GET a path on the adapter, recorded. Returns (status, json|None, body)."""
        url = self.cfg.host_url + path
        started = time.time()
        status, body = _http_get(url)
        parsed: Any = None
        try:
            parsed = json.loads(body)
        except ValueError:
            parsed = None
        self.recorder.exchange(
            step=step or f"GET {path}",
            request=f"GET {url}",
            response=f"HTTP {status}\n{body}",
            rc=0 if 200 <= status < 300 else 1,
            started=started,
        )
        return status, parsed, body


def _http_get(url: str, timeout: int = 15) -> Tuple[int, str]:
    req = urllib.request.Request(url, method="GET")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:  # noqa: S310 (loopback only)
            return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read().decode("utf-8", "replace")
    except urllib.error.URLError as exc:
        return 0, f"<no response: {exc.reason}>"
