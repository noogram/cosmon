# SPDX-License-Identifier: AGPL-3.0-only
"""The nucleon binding template — the one artefact a fresh operator copies.

No container is needed for any of this, which is the point: the failure
these tests catch (a template that has silently lost a key the loader
reads) costs minutes to find at admission time and milliseconds here.

The template is `crates/cosmon-rpp-adapter/deploy/state/nucleons/
nuc-tenant-demo/oidc-identity.toml.example`. The real file beside it is
gitignored, as it must be: it is the operator-written pin that admits a
JWT `(iss, sub, aud)` triple to one noyau, and a tracked copy would
publish a live habilitation. So the template IS what a fresh operator
provisions from, and the suite must exercise exactly it — never an
inline fixture, which would pass on a private copy the operator will
never have.
"""
from __future__ import annotations

import re

import pytest

from harness.compose import (
    binding_required_patterns,
    materialise_binding,
    unsubstituted_placeholders,
)


@pytest.fixture
def template(cfg):
    """The tracked `.example`, exactly as a fresh checkout ships it."""
    path = (
        cfg.deploy_src / "state" / "nucleons" / "nuc-tenant-demo" / "oidc-identity.toml.example"
    )
    assert path.is_file(), (
        f"the tracked binding template is missing at {path} — without it a clean "
        "checkout cannot boot the stack at all"
    )
    return path


@pytest.fixture
def rendered(cfg, template, tmp_path):
    """The template with this run's values substituted in."""
    return materialise_binding(cfg, template, tmp_path / "oidc-identity.toml")


def test_no_placeholder_survives_substitution(rendered, expect):
    """Every `REPLACE_ME_*` token outside a comment must be substituted.

    An unsubstituted placeholder is not a cosmetic defect: the loader
    would pin the literal string `REPLACE_ME_ISSUER` as the issuer and
    then fail closed at admission, far from here and with no hint that a
    template renamed a token.
    """
    left = unsubstituted_placeholders(rendered)
    expect.equals(
        left,
        (),
        "the harness substitutes exactly the placeholder names the template declares; "
        "a leftover means the template renamed or added one "
        "(deploy/state/nucleons/nuc-tenant-demo/oidc-identity.toml.example header)",
    )


def test_placeholder_scan_ignores_the_header_comment(cfg, template, tmp_path):
    """The scan must not read the template's own instructions as a defect.

    The header names the placeholder token to tell the operator what to
    replace. A scan that could not tell that sentence from an
    unsubstituted value would make the header unwritable — so the scan
    skips comment lines, and this pins that it still does.
    """
    text = template.read_text(encoding="utf-8")
    assert "REPLACE_ME_" in text.split("\n[oidc]")[0], (
        "the template header no longer names a placeholder; if that is deliberate, "
        "this test is the record of why the scan skips comments"
    )
    rendered = materialise_binding(cfg, template, tmp_path / "b.toml")
    assert unsubstituted_placeholders(rendered) == ()


def test_template_yields_every_key_the_loader_reads(cfg, rendered, expect):
    """`HabilitationMap::load` reads each of these; a missing one fails closed.

    Removing a key does not yield a "partial" binding — it yields a file
    the loader cannot resolve a noyau from. The template says so in its
    own header; this test is what keeps that sentence true.
    """
    for pattern, why in binding_required_patterns(cfg):
        found = re.search(pattern, rendered, re.M) is not None
        expect.truthy(found, f"/{pattern}/ — {why}")


def test_the_key_check_can_actually_go_red(cfg, rendered):
    """A falsifier for the test above: drop a key, the check must notice.

    A check that cannot fail is not evidence. This deletes the `noyau`
    line from an otherwise valid rendering and asserts the same scan
    rejects it.
    """
    mutilated = "\n".join(
        line for line in rendered.splitlines() if not line.startswith("noyau = ")
    )
    missed = [
        pattern
        for pattern, _why in binding_required_patterns(cfg)
        if re.search(pattern, mutilated, re.M) is None
    ]
    assert missed, "removing `noyau` left every required pattern matching — the scan is vacuous"
    assert any("noyau" in pattern for pattern in missed), (
        f"the scan noticed something, but not the missing key: {missed}"
    )
