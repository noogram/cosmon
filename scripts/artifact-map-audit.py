#!/usr/bin/env python3
"""
Artifact-map audit — exogenous residence gate over .cosmon/artifact-map.toml.

WHY THIS EXISTS
---------------
The artifact map (`.cosmon/artifact-map.toml`) declares what every tracked file
*is* (its genre) and *who it is for* (its audience). Since the one-repo model
switch (ADR-133) cosmon ships as ONE public repository, so the governance rule
the public flip depends on is brutally simple:

    A fresh public clone of `main` must contain ONLY public-audience artifacts.

cosmon uses exactly two audiences: `public` (ships) and `solo` (regenerable /
local-only, must never be tracked). Any tracked path that classifies `solo` —
runtime state, cargo-mutants output, a wrangler cache, the retired allowlist
machinery — is a residence leak: a local-only artifact that slipped into the
index and would ship on the public flip. This audit turns the build red on it.

WHY A SELF-CONTAINED PYTHON WALKER (and not `cs artifacts audit`)
-----------------------------------------------------------------
The thing being released is the `cs` binary itself. A gate that *is* the audited
binary auditing its own release is the self-referee pathology cosmon already
named (ADR-127 §6 "the detector must not be its own leak"). This walker is
EXOGENOUS: it is plain stdlib Python, lives in CI (not the operator's head),
has NO skip-env and NO continue-on-error. Silencing it requires editing this
file or the workflow in a visible, attributable commit. It is NOT a
`.git/hooks/pre-commit` (those are `--no-verify`-bypassable and self-refereed).
CI needs no extra binary and the audit stays runnable from a fresh clone with
nothing but Python 3.11+.

WHAT IT CHECKS (any one failing → exit 1, build red)
  1. TOTALITY (invariant I1): every `git ls-files` path matches >=1 genre glob.
     Guaranteed only while the `code` catch-all `**/*` is present and LAST.
  2. RESIDENCE (the release rule): no tracked path classifies as a non-public
     audience. Allowed on main = {public}. Anything `solo` is a leak → red.

CLASSIFICATION (invariant I2 — longest-match wins)
  the genre whose matching glob has the most fixed (non-wildcard) leading path
  components classifies the path; ties resolve in declaration order (first
  table wins). This mirrors cosmon-core/src/artifact_map.rs exactly so the
  Python gate and the Rust `cs inspect` never disagree.

Usage:
    scripts/artifact-map-audit.py                 # audit `git ls-files`
    scripts/artifact-map-audit.py --json          # machine-readable summary
    scripts/artifact-map-audit.py --map PATH      # alternate map location
    scripts/artifact-map-audit.py PATH [PATH...]  # classify explicit paths
                                                  # (testing; bypasses git)
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

try:
    import tomllib  # Python 3.11+ stdlib (GitHub's ubuntu-latest ships 3.12)
except ModuleNotFoundError:  # pragma: no cover — local Python < 3.11 fallback
    try:
        import tomli as tomllib  # type: ignore[no-redef]
    except ModuleNotFoundError:
        sys.stderr.write(
            "error: this audit requires Python 3.11+ (stdlib tomllib) or the "
            "`tomli` backport. GitHub's ubuntu-latest ships 3.12; locally try "
            "a 3.11+ interpreter, e.g. `python3.13 scripts/artifact-map-audit.py`.\n"
        )
        sys.exit(2)

# Audiences a fresh public clone of `main` is allowed to contain. Under the
# one-repo model (ADR-133) that is exactly {public}; everything `solo` must be
# untracked + gitignored. Keyed as a positive allow-list so a NEW confidential
# audience added to the map fails closed (red) by default.
PUBLIC_AUDIENCES = frozenset({"public"})

_WILDCARD_RE = re.compile(r"[*?\[]")


def glob_to_regex(glob: str) -> re.Pattern[str]:
    """Translate a location glob into an anchored regex.

    Semantics (gitignore-flavoured, the subset the map uses):
      **/*  -> .+        (one or more of anything, incl. slashes; "everything under")
      /**   -> (?:/.*)?  (trailing: this dir and everything under it)
      **/   -> (?:.*/)?  (zero or more leading directories)
      **    -> .*        (any run, incl. slashes)
      *     -> [^/]*     (any run within a single path component)
      ?     -> [^/]      (one char within a component)
    Order matters: the 4-/3-char forms are tested before the 2-/1-char forms.
    """
    out: list[str] = ["^"]
    i, n = 0, len(glob)
    while i < n:
        if glob[i : i + 4] == "**/*":
            out.append(".+")
            i += 4
        elif glob[i : i + 3] == "/**":
            out.append("(?:/.*)?")
            i += 3
        elif glob[i : i + 3] == "**/":
            out.append("(?:.*/)?")
            i += 3
        elif glob[i : i + 2] == "**":
            out.append(".*")
            i += 2
        elif glob[i] == "*":
            out.append("[^/]*")
            i += 1
        elif glob[i] == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(glob[i]))
            i += 1
    out.append("$")
    return re.compile("".join(out))


def specificity(glob: str) -> int:
    """Number of fixed (non-wildcard) leading path components.

    `.cosmon/release-rules.toml` -> 2, `mutants.out/**/*` -> 1, `**/*` -> 0,
    a bare literal like `Cargo.toml` -> 1. This is the longest-match key
    (invariant I2): higher specificity wins.
    """
    m = _WILDCARD_RE.search(glob)
    prefix = glob if m is None else glob[: m.start()]
    return len([c for c in prefix.split("/") if c])


class Rule:
    __slots__ = ("genre", "audience", "glob", "pattern", "spec", "order")

    def __init__(self, genre: str, audience: str, glob: str, order: int):
        self.genre = genre
        self.audience = audience
        self.glob = glob
        self.pattern = glob_to_regex(glob)
        self.spec = specificity(glob)
        self.order = order


def load_rules(map_path: Path) -> list[Rule]:
    with map_path.open("rb") as fh:
        data = tomllib.load(fh)
    rules: list[Rule] = []
    order = 0
    for genre, body in data.items():
        if not isinstance(body, dict) or "location" not in body:
            continue
        audience = body.get("audience", "")
        locations = body["location"]
        if isinstance(locations, str):
            locations = [locations]
        for glob in locations:
            rules.append(Rule(genre, audience, glob, order))
            order += 1
    return rules


def classify(path: str, rules: list[Rule]) -> Rule | None:
    """Longest-match wins; ties -> earliest declaration order."""
    best: Rule | None = None
    for rule in rules:
        if rule.pattern.match(path):
            if best is None or rule.spec > best.spec or (
                rule.spec == best.spec and rule.order < best.order
            ):
                best = rule
    return best


def tracked_paths() -> list[str]:
    out = subprocess.run(
        ["git", "ls-files"], capture_output=True, text=True, check=True
    ).stdout
    return [line for line in out.splitlines() if line]


def main() -> int:
    ap = argparse.ArgumentParser(description="artifact-map residence audit")
    ap.add_argument("--map", default=".cosmon/artifact-map.toml", help="path to artifact-map.toml")
    ap.add_argument("--json", action="store_true", help="machine-readable summary")
    ap.add_argument("paths", nargs="*", help="explicit paths to classify (testing; bypasses git)")
    args = ap.parse_args()

    map_path = Path(args.map)
    if not map_path.is_file():
        sys.stderr.write(f"error: artifact map not found at {map_path}\n")
        return 2

    rules = load_rules(map_path)
    if not any(r.glob == "**/*" for r in rules):
        sys.stderr.write(
            "error: catch-all glob `**/*` is missing from the map — totality "
            "(invariant I1) is not guaranteed.\n"
        )
        return 2

    paths = args.paths if args.paths else tracked_paths()

    per_genre: dict[str, int] = {}
    per_audience: dict[str, int] = {}
    unmapped: list[str] = []
    leaks: list[tuple[str, str, str]] = []  # (path, genre, audience)

    for path in paths:
        rule = classify(path, rules)
        if rule is None:
            unmapped.append(path)
            continue
        per_genre[rule.genre] = per_genre.get(rule.genre, 0) + 1
        per_audience[rule.audience] = per_audience.get(rule.audience, 0) + 1
        if rule.audience not in PUBLIC_AUDIENCES:
            leaks.append((path, rule.genre, rule.audience))

    ok = not unmapped and not leaks

    if args.json:
        print(json.dumps({
            "tracked": len(paths),
            "per_genre": dict(sorted(per_genre.items())),
            "per_audience": dict(sorted(per_audience.items())),
            "unmapped": unmapped,
            "leaks": [{"path": p, "genre": g, "audience": a} for p, g, a in leaks],
            "ok": ok,
        }, indent=2))
        return 0 if ok else 1

    print(f"artifact-map audit — {len(paths)} tracked paths")
    print()
    for genre, count in sorted(per_genre.items()):
        print(f"  {genre:<24} {count:>5}")
    print()
    print("  audience census:")
    for aud, count in sorted(per_audience.items()):
        flag = "" if aud in PUBLIC_AUDIENCES else "   <-- NON-PUBLIC (leak)"
        print(f"    {aud:<22} {count:>5}{flag}")
    print()

    if unmapped:
        print(f"::error::I1 TOTALITY VIOLATION — {len(unmapped)} path(s) match no genre:")
        for p in unmapped:
            print(f"::error::  unmapped: {p}")
    if leaks:
        print(
            f"::error::RESIDENCE VIOLATION — {len(leaks)} non-public path(s) "
            "tracked on main (solo artifacts must be `git rm --cached`'d + "
            "gitignored, never shipped):"
        )
        for p, g, a in leaks:
            print(f"::error::  leak: {p}  [genre={g}, audience={a}]")

    if ok:
        print("invariants: OK — every tracked path is public-audience and classified.")
        return 0

    print()
    print(
        "AUDIT RED. This referee is exogenous and non-waivable: silencing it "
        "requires editing scripts/artifact-map-audit.py or the workflow in a "
        "visible commit."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
