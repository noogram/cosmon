# SPDX-License-Identifier: AGPL-3.0-only
"""The per-test-set boundary, asserted rather than assumed.

The reviewer's third point: *"Server (docker-compose) should be
reinitialized and populated before each test set to avoid left over
making test less reproducible."* The `stack` fixture does that — `down
-v`, `up --wait`, provision, and a fresh tenant galaxy tree, once per
class. This file is what makes that a claim the suite supports instead
of one it merely asserts.

The shape is a falsifier, not a smoke test. The first set plants a
leftover through the real product surface (it nucleates a molecule); the
second set proves the same molecule is gone. Run with
`RPP_E2E_REINIT=0` and the second set inherits the first set's stack,
the molecule is still there, and this file goes RED. A boundary that
cannot be observed failing is not a boundary anyone can rely on.
"""
from __future__ import annotations

from typing import Optional

import pytest

pytestmark = pytest.mark.stack

#: What set one planted, read by set two. Module state is the honest
#: representation here: the two sets are deliberately not independent —
#: the claim under test is precisely about what crosses between them.
PLANTED: Optional[str] = None


class TestSetOnePlantsALeftover:
    """First set: write state a naive next set would inherit."""

    def test_a_molecule_is_nucleated_and_present(self, molecule, logged_in, cfg, expect):
        """Plant the leftover, and prove it is really there before moving on.

        A "leftover" nobody verified was created would make the next
        set's absence assertion vacuous — it would pass on a stack that
        never received anything.
        """
        global PLANTED
        rc, payload, stderr = logged_in.observe(molecule)
        expect.equals(rc, 0, "the molecule this set just nucleated must read back within the set")
        expect.equals(
            (payload or {}).get("molecule", {}).get("id"),
            molecule,
            "the plant is only a valid leftover if the server really holds it",
        )
        expect.truthy(
            (cfg.galaxy / ".cosmon" / "state" / "fleets" / "default" / "molecules" / molecule).is_dir(),
            "the leftover must exist on both sides of the bind-mount: the named volume is "
            "what `down -v` destroys, the host tree is what the fixture recreates, and a "
            "reinit that missed either one would leave state behind",
        )
        PLANTED = molecule


class TestSetTwoStartsClean:
    """Second set: a new class, hence a new stack. Nothing may survive."""

    def test_the_previous_sets_molecule_is_gone(self, logged_in, stack, cfg, expect):
        """The planted molecule is absent — server-side and on disk.

        RED with `RPP_E2E_REINIT=0` (the stack is reused and the molecule
        is still served), GREEN with the reinit the `stack` fixture
        performs by default. That difference is the whole evidence for
        the reinit claim.
        """
        if PLANTED is None:
            pytest.fail(
                "nothing was planted: this test reads what TestSetOnePlantsALeftover wrote, so "
                "it is only meaningful for the whole module (`pytest tests/e2e/test_reinit.py`)"
            )
        expect.truthy(
            stack.generation >= 2 or not cfg.reinit,
            f"two test sets ran, so the stack should have been reinitialised twice "
            f"(generation={stack.generation}); with RPP_E2E_REINIT=0 it is deliberately not, "
            "and this test is then expected to go red below",
        )
        rc, payload, stderr = logged_in.observe(PLANTED)
        expect.truthy(
            rc != 0,
            f"molecule {PLANTED} was nucleated by the PREVIOUS test set; after `down -v` + "
            "`up` the state volume and the tenant galaxy tree are new, so observing it must "
            "fail. A zero exit here means the set inherited its predecessor's state",
        )
        expect.truthy(
            not (cfg.galaxy / ".cosmon" / "state" / "fleets" / "default" / "molecules" / PLANTED).is_dir(),
            "the host-side tenant tree is a bind-mount: `down -v` does not reach it, so the "
            "fixture recreates it explicitly. This asserts that half of the boundary",
        )

    def test_this_set_can_nucleate_its_own_molecule(self, molecule, logged_in, expect):
        """A clean set is a *working* set, not merely an empty one.

        A reinit that left the stack unable to serve would also make the
        absence assertion above pass, for the wrong reason.
        """
        rc, payload, stderr = logged_in.observe(molecule)
        expect.equals(rc, 0, "the fresh set's own molecule reads back — the stack is live, not just empty")
        expect.truthy(
            molecule != PLANTED,
            "a fresh nucleation must mint a new id; reusing the planted one would mean the "
            "previous set's tree is still underneath",
        )
