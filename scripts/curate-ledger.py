#!/usr/bin/env python3
"""Curate-patrol ledger: append-and-flush-and-BLAKE3-seal discipline.

Operationalises the 2026-05-21 drain-worker lesson — a worker died in
machine sleep with its final report unflushed, and the verdict batch was
lost. Every row written to `scan.ndjson` / `decisions.ndjson` must be on
disk *before* the action side-effect fires, sealed with BLAKE3 so a later
pass can detect torn writes, tampering, or post-hoc edits.

This is the cosmon-side analogue of `cs verify <mol_id>` (which verifies
briefing seals). See:
  - delib-20260521-c3cd/synthesis.md
  - global memory feedback_worker_checkpoint_discipline.md

Subcommands:
  append    <ledger-path> <row-json>      # write one sealed row, fsync, then return
  resume    <ledger-path>                  # print already-decided mol_ids, one per line
  verify    <molecule-dir>                 # exit 0 clean / 1 mismatch / 2 corrupt

Contract per row:
  sealed = blake3( mol_id || canonical_json(action) || decided_at ).hex()

Canonical JSON = json.dumps(x, sort_keys=True, separators=(',',':')).
Reproducible across writer + verifier; no whitespace ambiguity.
"""

from __future__ import annotations

import argparse
import hashlib  # noqa: F401  -- documents stdlib lacks blake3; we use b3sum
import json
import os
import subprocess
import sys
from pathlib import Path


EXIT_OK = 0
EXIT_MISMATCH = 1
EXIT_CORRUPT = 2


def _canonical_action(action: object) -> str:
    """Canonical JSON for the `action` field — sort_keys, no whitespace."""
    return json.dumps(action, sort_keys=True, separators=(",", ":"))


def _seal_input(mol_id: str, action: object, decided_at: str) -> bytes:
    return f"{mol_id}||{_canonical_action(action)}||{decided_at}".encode("utf-8")


def _blake3_hex(data: bytes) -> str:
    """Compute BLAKE3 via the b3sum CLI (already installed via cargo).

    Pure-stdlib Python has no blake3; we shell out rather than require pip.
    """
    r = subprocess.run(
        ["b3sum", "--no-names"],
        input=data,
        check=True,
        capture_output=True,
    )
    return r.stdout.decode("ascii").strip().split()[0]


def cmd_append(ledger_path: Path, row_json: str) -> int:
    """Write one sealed row to ledger_path: O_APPEND, then fsync.

    The caller MUST NOT fire any side-effect until this exits 0.
    """
    try:
        row = json.loads(row_json)
    except json.JSONDecodeError as e:
        print(f"curate-ledger: invalid row JSON: {e}", file=sys.stderr)
        return EXIT_CORRUPT

    for required in ("mol_id", "action", "decided_at"):
        if required not in row:
            print(
                f"curate-ledger: row missing required field {required!r}",
                file=sys.stderr,
            )
            return EXIT_CORRUPT

    seal_hex = _blake3_hex(_seal_input(row["mol_id"], row["action"], row["decided_at"]))
    row["sealed"] = seal_hex

    line = json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n"

    ledger_path.parent.mkdir(parents=True, exist_ok=True)

    fd = os.open(
        str(ledger_path),
        os.O_WRONLY | os.O_CREAT | os.O_APPEND,
        0o644,
    )
    try:
        written = os.write(fd, line.encode("utf-8"))
        if written != len(line.encode("utf-8")):
            print(
                f"curate-ledger: short write {written} of {len(line)} bytes",
                file=sys.stderr,
            )
            return EXIT_CORRUPT
        os.fsync(fd)
    finally:
        os.close(fd)

    print(seal_hex)
    return EXIT_OK


def _iter_ledger_rows(path: Path):
    """Yield (lineno, row_dict_or_None, raw_line). row=None on torn-write."""
    if not path.exists():
        return
    with path.open("r", encoding="utf-8") as f:
        for lineno, raw in enumerate(f, start=1):
            # A torn write at EOF will lack the trailing '\n'; we surface it
            # as raw.endswith("\n") == False so the caller can decide.
            stripped = raw.rstrip("\n")
            if not stripped:
                continue
            try:
                row = json.loads(stripped)
            except json.JSONDecodeError:
                yield lineno, None, raw
                continue
            yield lineno, row, raw


def cmd_resume(ledger_path: Path) -> int:
    """Print already-decided mol_ids (one per line) for resume-after-crash.

    Skips torn-write final line silently (the next pass will rewrite it).
    Bad seals → exit 1, the operator must investigate.
    """
    if not ledger_path.exists():
        return EXIT_OK

    seen_ids: set[str] = set()
    last_lineno = 0
    for lineno, row, raw in _iter_ledger_rows(ledger_path):
        last_lineno = lineno
        if row is None:
            # Torn write: ignore IF it's the last line; otherwise corrupt.
            continue
        mol_id = row.get("mol_id")
        action = row.get("action")
        decided_at = row.get("decided_at")
        sealed = row.get("sealed")
        if mol_id is None or action is None or decided_at is None or sealed is None:
            print(
                f"curate-ledger: line {lineno} missing fields",
                file=sys.stderr,
            )
            return EXIT_MISMATCH
        expected = _blake3_hex(_seal_input(mol_id, action, decided_at))
        if expected != sealed:
            print(
                f"curate-ledger: line {lineno} seal mismatch "
                f"(have={sealed} want={expected})",
                file=sys.stderr,
            )
            return EXIT_MISMATCH
        seen_ids.add(mol_id)

    # Detect a torn write that is NOT the last line: any None entry whose
    # lineno < last_lineno means the file is fundamentally corrupt.
    for lineno, row, raw in _iter_ledger_rows(ledger_path):
        if row is None and lineno < last_lineno:
            print(
                f"curate-ledger: torn write at line {lineno} (not EOF)",
                file=sys.stderr,
            )
            return EXIT_CORRUPT

    for mol_id in sorted(seen_ids):
        print(mol_id)
    return EXIT_OK


def cmd_verify(molecule_dir: Path) -> int:
    """Verify scan.ndjson + decisions.ndjson in a molecule directory.

    Exit codes:
      0 — clean (all seals valid, every decision has a matching scan).
      1 — seal mismatch (tampered or post-hoc edit).
      2 — truncated / corrupt (torn write mid-ledger, not EOF).
    """
    scan_path = molecule_dir / "scan.ndjson"
    dec_path = molecule_dir / "decisions.ndjson"

    if not scan_path.exists() and not dec_path.exists():
        print(
            f"curate-ledger: no ledgers in {molecule_dir} (nothing to verify)",
            file=sys.stderr,
        )
        return EXIT_OK

    scan_ids: set[str] = set()
    scan_corrupt = False
    scan_eof_torn = False
    scan_rows: list[tuple[int, dict | None]] = []

    for lineno, row, raw in _iter_ledger_rows(scan_path):
        scan_rows.append((lineno, row))

    if scan_rows:
        last_lineno = scan_rows[-1][0]
        for lineno, row in scan_rows:
            if row is None:
                if lineno == last_lineno:
                    scan_eof_torn = True
                    print(
                        f"curate-ledger: scan.ndjson torn write at EOF "
                        f"line {lineno} (recoverable)",
                        file=sys.stderr,
                    )
                else:
                    scan_corrupt = True
                    print(
                        f"curate-ledger: scan.ndjson torn write at line "
                        f"{lineno} (mid-file, unrecoverable)",
                        file=sys.stderr,
                    )
                continue
            seal_ok = _verify_row(row, lineno, "scan.ndjson")
            if seal_ok is False:
                return EXIT_MISMATCH
            if seal_ok is None:
                return EXIT_MISMATCH
            scan_ids.add(row["mol_id"])

    if scan_corrupt:
        return EXIT_CORRUPT

    dec_rows: list[tuple[int, dict | None]] = []
    for lineno, row, raw in _iter_ledger_rows(dec_path):
        dec_rows.append((lineno, row))

    if dec_rows:
        last_lineno = dec_rows[-1][0]
        for lineno, row in dec_rows:
            if row is None:
                if lineno == last_lineno:
                    print(
                        f"curate-ledger: decisions.ndjson torn write at EOF "
                        f"line {lineno} (recoverable)",
                        file=sys.stderr,
                    )
                else:
                    print(
                        f"curate-ledger: decisions.ndjson torn write at line "
                        f"{lineno} (mid-file, unrecoverable)",
                        file=sys.stderr,
                    )
                    return EXIT_CORRUPT
                continue
            seal_ok = _verify_row(row, lineno, "decisions.ndjson")
            if seal_ok is False or seal_ok is None:
                return EXIT_MISMATCH
            # Every decision MUST have been scanned first.
            if row["mol_id"] not in scan_ids:
                print(
                    f"curate-ledger: decisions.ndjson line {lineno} "
                    f"mol_id={row['mol_id']} has no matching scan.ndjson row",
                    file=sys.stderr,
                )
                return EXIT_MISMATCH

    scan_count = sum(1 for _, r in scan_rows if r is not None)
    dec_count = sum(1 for _, r in dec_rows if r is not None)
    eof_note = " (EOF torn write tolerated)" if scan_eof_torn else ""
    print(
        f"curate-ledger: clean — {scan_count} scan rows, {dec_count} "
        f"decisions, all seals valid{eof_note}"
    )
    return EXIT_OK


def _verify_row(row: dict, lineno: int, label: str) -> bool | None:
    """Return True if seal valid, False if mismatch, None if structural error."""
    for required in ("mol_id", "action", "decided_at", "sealed"):
        if required not in row:
            print(
                f"curate-ledger: {label} line {lineno} missing field "
                f"{required!r}",
                file=sys.stderr,
            )
            return None
    expected = _blake3_hex(
        _seal_input(row["mol_id"], row["action"], row["decided_at"])
    )
    if expected != row["sealed"]:
        print(
            f"curate-ledger: {label} line {lineno} seal mismatch "
            f"(have={row['sealed']} want={expected})",
            file=sys.stderr,
        )
        return False
    return True


def main() -> int:
    p = argparse.ArgumentParser(
        prog="curate-ledger",
        description=(
            "Append+flush+BLAKE3-seal helper for curate-patrol ledgers. "
            "See scripts/curate-ledger.py module docstring for the contract."
        ),
    )
    sub = p.add_subparsers(dest="cmd", required=True)

    a = sub.add_parser("append", help="Append one sealed row + fsync.")
    a.add_argument("ledger_path", type=Path)
    a.add_argument("row_json", help="JSON object (must include mol_id, action, decided_at).")

    r = sub.add_parser("resume", help="Print already-decided mol_ids for crash-resume.")
    r.add_argument("ledger_path", type=Path)

    v = sub.add_parser("verify", help="Verify scan.ndjson + decisions.ndjson in a molecule dir.")
    v.add_argument("molecule_dir", type=Path)

    args = p.parse_args()
    if args.cmd == "append":
        return cmd_append(args.ledger_path, args.row_json)
    if args.cmd == "resume":
        return cmd_resume(args.ledger_path)
    if args.cmd == "verify":
        return cmd_verify(args.molecule_dir)
    return EXIT_CORRUPT


if __name__ == "__main__":
    sys.exit(main())
