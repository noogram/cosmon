# SPDX-License-Identifier: AGPL-3.0-only
"""Harness for the container-level end-to-end suite of the §8j Remote Pilot Port.

The modules here are the parts a test should never have to spell out
itself: how the compose stack is staged and reinitialised
(:mod:`harness.compose`), how the tenant's `cosmon-remote` binary is
driven and every exchange recorded (:mod:`harness.remote`), and how a
failed expectation turns into a report that names the request, the
response and the adapter log tail (:mod:`harness.expect`).

The split exists so that a test file reads as a scenario — what is
expected and why — with the plumbing named but not inlined.
"""
