# SPDX-License-Identifier: AGPL-3.0-only
"""The tenant's CLI, driven as a tenant would drive it.

Every exchange goes through the real compiled ``cosmon-remote`` binary
over the published loopback ports, not through a curl transcript: the
claim this suite makes is that a tenant's CLI drives the stack, and only
the CLI can make it.

``$HOME`` is redirected for the CLI process — and *only* for it — so a
run reads neither the operator's ``cosmon-remote`` profiles nor their OS
keychain. Exporting the redirect for the whole session also moves
``docker``'s home, and ``docker compose`` is a CLI *plugin* resolved out
of ``$HOME/.docker/cli-plugins``: the teardown then fails with a usage
dump and leaves the stack up, which the next run meets as "port already
allocated". Scope the redirect to the process that asked for it.
"""
from __future__ import annotations

import json
import subprocess
import time
from pathlib import Path
from typing import Any, List, Optional, Tuple

from .config import E2EConfig
from .record import Recorder


class RemoteCli:
    """A `cosmon-remote` invocation surface bound to one tenant profile."""

    def __init__(self, cfg: E2EConfig, binary: Path, recorder: Recorder, slot: str = "default") -> None:
        self.cfg = cfg
        self.binary = binary
        self.recorder = recorder
        # One $HOME per test set. A credential minted before a reinit is
        # signed by a key the restarted IdP no longer has, so carrying
        # the previous set's profile over would make a fresh set fail for
        # a reason that has nothing to do with what it tests.
        self.home = cfg.run_dir / "home" / slot
        self.home.mkdir(parents=True, exist_ok=True)

    def _env(self) -> dict:
        return {
            "HOME": str(self.home),
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin",
            # The file backend keeps the credential inside the run dir;
            # the OS keychain is neither touched nor needed.
            "COSMON_REMOTE_CRED_BACKEND": "file",
            "COSMON_REMOTE_TOKEN": "",
            # Headless login: `login` opens the authorization URL with
            # this command instead of a browser (no shell; the URL is
            # appended as the last argument).
            "COSMON_REMOTE_BROWSER": "curl -sS -L -o /dev/null",
        }

    def run(self, *args: str, step: Optional[str] = None, timeout: int = 300) -> subprocess.CompletedProcess:
        """Invoke the CLI, record the exchange, return the process."""
        cmd = [str(self.binary), "--profile", "e2e", *args]
        started = time.time()
        proc = subprocess.run(cmd, capture_output=True, text=True, env=self._env(), timeout=timeout)
        self.recorder.exchange(
            step=step or " ".join(args[:2]) or "cosmon-remote",
            request="cosmon-remote " + " ".join(["--profile", "e2e", *args]),
            response=proc.stdout,
            rc=proc.returncode,
            started=started,
            stderr=proc.stderr,
        )
        return proc

    def json(self, *args: str, step: Optional[str] = None) -> Tuple[int, Any, str]:
        """Invoke with ``--json`` and parse. Returns (rc, parsed|None, stderr)."""
        proc = self.run("--json", *args, step=step)
        try:
            parsed = json.loads(proc.stdout)
        except ValueError:
            parsed = None
        return proc.returncode, parsed, proc.stderr

    # -- the scenario's own verbs -------------------------------------

    def configure(self) -> List[subprocess.CompletedProcess]:
        """Create the ``e2e`` profile the rest of the scenario uses."""
        cfg = self.cfg
        steps = [
            ("config init", ("config", "init", "e2e", cfg.host_url)),
            ("config sub", ("config", "set", "sub", cfg.idp_sub)),
            # The CLIENT's audience is a falsification seam of its own:
            # moving it away from the server-side pin must break login.
            ("config aud", ("config", "set", "aud", cfg.client_audience)),
            ("config oidc-url", ("config", "set", "oidc-url", cfg.issuer)),
            ("config noyau", ("config", "set", "noyau", cfg.noyau)),
        ]
        out = []
        for step, args in steps:
            proc = self.run(*args, step=step)
            if proc.returncode != 0:
                raise AssertionError(f"{step} failed: {proc.stderr.strip()[:400]}")
            out.append(proc)
        return out

    def login(self) -> subprocess.CompletedProcess:
        """The full authorization-code + PKCE flow against the mock IdP."""
        return self.run("login", step="login")

    def auth_me(self) -> Tuple[int, Any, str]:
        """``GET /v1/auth/me`` — the token as the SERVER sees it."""
        return self.json("auth", "me", step="auth me")

    def nucleate(self, formula: str, topic: str) -> Tuple[int, Any, str]:
        """``POST /v1/molecules`` — library-direct, writes the tenant tree."""
        return self.json("molecule", "nucleate", formula, "--topic", topic, step="nucleate")

    def observe(self, molecule_id: str) -> Tuple[int, Any, str]:
        """``GET /v1/molecules/:id``."""
        return self.json("molecule", "get", molecule_id, step="observe")

    def tackle(self, molecule_id: str) -> Tuple[int, Any, str]:
        """``POST /v1/molecules/:id/tackle`` — dispatch a real worker.

        Library-direct since issue #54 U6: the adapter resolves the
        molecule and its formula in-process, cuts a git worktree, writes
        the dispatch ledger entry BEFORE the spawn, opens a tmux session
        under the worker envelope's ``env -i`` and pastes the briefing
        into it — with no ``cs`` binary anywhere in its own image.
        """
        return self.json("molecule", "tackle", molecule_id, step="tackle")

    def done(self, molecule_id: str, reason: str) -> Tuple[int, Any, str]:
        """``POST /v1/molecules/:id/done`` — the harvest door.

        One gesture again since issue #51: the second `land` verb was
        withdrawn, and closing a molecule is the last step of its normal
        life rather than a separate administrative act.

        ``--reason`` is mandatory and is never fabricated — the door
        refuses `missing_reason` rather than inventing a sentence — so
        the caller must say why, here as at the terminal.
        """
        return self.json(
            "molecule", "done", molecule_id, "--reason", reason, step="done"
        )
