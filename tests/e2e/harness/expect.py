# SPDX-License-Identifier: AGPL-3.0-only
"""The expectation helper: an assertion that explains itself when it breaks.

The review this suite answers made the point precisely: *"I don't see
explanation on why the request output is expected to have this value and
no tooling to debug."* Two separate defects, so two separate remedies,
both of them structural rather than a convention someone must remember.

The *why* is a required argument. :func:`Expect.equals` cannot be called
without one, so an assertion that does not say what it derives from does
not compile past review — the string is meant to name the ADR section or
route doc the value comes from, not to restate the comparison.

The *debugging* is automatic. On failure the report carries the last
exchange in full — the request as sent, the response as received — and
the tail of the adapter's own log for the same window. That is the three
things a reader needs to tell a wrong expectation from a broken server,
assembled without anybody having to re-run the scenario by hand.
"""
from __future__ import annotations

from typing import Any, Callable, Optional

from .record import Exchange, Recorder


class Expect:
    """Assertions bound to a live run, so failures can quote it.

    Instantiated by the ``expect`` fixture; tests receive it ready to
    use. It is a class rather than a module function because the log
    tail it needs is a property of the running stack, and threading that
    through every call site is exactly the noise the reviewer objected
    to.
    """

    def __init__(
        self,
        recorder: Recorder,
        log_tail: Optional[Callable[[], str]] = None,
    ) -> None:
        self._recorder = recorder
        self._log_tail = log_tail

    # -- assertions ---------------------------------------------------

    def equals(self, actual: Any, expected: Any, why: str, *, step: Optional[str] = None) -> None:
        """Assert equality, and say what the expected value derives from.

        :param why: why ``expected`` is the right value — a sentence
            naming the ADR section, route doc or invariant it comes
            from. Required: an expectation nobody can trace is the
            defect this argument exists to prevent.
        :param step: quote the last exchange of this step rather than
            the last exchange overall, when the assertion is made after
            some later traffic.
        """
        if actual == expected:
            return
        raise AssertionError(
            self._report(
                headline=f"expected {expected!r}, observed {actual!r}",
                why=why,
                step=step,
            )
        )

    def truthy(self, actual: Any, why: str, *, step: Optional[str] = None) -> None:
        """Assert a value is present/true, with the same `why` contract."""
        if actual:
            return
        raise AssertionError(
            self._report(
                headline=f"expected a truthy value, observed {actual!r}",
                why=why,
                step=step,
            )
        )

    def contains(self, haystack: str, needle: str, why: str, *, step: Optional[str] = None) -> None:
        """Assert a substring is present, with the same `why` contract."""
        if needle in haystack:
            return
        raise AssertionError(
            self._report(
                headline=f"expected to find {needle!r} in the observed value",
                why=why,
                step=step,
            )
        )

    def fail(self, headline: str, why: str, *, step: Optional[str] = None) -> None:
        """Fail outright with the same report shape (no comparison to make)."""
        raise AssertionError(self._report(headline=headline, why=why, step=step))

    # -- reporting ----------------------------------------------------

    def _report(self, *, headline: str, why: str, step: Optional[str]) -> str:
        parts = [headline, "", f"why this value: {why}", ""]
        ex: Optional[Exchange] = self._recorder.last(step)
        if ex is None:
            parts.append("last exchange: <none recorded>")
        else:
            parts.append("last exchange:")
            parts.append(ex.render())
        if self._log_tail is not None:
            tail = self._log_tail()
            parts.append("")
            parts.append("adapter log tail:")
            parts.append(tail if tail.strip() else "  <empty>")
        parts.append("")
        parts.append(f"full record: {self._recorder.artifacts}")
        return "\n".join(parts)
