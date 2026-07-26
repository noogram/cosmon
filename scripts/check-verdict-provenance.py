#!/usr/bin/env python3
"""Verify, mechanically, that the issue-#20 verdict provenance index is honest.

Provenance is only worth having if it is a *testable* property.  A documentary
convention degrades silently: a verdict is added and nobody links it, a chain
is closed into a loop, an old file is edited to make it agree with the present.
None of those are visible by reading.  This script makes each of them a
failure with a name.

It checks four conditions, in the order in which they can be trusted:

(a) IMMUTABILITY  -- every catalogued verdict is byte-identical to the
    fingerprint recorded beside it.  A verdict that changed after being
    pronounced is not evidence of anything, so this is checked first and a
    mismatch is fatal.

(b) ACCESSIBILITY -- starting from *any* catalogued verdict and following the
    succession register forward, a reader reaches a terminal verdict.  No path
    ends on a pointer to something that is not catalogued.

(c) ACYCLICITY AND TERMINATION -- the transition graph has no cycle, and every
    chain ends on exactly one terminal.  A cycle would make "who has authority
    here?" undecidable; a chain that does not terminate would leave it
    unanswered.

(d) UNICITY -- for the frozen head, each subject has exactly one authoritative
    verdict.  Zero is a hole; two is an ambiguity.  Both are *reported*, with
    the candidates named, and neither is arbitrated here: a provenance index
    that silently picks a winner is the class of surface lie this whole
    apparatus exists to prevent.

Exit codes are graded, because the two kinds of failure are not the same:

    0  all four conditions clean
    2  (a), (b) and (c) clean, (d) has findings -- holes or ambiguities
    1  a hard failure: mutated verdict, dangling pointer, cycle, or a
       malformed input file

Usage::

    scripts/check-verdict-provenance.py
    scripts/check-verdict-provenance.py --resolve task-20260723-5371/verdict.json
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path

DEFAULT_DOSSIER = Path("docs/provenance/issue-20")
MOLECULES_SUFFIX = Path(".cosmon/state/fleets/default/molecules")


class Fatal(Exception):
    """A malformed input, which makes every downstream answer meaningless."""


def load_jsonl(path: Path) -> list[dict]:
    """Read a JSON-lines file, dropping the leading ``_comment`` header line."""
    if not path.is_file():
        raise Fatal(f"missing input file: {path}")
    rows = []
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError as exc:
            raise Fatal(f"{path}:{lineno}: not valid JSON: {exc}") from exc
        if "_comment" in row and len(row) == 1:
            continue
        rows.append(row)
    return rows


def find_repo_root(start: Path) -> Path:
    """Walk up to the repository root, the way ``git`` itself finds it."""
    for candidate in [start, *start.parents]:
        if (candidate / ".git").exists():
            return candidate
    return start


def find_molecules_root(repo_root: Path) -> Path | None:
    """Locate the fleet molecule directory, which lives outside the repository."""
    for candidate in [repo_root, *repo_root.parents]:
        root = candidate / MOLECULES_SUFFIX
        if root.is_dir():
            return root
        # A worktree sits under <galaxy>/.worktrees/<id>; the state is the
        # galaxy's, not the worktree's.
        if candidate.name == ".worktrees":
            root = candidate.parent / MOLECULES_SUFFIX
            if root.is_dir():
                return root
    return None


def resolve(entry: dict, repo_root: Path, molecules_root: Path | None) -> Path | None:
    """Turn a catalogue path into an absolute path, honouring its declared root."""
    if entry.get("root") == "repo":
        return repo_root / entry["path"]
    if molecules_root is None:
        return None
    return molecules_root / entry["path"]


def git(repo_root: Path, *args: str) -> tuple[int, str]:
    """Run git in the repository, returning the exit code and trimmed stdout."""
    proc = subprocess.run(
        ["git", "-C", str(repo_root), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    return proc.returncode, proc.stdout.strip()


def is_ancestor(repo_root: Path, older: str, newer: str) -> bool | None:
    """True/False when git can answer, None when either commit is unknown here."""
    for sha in (older, newer):
        if git(repo_root, "cat-file", "-e", f"{sha}^{{commit}}")[0] != 0:
            return None
    return git(repo_root, "merge-base", "--is-ancestor", older, newer)[0] == 0


# --------------------------------------------------------------------------
# (a) immutability


def check_immutability(
    catalogue: list[dict],
    fingerprints: dict[str, str],
    repo_root: Path,
    molecules_root: Path | None,
) -> list[str]:
    """Re-hash every catalogued verdict and compare with the recorded digest."""
    failures = []
    for entry in catalogue:
        path = resolve(entry, repo_root, molecules_root)
        key = entry["path"]
        recorded = fingerprints.get(key)
        if recorded is None:
            failures.append(f"{entry['id']} {key}: no fingerprint recorded")
            continue
        if path is None or not path.is_file():
            failures.append(f"{entry['id']} {key}: file not found under its declared root")
            continue
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
        if actual != recorded:
            failures.append(
                f"{entry['id']} {key}: MUTATED\n"
                f"      recorded {recorded}\n"
                f"      actual   {actual}"
            )
    return failures


# --------------------------------------------------------------------------
# (b)+(c) the transition graph


def build_graph(catalogue: list[dict], register: list[dict]) -> tuple[dict, list[str]]:
    """Map each superseded verdict to its successor, reporting dangling ends."""
    known = {entry["path"] for entry in catalogue}
    successor: dict[str, list[tuple[str, str]]] = {}
    dangling = []
    for row in register:
        prev, nxt = row["previous"], row["next"]
        if prev not in known:
            dangling.append(f"{row['entry']}: `previous` {prev} is not catalogued")
        if nxt not in known:
            dangling.append(f"{row['entry']}: `next` {nxt} is not catalogued")
        successor.setdefault(prev, []).append((nxt, row["entry"]))
    return successor, dangling


def walk(successor: dict, start: str, limit: int = 64) -> tuple[list[str], str | None]:
    """Follow the chain forward from ``start``; return the path and any fault."""
    seen = [start]
    node = start
    while node in successor:
        edges = successor[node]
        if len(edges) > 1:
            return seen, f"forked at {node}: {[e[1] for e in edges]}"
        node = edges[0][0]
        if node in seen:
            return seen + [node], f"cycle re-entering {node}"
        seen.append(node)
        if len(seen) > limit:
            return seen, "chain exceeded the walk limit"
    return seen, None


# --------------------------------------------------------------------------
# (d) unicity


def classify(
    catalogue: list[dict],
    successor: dict,
    head: dict,
    repo_root: Path,
) -> dict[str, tuple[str, str]]:
    """Label each verdict authoritative, superseded or stale-unreplaced."""
    frozen = head["frozen_head"]
    fix = head["final_door_4_fix"]
    status = {}
    for entry in catalogue:
        path = entry["path"]
        if path in successor:
            status[path] = ("superseded", f"named as `previous` by {successor[path][0][1]}")
            continue
        sha = entry.get("subject_sha")
        if not sha:
            status[path] = ("stale-unreplaced", "subject sha indeterminate")
            continue
        on_history = is_ancestor(repo_root, sha, frozen)
        if on_history is None:
            status[path] = ("stale-unreplaced", f"commit {sha} is not in this repository")
        elif not on_history:
            status[path] = (
                "stale-unreplaced",
                f"commit {sha} is not an ancestor of the frozen head (abandoned line)",
            )
        elif not is_ancestor(repo_root, fix, sha):
            status[path] = (
                "stale-unreplaced",
                f"pronounced on {sha}, before the final door-4 fix {fix}",
            )
        else:
            status[path] = ("authoritative", f"pronounced on {sha}, at or after {fix}")
    return status


# --------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--dossier",
        type=Path,
        default=None,
        help="directory holding the catalogue, register, fingerprints and frozen head",
    )
    parser.add_argument(
        "--resolve",
        metavar="VERDICT_PATH",
        default=None,
        help="print the succession chain from one verdict to its terminal, then exit",
    )
    args = parser.parse_args()

    repo_root = find_repo_root(Path(__file__).resolve().parent)
    dossier = args.dossier or (repo_root / DEFAULT_DOSSIER)
    molecules_root = find_molecules_root(repo_root)

    try:
        catalogue = load_jsonl(dossier / "verdict-catalogue.jsonl")
        register = load_jsonl(dossier / "succession-register.jsonl")
        head = json.loads((dossier / "frozen-head.json").read_text(encoding="utf-8"))
        fingerprints = {}
        fp_path = dossier / "verdict-fingerprints.sha256"
        if fp_path.is_file():
            for line in fp_path.read_text(encoding="utf-8").splitlines():
                line = line.strip()
                if not line or line.startswith("#"):
                    continue
                digest, _, name = line.partition("  ")
                fingerprints[name.strip()] = digest.strip()
    except (Fatal, OSError, json.JSONDecodeError) as exc:
        print(f"FATAL: {exc}", file=sys.stderr)
        return 1

    successor, dangling = build_graph(catalogue, register)
    by_path = {entry["path"]: entry for entry in catalogue}

    if args.resolve:
        if args.resolve not in by_path:
            print(f"not a catalogued verdict: {args.resolve}", file=sys.stderr)
            return 1
        chain, fault = walk(successor, args.resolve)
        for step, path in enumerate(chain):
            entry = by_path[path]
            arrow = "    " if step == 0 else " -> "
            print(f"{arrow}{entry['id']}  {entry['subject']:<26} {entry['verdict']}")
            print(f"        {path}")
        if fault:
            print(f"FAULT: {fault}", file=sys.stderr)
            return 1
        terminal = by_path[chain[-1]]
        state, why = classify(catalogue, successor, head, repo_root)[terminal["path"]]
        print(f"\nterminal for subject `{terminal['subject']}` — {state}: {why}")
        if state != "authoritative":
            print(
                "the chain ends here, but this verdict does NOT speak for the frozen\n"
                f"head {head['frozen_head'][:12]}; nothing does. See the index."
            )
        try:
            index = (dossier / "AUTHORITATIVE-INDEX.md").relative_to(repo_root)
        except ValueError:
            index = dossier / "AUTHORITATIVE-INDEX.md"
        print(f"index: {index}")
        return 0

    print(f"mission          {head['mission']}")
    print(f"frozen head      {head['frozen_head']}")
    print(f"final door-4 fix {head['final_door_4_fix']}")
    print(f"catalogue        {len(catalogue)} verdicts")
    print(f"register         {len(register)} transitions")
    # Deliberately not the absolute path: this output is committed, and a
    # machine path is exactly what must never be tracked.
    print(f"molecules root   {MOLECULES_SUFFIX} ({'found' if molecules_root else 'NOT FOUND'})")
    print()

    hard = 0

    # (a)
    mutations = check_immutability(catalogue, fingerprints, repo_root, molecules_root)
    if mutations:
        hard += 1
        print(f"(a) IMMUTABILITY        FAIL — {len(mutations)}")
        for line in mutations:
            print(f"    {line}")
    else:
        print(f"(a) IMMUTABILITY        PASS — {len(catalogue)}/{len(catalogue)} byte-identical")

    # (b)
    dead_ends = list(dangling)
    for entry in catalogue:
        chain, fault = walk(successor, entry["path"])
        if fault:
            dead_ends.append(f"{entry['id']}: {fault}")
    if dead_ends:
        hard += 1
        print(f"(b) ACCESSIBILITY       FAIL — {len(dead_ends)}")
        for line in dead_ends:
            print(f"    {line}")
    else:
        print(f"(b) ACCESSIBILITY       PASS — every verdict reaches a terminal")

    # (c)
    terminals = {}
    faults = []
    for entry in catalogue:
        chain, fault = walk(successor, entry["path"])
        if fault:
            faults.append(f"{entry['id']}: {fault}")
        else:
            terminals[entry["path"]] = chain[-1]
    if faults:
        hard += 1
        print(f"(c) ACYCLIC+TERMINATES  FAIL — {len(faults)}")
        for line in faults:
            print(f"    {line}")
    else:
        distinct = sorted(set(terminals.values()))
        print(
            f"(c) ACYCLIC+TERMINATES  PASS — no cycle, no fork; "
            f"{len(terminals)} chains end on {len(distinct)} terminals"
        )

    # (d)
    status = classify(catalogue, successor, head, repo_root)
    subjects: dict[str, list[str]] = {}
    for entry in catalogue:
        subjects.setdefault(entry["subject"], [])
        if status[entry["path"]][0] == "authoritative":
            subjects[entry["subject"]].append(entry["path"])
    holes = [s for s, v in subjects.items() if len(v) == 0]
    ambiguities = {s: v for s, v in subjects.items() if len(v) > 1}
    ok = [s for s, v in subjects.items() if len(v) == 1]
    print(
        f"(d) UNICITY             {len(ok)} exact, {len(holes)} holes, "
        f"{len(ambiguities)} ambiguities  (over {len(subjects)} subjects)"
    )
    for subject in holes:
        print(f"    HOLE      {subject}: no verdict speaks for the frozen head")
    for subject, candidates in ambiguities.items():
        print(f"    AMBIGUOUS {subject}: {len(candidates)} claimants — NOT arbitrated here")
        for path in candidates:
            print(f"              {path}")

    print()
    print("status of every catalogued verdict")
    width = max(len(e["path"]) for e in catalogue)
    for entry in catalogue:
        state, why = status[entry["path"]]
        print(f"  {entry['id']}  {entry['path']:<{width}}  {state:<17} {why}")

    if hard:
        return 1
    if holes or ambiguities:
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
