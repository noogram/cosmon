# SPDX-License-Identifier: AGPL-3.0-only
"""The debugging tooling itself, tested — no container required.

The review asked for two things a shell `[[ x == y ]]` cannot give: a
stated reason for every expectation, and enough output on failure to
tell a wrong expectation from a broken server. Both are properties of
:mod:`harness.expect`, so both are testable here, cheaply, and are.

A failure report that quietly stopped carrying the response body would
otherwise be discovered exactly when it is needed most — in the middle
of a red nightly at 3am.
"""
from __future__ import annotations

import time

import pytest

from harness.expect import Expect
from harness.record import Recorder


@pytest.fixture
def recorded(tmp_path):
    """A recorder holding one realistic exchange."""
    rec = Recorder.open(tmp_path / "artifacts")
    rec.exchange(
        step="observe",
        request="cosmon-remote --profile e2e --json molecule get task-20260101-abcd",
        response='{"molecule":{"id":"task-20260101-abcd","status":"queued"}}',
        rc=0,
        started=time.time(),
    )
    return rec


def test_a_failed_expectation_names_the_reason(recorded):
    """The `why` argument is reproduced verbatim in the report.

    It is the sentence that lets a reader decide whether the *test* is
    wrong, which is the judgement the reviewer could not make against
    the shell harness.
    """
    expect = Expect(recorded, log_tail=lambda: "adapter: 200 GET /v1/molecules/...")
    with pytest.raises(AssertionError) as excinfo:
        expect.equals("queued", "pending", "ADR-080: a molecule nucleated over the API is unassigned")
    report = str(excinfo.value)
    assert "ADR-080: a molecule nucleated over the API is unassigned" in report


def test_a_failed_expectation_quotes_request_response_and_log(recorded):
    """The three things a debugger needs, present without re-running anything."""
    expect = Expect(recorded, log_tail=lambda: "adapter: GET /v1/molecules/task-20260101-abcd 200")
    with pytest.raises(AssertionError) as excinfo:
        expect.equals("queued", "pending", "why-string")
    report = str(excinfo.value)
    assert "molecule get task-20260101-abcd" in report, "the request is missing from the report"
    assert '"status":"queued"' in report, "the response body is missing from the report"
    assert "adapter: GET /v1/molecules" in report, "the adapter log tail is missing from the report"
    assert str(recorded.artifacts) in report, "the report does not say where the full record is"


def test_a_satisfied_expectation_is_silent(recorded):
    """No report, no noise: the helper only speaks when something is wrong."""
    expect = Expect(recorded, log_tail=lambda: "")
    expect.equals("pending", "pending", "why-string")
    expect.truthy("task-20260101-abcd", "why-string")
    expect.contains('{"status":"queued"}', "queued", "why-string")


def test_the_report_survives_a_missing_log_tail(recorded):
    """Off-stack assertions still report; they just have no container log."""
    expect = Expect(recorded, log_tail=None)
    with pytest.raises(AssertionError) as excinfo:
        expect.truthy("", "why-string")
    assert "adapter log tail" not in str(excinfo.value)
    assert "why-string" in str(excinfo.value)


def test_a_long_response_is_truncated_not_dropped(tmp_path):
    """A 500's HTML body must not push the reason off the screen."""
    rec = Recorder.open(tmp_path / "artifacts")
    rec.exchange(step="observe", request="GET /x", response="y" * 10_000, rc=1, started=time.time())
    expect = Expect(rec, log_tail=None)
    with pytest.raises(AssertionError) as excinfo:
        expect.equals(1, 2, "why-string")
    report = str(excinfo.value)
    assert "truncated, 10000 bytes total" in report
    assert len(report) < 4000


def test_every_exchange_is_written_to_its_own_file(recorded):
    """The on-disk record is what a nightly reader has after the process is gone."""
    files = sorted(p.name for p in recorded.artifacts.glob("*.txt"))
    assert files == ["001-observe.txt"], files
    body = (recorded.artifacts / "001-observe.txt").read_text()
    assert "molecule get task-20260101-abcd" in body
    assert '"status":"queued"' in body
    ndjson = (recorded.artifacts / "e2e.ndjson").read_text().strip().splitlines()
    assert len(ndjson) == 1 and '"step": "observe"' in ndjson[0]
