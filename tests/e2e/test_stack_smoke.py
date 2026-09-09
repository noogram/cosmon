# SPDX-License-Identifier: AGPL-3.0-only
"""The stack boots and publishes what a login needs before any login runs.

Every other test of this surface runs the adapter in-process against a
tower of test doubles. That proves the handlers; it cannot prove that
the *image* boots, that the JWKS hand-off between the two containers
lands where the adapter looks for it, or that the operator's
out-of-band provisioning is readable over the wire. Those are the
failures this file catches, and they are exactly the ones a fresh
operator meets first.
"""
from __future__ import annotations

import pytest

pytestmark = pytest.mark.stack


class TestStackBoots:
    """One test set: a freshly initialised stack, nothing driven yet."""

    def test_healthz_answers_through_the_published_port(self, stack, expect):
        """`GET /healthz` → `{"ok":true}` on the host-published port.

        The route is excluded from the §8p frozen API surface (it is
        operational, not user-facing), but its body shape is what the
        compose healthcheck greps for — `wget -qO- … | grep -q '"ok":true'`
        in docker-compose.yml. A body that stopped saying `ok:true` would
        make `up --wait` hang rather than fail, so it is pinned here
        where the failure is legible.
        """
        status, payload, body = stack.get_json("/healthz", step="healthz")
        expect.equals(status, 200, "the adapter publishes /healthz unauthenticated (routes/mod.rs)")
        expect.equals(
            (payload or {}).get("ok"),
            True,
            'docker-compose.yml healthchecks the adapter with grep -q \'"ok":true\'; '
            "the field name and the literal true are the contract, not a detail",
        )

    def test_the_oauth_client_registry_is_served(self, stack, cfg, expect):
        """`/.well-known/cosmon-oauth-clients` is what `login` reads first.

        Reverse discovery: the tenant knows the adapter's URL and nothing
        else, and learns its own `client_id` and the issuer to talk to
        from this document. The operator publishes it out of band (it is
        a `client_id` publication, not a request-time concern) — here,
        the `stack` fixture's provisioning step. If the issuer served
        here disagrees with the one the IdP mints, every later login
        fails with a signature error that names nothing.
        """
        status, doc, body = stack.get_json(
            "/.well-known/cosmon-oauth-clients", step="oauth-clients discovery"
        )
        expect.equals(status, 200, "the registry is published unauthenticated by design")
        expect.equals(
            (doc or {}).get("issuer"),
            cfg.issuer,
            "wiring contract A: the registry's issuer must be byte-for-byte the IdP's, "
            "because the adapter pins JWKS by `iss`",
        )
        clients = (doc or {}).get("clients") or []
        audiences = [c.get("audience") for c in clients]
        expect.contains(
            ",".join(a for a in audiences if a),
            cfg.audience,
            f"the run provisions the audience {cfg.audience!r} into the IdP, the nucleon "
            "binding and this registry at once; a registry that lost it cannot serve a login",
        )
