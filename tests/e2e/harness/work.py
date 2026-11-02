# SPDX-License-Identifier: AGPL-3.0-only
"""Pieces the cross-machine ``work`` acceptance needs beyond the base harness.

Issue #147 W4 asks for one claim the in-process route tests cannot make: two
*distinct* clients, one per machine, exchange advisory work through the one
authoritative host, and every verdict is read from the host's own store.
Three things are provisioned here, each by a documented operator gesture and
never by a test-only hook in production code:

* :class:`ContainerCli` — a ``cosmon-remote`` that runs **inside its own
  container** on the compose network, with no bind mount. It speaks the same
  ``run``/``json`` surface as :class:`harness.remote.RemoteCli`, so a test
  does not know which of the two it is driving.
* :func:`mint_token` — a bearer for a chosen subject from the mock IdP's
  documented ``POST /issue`` side channel (the same one the harvest-only
  identity uses).
* :func:`raw_request` — one HTTP exchange with explicit headers, for the two
  facts the CLI hides: the HTTP status of a refusal, and a request to a route
  the CLI has no verb for.
"""
from __future__ import annotations

import json
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Dict, Optional, Tuple

from .config import E2EConfig
from .record import Recorder


def mint_token(cfg: E2EConfig, sub: str, scopes: str = "") -> str:
    """Ask the mock IdP for a bearer whose ``sub`` is ``sub``.

    The IdP only signs; what the token may do is decided by the adapter from
    the nucleon binding and the token's own scopes. Passing ``scopes=""``
    mints a token that carries none, which is how a token "without
    ``cosmon:work:write``" is made without editing any binding.
    """
    query = {"sub": sub, "aud": cfg.audience, "lifetime": "3600"}
    if scopes:
        query["scopes"] = scopes
    request = urllib.request.Request(
        cfg.issuer + "/issue?" + urllib.parse.urlencode(query), data=b"", method="POST"
    )
    with urllib.request.urlopen(request, timeout=15) as response:  # noqa: S310 (loopback only)
        return json.load(response)["access_token"]


def raw_request(
    cfg: E2EConfig,
    recorder: Recorder,
    method: str,
    path: str,
    *,
    token: str,
    attachment: Optional[str] = None,
    proof: Optional[str] = None,
    body: Optional[Any] = None,
    step: str,
) -> Tuple[int, Any]:
    """One recorded HTTP exchange against the published adapter port.

    The attachment proof is a secret: it travels in a header, as the real
    client sends it, and the recorder is handed a request line that names the
    route only.
    """
    headers: Dict[str, str] = {"Authorization": f"Bearer {token}"}
    if attachment:
        headers["cosmon-attachment"] = attachment
    if proof:
        headers["cosmon-attachment-proof"] = proof
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(cfg.host_url + path, data=data, headers=headers, method=method)
    started = time.time()
    try:
        with urllib.request.urlopen(request, timeout=30) as response:  # noqa: S310 (loopback only)
            status, text = response.status, response.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        status, text = exc.code, exc.read().decode("utf-8", "replace")
    try:
        parsed: Any = json.loads(text)
    except ValueError:
        parsed = None
    recorder.exchange(
        step=step,
        request=f"{method} {cfg.host_url}{path}",
        response=f"HTTP {status}\n{text}",
        rc=0 if 200 <= status < 300 else 1,
        started=started,
    )
    return status, parsed


class ContainerCli:
    """``cosmon-remote`` run in its own container, on the compose network.

    The container has **no bind mount and no published port**: its only
    contact with the host is the compose network, where it reaches the
    service as ``rpp-adapter:8443`` — the way a second machine reaches the
    authoritative host, minus the physical network. Its ``$HOME``, profile
    and credential file live in the container's own writable layer, so the
    first client's credential store and the host filesystem are both
    unreachable from it by construction.

    The binary is a Linux build of the same ``cosmon-remote`` crate (see
    ``tests/e2e/client.Dockerfile``); nothing here reimplements the client.
    """

    HOME = "/home/client"
    SERVICE_URL = "http://rpp-adapter:8443"

    def __init__(self, cfg: E2EConfig, recorder: Recorder, name: str, network: str,
                 image: str, sub: str) -> None:
        self.cfg = cfg
        self.recorder = recorder
        self.name = name
        self.sub = sub
        self.network = network
        self.image = image
        self.token: Optional[str] = None

    # -- lifecycle ----------------------------------------------------

    def start(self) -> None:
        """Start the idle container: no mounts, no ports, non-root."""
        subprocess.run(["docker", "rm", "-f", self.name], capture_output=True, text=True)
        proc = subprocess.run(
            [
                "docker", "run", "-d", "--name", self.name,
                "--network", self.network,
                "--label", f"cs-e2e-owner={self.cfg.project}",
                self.image, "sleep", "infinity",
            ],
            capture_output=True, text=True,
        )
        if proc.returncode != 0:
            raise AssertionError(f"could not start the collaborator container: {proc.stderr[-800:]}")
        inspect = subprocess.run(
            ["docker", "inspect", self.name, "--format",
             "{{json .Mounts}}|{{json .NetworkSettings.Ports}}"],
            capture_output=True, text=True,
        )
        mounts, ports = inspect.stdout.strip().split("|", 1)
        if mounts != "[]":
            raise AssertionError(f"the collaborator container must have no mounts, has {mounts}")
        self.isolation = {"mounts": json.loads(mounts), "published_ports": json.loads(ports)}

    def stop(self) -> None:
        """Remove the container; it owns nothing the host needs."""
        subprocess.run(["docker", "rm", "-f", self.name], capture_output=True, text=True)

    def inspect_filesystem(self) -> Dict[str, Any]:
        """Facts that bear on isolation, read from inside the container."""
        def sh(script: str) -> str:
            return subprocess.run(
                ["docker", "exec", self.name, "sh", "-c", script], capture_output=True, text=True
            ).stdout.strip()

        return {
            "uid": sh("id -u"),
            "mountinfo_bind_lines": sh(
                "grep -vE ' (proc|sysfs|tmpfs|devpts|mqueue|cgroup2?|devtmpfs) ' /proc/self/mountinfo "
                "| awk '{print $5}' | sort -u | tr '\\n' ' '"
            ),
            "host_galaxy_visible": sh("ls -d /Users /cosmon 2>/dev/null | tr '\\n' ' '"),
        }

    # -- the RemoteCli surface a test uses ----------------------------

    def run(self, *args: str, step: Optional[str] = None, timeout: int = 300,
            token_override: Optional[str] = None,
            stdin: Optional[str] = None) -> subprocess.CompletedProcess:
        """``docker exec`` one CLI invocation, record it, return the process."""
        token = token_override if token_override is not None else self.token
        cmd = [
            "docker", "exec", "-i",
            "-e", f"HOME={self.HOME}",
            "-e", "COSMON_REMOTE_CRED_BACKEND=file",
            # The bearer goes by `--token`, not the env var: the env var selects
            # the static-bearer backend, which cannot hold an attachment proof.
            "-e", "COSMON_REMOTE_TOKEN=",
            self.name, "cosmon-remote", "--profile", "e2e",
            *(["--token", token] if token else []), *args,
        ]
        started = time.time()
        # `-i` without input would inherit the runner's stdin and could block.
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout,
                              input=stdin if stdin is not None else "")
        self.recorder.exchange(
            step=step or " ".join(args[:2]) or "cosmon-remote",
            request=f"[{self.name}] cosmon-remote --profile e2e " + " ".join(args),
            response=proc.stdout,
            rc=proc.returncode,
            started=started,
            stderr=proc.stderr,
        )
        return proc

    def json(self, *args: str, step: Optional[str] = None,
             token_override: Optional[str] = None,
             stdin: Optional[str] = None) -> Tuple[int, Any, str]:
        """Invoke with ``--json`` and parse. Returns (rc, parsed|None, stderr)."""
        proc = self.run("--json", *args, step=step, token_override=token_override, stdin=stdin)
        try:
            parsed = json.loads(proc.stdout)
        except ValueError:
            parsed = None
        return proc.returncode, parsed, proc.stderr

    def configure(self) -> None:
        """Create the ``e2e`` profile, pointed at the service name.

        ``issuer`` and ``client-id`` are set by hand because this client does
        not run the interactive login (the IdP's published issuer is a host
        loopback address the container cannot dial). They only address the
        credential file the attachment proof is stored in; the bearer is the
        one minted for ``sub``.
        """
        cfg = self.cfg
        for step, args in (
            ("config init", ("config", "init", "e2e", self.SERVICE_URL)),
            ("config sub", ("config", "set", "sub", self.sub)),
            ("config aud", ("config", "set", "aud", cfg.audience)),
            ("config oidc-url", ("config", "set", "oidc-url", cfg.issuer)),
            ("config issuer", ("config", "set", "issuer", cfg.issuer)),
            ("config client-id", ("config", "set", "client-id", cfg.audience)),
            ("config noyau", ("config", "set", "noyau", cfg.noyau)),
        ):
            proc = self.run(*args, step=step, token_override="")
            if proc.returncode != 0:
                raise AssertionError(f"{step} failed in the collaborator container: {proc.stderr.strip()[:400]}")


def build_client_image(cfg: E2EConfig, recorder: Recorder, tag: str) -> None:
    """Build the Linux ``cosmon-remote`` client image (cached after the first).

    Two steps: the adapter Dockerfile's ``builder`` stage is tagged (a layer
    cache hit when the adapter image was built from the same tree), then
    ``client.Dockerfile`` compiles ``cosmon-remote`` on top of it.
    """
    adapter_dockerfile = cfg.build_root / "crates" / "cosmon-rpp-adapter" / "Dockerfile"
    client_dockerfile = cfg.repo_root / "tests" / "e2e" / "client.Dockerfile"
    builder_tag = tag + "-builder"
    for step, cmd in (
        ("tag builder stage", ["docker", "build", "--target", "builder", "-f", str(adapter_dockerfile),
                               "-t", builder_tag, str(cfg.build_root)]),
        ("build client image", ["docker", "build", "-f", str(client_dockerfile),
                                "--build-arg", f"BUILDER={builder_tag}", "-t", tag, str(cfg.build_root)]),
    ):
        started = time.time()
        proc = subprocess.run(cmd, capture_output=True, text=True)
        recorder.exchange(
            step=step, request=" ".join(cmd[:4]) + " ...",
            response=proc.stdout[-2000:], rc=proc.returncode, started=started,
            stderr=proc.stderr[-2000:],
        )
        if proc.returncode != 0:
            raise AssertionError(f"{step} failed:\n{proc.stderr[-4000:]}")
