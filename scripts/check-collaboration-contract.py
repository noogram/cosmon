#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Validate the cross-machine collaboration contract (issue #147, W1).

The contract (`docs/specs/cross-machine-collaboration.md`) is prose plus a few
decision tables. This check reads only the tables and fails when a route row
omits its scope, CLI counterpart, custody writer, limit, retry rule or effect
perimeter disposition, or when a table refers to a name the contract does not
declare. It validates the contract's shape, not any feature behaviour.

Usage: check-collaboration-contract.py [--spec PATH] [--surface PATH]
Exit:  0 when the contract is complete, 1 with one line per finding otherwise.
"""

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_SPEC = ROOT / "docs/specs/cross-machine-collaboration.md"
DEFAULT_SURFACE = ROOT / "crates/cosmon-rpp-adapter/data/surface_events.txt"

ROUTE_COLUMNS = [
    "ID", "Verb", "Route", "Scope", "CLI counterpart", "Writer", "Limit",
    "Retry", "Effect perimeter", "Phase",
]
REFUSED_COLUMNS = ["ID", "Operation", "Disposition", "Reason"]
ERROR_COLUMNS = ["Code", "HTTP", "Meaning"]
DECISION_COLUMNS = ["ID", "Decision", "Status"]
SCOPE_COLUMNS = ["Scope", "Grants", "Implies"]
CUSTODY_COLUMNS = ["Writer", "Record class", "Location"]

EFFECTS = {"read-only", "observation-write", "advisory-write"}
RETRIES = {"idempotent-key", "idempotent-ack", "safe-repeat", "bounded-redelivery"}
PHASES = {"W4", "W7"}
DECISION_STATUS = {"adopted-default", "operator-confirmed"}
REQUIRED_DECISIONS = {f"O{n}" for n in range(1, 9)}
# Operations the contract must name as refused, matched by keyword.
REQUIRED_REFUSALS = [
    "evolve", "complete", "grant", "shell", "transcript", "wake", "nucleate",
    "tackle",
]
ROUTE_RE = re.compile(r"^(GET|POST|PUT|DELETE) /v1/\S+$")
LIMIT_RE = re.compile(r"\d|budget\.")


def cells(line):
    return [c.strip().strip("`") for c in line.strip().strip("|").split("|")]


def tables(text):
    """Yield (header, rows) for every markdown table in `text`."""
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        if lines[i].lstrip().startswith("|") and i + 1 < len(lines) \
                and re.match(r"^\s*\|[\s:|-]+\|\s*$", lines[i + 1]):
            header = cells(lines[i])
            rows = []
            i += 2
            while i < len(lines) and lines[i].lstrip().startswith("|"):
                rows.append(cells(lines[i]))
                i += 1
            yield header, rows
        else:
            i += 1


def find(all_tables, columns):
    return [(h, r) for h, r in all_tables if h[:len(columns)] == columns]


def empty(value):
    return value.strip().lower() in {"", "-", "—", "tbd", "todo", "?", "n/a"}


def mounted_routes(surface_path):
    mounted = set()
    for line in surface_path.read_text(encoding="utf-8").splitlines():
        if line.startswith("#") or "|" not in line:
            continue
        mounted.add(line.split("|")[0].strip())
    return mounted


def check(spec_text, surface_path):
    findings = []
    all_tables = list(tables(spec_text))

    def one(columns, name):
        found = find(all_tables, columns)
        if len(found) < 1:
            findings.append(f"missing table: {name} (columns {' | '.join(columns)})")
            return []
        return [row for _, rows in found for row in rows]

    def rows_of(columns, name):
        rows = one(columns, name)
        for row in rows:
            if len(row) != len(columns):
                findings.append(f"{name}: row {row[0] if row else '?'} has "
                                f"{len(row)} cells, expected {len(columns)}")
        return [r for r in rows if len(r) == len(columns)]

    scopes = {r[0] for r in rows_of(SCOPE_COLUMNS, "scope table")}
    writers = {r[0] for r in rows_of(CUSTODY_COLUMNS, "custody table")}
    for row in rows_of(SCOPE_COLUMNS, "scope table"):
        for name, value in zip(SCOPE_COLUMNS, row):
            if empty(value) and name != "Implies":
                findings.append(f"scope table: {row[0] or '?'} has empty {name}")
    for row in rows_of(CUSTODY_COLUMNS, "custody table"):
        for name, value in zip(CUSTODY_COLUMNS, row):
            if empty(value):
                findings.append(f"custody table: {row[0] or '?'} has empty {name}")

    mounted = mounted_routes(surface_path) if surface_path.exists() else set()
    seen_ids, seen_routes = set(), set()
    routes = rows_of(ROUTE_COLUMNS, "route table")
    if not routes and find(all_tables, ROUTE_COLUMNS):
        findings.append("route table has no rows")
    for row in routes:
        rec = dict(zip(ROUTE_COLUMNS, row))
        rid = rec["ID"] or "?"
        for name in ROUTE_COLUMNS:
            if empty(rec[name]):
                findings.append(f"route {rid}: empty {name}")
        if rid in seen_ids:
            findings.append(f"route {rid}: duplicate ID")
        seen_ids.add(rid)
        if not empty(rec["Route"]):
            if not ROUTE_RE.match(rec["Route"]):
                findings.append(f"route {rid}: Route must be 'METHOD /v1/...'")
            if rec["Route"] in seen_routes:
                findings.append(f"route {rid}: duplicate route {rec['Route']}")
            seen_routes.add(rec["Route"])
            if rec["Route"] in mounted:
                findings.append(f"route {rid}: {rec['Route']} is already mounted; "
                                "this contract proposes new routes only")
        if not empty(rec["Scope"]) and rec["Scope"] not in scopes:
            findings.append(f"route {rid}: scope {rec['Scope']} is not in the scope table")
        if not empty(rec["Writer"]) and rec["Writer"] not in writers:
            findings.append(f"route {rid}: writer {rec['Writer']} is not in the custody table")
        if not empty(rec["CLI counterpart"]) and "cs " not in rec["CLI counterpart"]:
            findings.append(f"route {rid}: CLI counterpart must name a `cs ...` verb")
        if not empty(rec["Limit"]) and not LIMIT_RE.search(rec["Limit"]):
            findings.append(f"route {rid}: Limit must be a number or a budget.* field")
        if not empty(rec["Retry"]) and rec["Retry"] not in RETRIES:
            findings.append(f"route {rid}: Retry {rec['Retry']!r} not in {sorted(RETRIES)}")
        if not empty(rec["Effect perimeter"]) and rec["Effect perimeter"] not in EFFECTS:
            findings.append(f"route {rid}: Effect perimeter {rec['Effect perimeter']!r} "
                            f"not in {sorted(EFFECTS)}")
        if not empty(rec["Phase"]) and rec["Phase"] not in PHASES:
            findings.append(f"route {rid}: Phase {rec['Phase']!r} not in {sorted(PHASES)}")

    refused = rows_of(REFUSED_COLUMNS, "refused-operations table")
    for row in refused:
        rec = dict(zip(REFUSED_COLUMNS, row))
        for name in REFUSED_COLUMNS:
            if empty(rec[name]):
                findings.append(f"refused {rec['ID'] or '?'}: empty {name}")
        if rec["Disposition"] != "refused":
            findings.append(f"refused {rec['ID']}: Disposition must be 'refused'")
    named = " ".join(r[1].lower() for r in refused)
    for keyword in REQUIRED_REFUSALS:
        if refused and keyword not in named:
            findings.append(f"refused-operations table does not name '{keyword}'")

    errors = rows_of(ERROR_COLUMNS, "error table")
    codes = [r[0] for r in errors]
    for row in errors:
        if any(empty(c) for c in row):
            findings.append(f"error {row[0] or '?'}: empty cell")
        if not re.match(r"^[a-z][a-z0-9_]*$", row[0]):
            findings.append(f"error {row[0]!r}: code must be snake_case")
        if not re.match(r"^[45]\d\d$", row[1]):
            findings.append(f"error {row[0]}: HTTP must be a 4xx/5xx status")
    if len(codes) != len(set(codes)):
        findings.append("error table: duplicate code")

    decisions = rows_of(DECISION_COLUMNS, "operator-decision table")
    ids = {r[0] for r in decisions}
    for missing in sorted(REQUIRED_DECISIONS - ids):
        findings.append(f"operator-decision table omits {missing}")
    for row in decisions:
        if any(empty(c) for c in row):
            findings.append(f"decision {row[0] or '?'}: empty cell")
        if row[2] not in DECISION_STATUS:
            findings.append(f"decision {row[0]}: Status {row[2]!r} not in "
                            f"{sorted(DECISION_STATUS)}")
    return findings


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--spec", type=Path, default=DEFAULT_SPEC)
    parser.add_argument("--surface", type=Path, default=DEFAULT_SURFACE)
    args = parser.parse_args()
    if not args.spec.exists():
        print(f"collaboration contract not found: {args.spec}", file=sys.stderr)
        return 1
    findings = check(args.spec.read_text(encoding="utf-8"), args.surface)
    for finding in findings:
        print(f"collaboration contract: {finding}", file=sys.stderr)
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
