# SPDX-License-Identifier: AGPL-3.0-only
"""The SHIPPED stage, inspected: it carries neither `cs` nor an agent CLI.

The other half of the `tackle` claim, and the one the dispatch tests
cannot make on their own. The image they run against is
`cs-rpp-adapter:e2e`, which carries a dummy agent and a worker-side `cs`
on purpose. That is only honest if the stage a release ships carries
NEITHER — otherwise the suite would be proving "an image with `cs` in it
can tackle", which is what the whole issue is about not doing.

So the `runtime` target is built (every layer is already cached from the
e2e build, which is a `FROM runtime`) and looked inside: `command -v` on
the two names, and finding either is the failure.
"""
from __future__ import annotations

import os
import subprocess

import pytest

pytestmark = pytest.mark.stack


class TestShippedImage:
    """One test set. It needs the built images, not a running stack."""

    def test_runtime_stage_carries_no_cs_and_no_agent(self, compose_stack, cfg, expect):
        """`command -v cs; command -v claude` must print nothing.

        Deliberately on `compose_stack` (session-scoped, the built
        images) rather than `stack`: this asks what is INSIDE an image,
        not what a running container answers, so it needs no reinit and
        must not pay for one.

        Skipped-by-construction is not an option here either. When
        `RPP_E2E_E2E_STAGE=0` there is no doctored image to distinguish
        the shipped one from, and the run is not making the claim — the
        assertion below still runs against whatever `runtime` builds, and
        the tag is torn down either way.
        """
        tag = f"cs-rpp-adapter:shipped-probe-{os.getpid()}"
        dockerfile = cfg.build_root / "crates" / "cosmon-rpp-adapter" / "Dockerfile"
        build = subprocess.run(
            ["docker", "build", "--target", "runtime", "-t", tag,
             "-f", str(dockerfile), str(cfg.build_root)],
            capture_output=True, text=True,
        )
        if build.returncode != 0:
            raise AssertionError(
                "could not build the shipped `runtime` target:\n" + build.stderr[-4000:]
            )
        try:
            probe = subprocess.run(
                ["docker", "run", "--rm", "--entrypoint", "sh", tag,
                 "-c", "command -v cs; command -v claude; true"],
                capture_output=True, text=True,
            )
            leaked = probe.stdout.strip()
        finally:
            subprocess.run(["docker", "image", "rm", "-f", tag],
                           capture_output=True, text=True)
        expect.equals(
            leaked,
            "",
            "the SHIPPED `runtime` stage must contain neither `cs` nor an agent CLI; "
            "anything printed here is the e2e stage leaking into the image an operator "
            f"deploys (found: {leaked!r})",
        )
