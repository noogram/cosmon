#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Detect offline model-pin drift; write a proposal without modifying consumers.

Exit codes: 0 clean, 1 drift, 2 unmeasured (takes precedence over drift).
All source, consumer and output paths are explicit. The JSON proposal contains
no inferred tiers. Catalog membership is evidence of listing, not runnability.

Example: python3 scripts/model-ladder-drift.py --codex-cache caches/codex.json
  --claude-config-dir accounts/example --consumer config.toml
  --proposal model-ladder.proposed.json --max-age-hours 24
Repeat --claude-config-dir for separate accounts and --consumer for more files.
Only the proposal is written. Overflow and disabled listings are reported as
plan-gated at their fetch time; unknown consumer ids count as drift. Conflicting
account evidence is retained and any older match takes precedence. A staleAt
expiry also makes a catalog unmeasured. No network or model probe is performed.
"""

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import sys


MODEL = re.compile(r"(?<![\w-])(?:gpt-\d(?:[\w.-]*\w)?|claude-(?:[a-z]+-)+\d(?:[\w.-]*\w)?)(?![\w-])")
OLDER = re.compile(r"\b(?:older|previous)\b", re.I)
FRONTIER = re.compile(r"\b(?:frontier|most capable)\b|(?:^|-)opus-|(?:^|-)fable-", re.I)


def timestamp(value):
    """Require timezone-aware ISO timestamps so age has an unambiguous meaning."""
    result = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if result.tzinfo is None:
        raise ValueError("timestamp has no timezone")
    return result


def read_cache(path, provider, now, max_age):
    """Normalize one catalog while retaining the vendor evidence verbatim."""
    data = json.loads(path.read_text())
    if not isinstance(data, dict):
        raise ValueError("cache must be an object")
    fetched = data["fetched_at" if provider == "codex" else "fetchedAt"]
    age = (now - timestamp(fetched)).total_seconds()
    if age < 0 or age > max_age:
        raise ValueError("fetch time is future-dated or exceeds max age")
    stale = data.get("staleAt") if provider == "claude" else None
    if stale and now >= timestamp(stale):
        raise ValueError("catalog staleAt has elapsed")
    rows = data["models"] if provider == "codex" else data["catalog"]["config"]["models"]
    if not isinstance(rows, list) or not rows:
        raise ValueError("model list is empty or invalid")
    models = []
    for row in rows:
        if not isinstance(row, dict):
            raise ValueError("model entry must be an object")
        ident = row["slug" if provider == "codex" else "id"]
        if not isinstance(ident, str) or not ident:
            raise ValueError("invalid model id")
        description = row.get("description", "")
        section = row.get("section")
        disabled = row.get("disabled", False)
        if not isinstance(description, str) or not isinstance(disabled, bool):
            raise ValueError("invalid description or disabled flag")
        models.append({
            "id": ident, "name": row.get("name"), "description": description,
            "section": section, "disabled": disabled,
            "priority": row.get("priority"), "visibility": row.get("visibility"),
            "classification": "older" if OLDER.search(description) or section == "overflow" else "current",
            "plan_gated": disabled or section == "overflow",
            "source": str(path), "fetched_at": fetched,
        })
    return models


def newest_catalog(config):
    """Select by fetch timestamp, not filename or filesystem modification time.

    A malformed candidate makes the measurement unavailable: silently skipping
    it could select an older snapshot and misrepresent the account's state.
    """
    paths = list((config / "cache" / "model-catalog").glob("*-cc.json"))
    if not paths:
        raise ValueError("no catalog snapshots")
    def fetched(path):
        data = json.loads(path.read_text())
        if not isinstance(data, dict):
            raise ValueError("catalog must be an object")
        return timestamp(data["fetchedAt"]), str(path)

    return max(paths, key=fetched)


def uncovered_defaults(text, models):
    """Report heuristic frontier defaults lacking exact coverage in any strong list.

    This is a textual audit of literal TOML-style assignments, including multiline
    arrays; it neither evaluates dynamic expressions nor defines dispatch tiers.
    """
    # Remove TOML comments without erasing hashes inside quoted strings.
    text = re.sub(r'''("(?:\\.|[^"\\])*"|'[^']*')|#[^\n]*''',
                  lambda match: match[1] or "", text)
    strong = set()
    for match in re.finditer(r"(?m)^\s*strong\s*=\s*\[([^]]*)\]", text):
        strong.update(MODEL.findall(match[1]))
    for match in re.finditer(r'''(?m)^\s*default_model\s*=\s*["']([^"']+)["']''', text):
        ident = match[1]
        descriptions = " ".join(m["description"] for m in models if m["id"] == ident)
        if ident not in strong and FRONTIER.search(ident + " " + descriptions):
            yield {"id": ident, "line": text.count("\n", 0, match.start()) + 1,
                   "finding": "frontier-looking default_model lacks strong coverage (heuristic)"}


def main():
    """Run the detect-only audit and emit a machine-readable proposal/report."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex-cache", required=True, type=Path)
    parser.add_argument("--claude-config-dir", required=True, action="append", type=Path)
    parser.add_argument("--consumer", required=True, action="append", type=Path)
    parser.add_argument("--proposal", required=True, type=Path)
    parser.add_argument("--max-age-hours", required=True, type=float)
    parser.add_argument("--now", help="ISO timestamp for reproducible offline audits (default: UTC now)")
    args = parser.parse_args()
    if not 0 < args.max_age_hours < float("inf"):
        parser.error("max age must be positive and finite")
    try:
        now = timestamp(args.now) if args.now else datetime.now(timezone.utc)
        # Refuse both aliases and hard links to any input, including old snapshots.
        inputs = [args.codex_cache, *args.consumer]
        for config in args.claude_config_dir:
            inputs.extend((config / "cache" / "model-catalog").glob("*-cc.json"))
        for path in inputs:
            if args.proposal.resolve() == path.resolve() or (
                args.proposal.exists() and path.exists() and args.proposal.samefile(path)
            ):
                raise ValueError("proposal must not overwrite an input")
    except (OSError, ValueError) as error:
        parser.error(str(error))
    report = {"status": "proposed", "measured_at": now.isoformat(),
              "providers": {"codex": [], "claude": []}, "consumers": [],
              "unmeasured": [], "findings": []}
    sources = [("codex", args.codex_cache)]
    for config in args.claude_config_dir:
        try:
            sources.append(("claude", newest_catalog(config)))
        except (OSError, UnicodeError, ValueError, KeyError, TypeError, AttributeError) as error:
            report["unmeasured"].append({"source": str(config), "reason": str(error)})
    for provider, path in sources:
        try:
            report["providers"][provider].extend(read_cache(path, provider, now, args.max_age_hours * 3600))
        except (OSError, UnicodeError, ValueError, KeyError, TypeError, AttributeError) as error:
            report["unmeasured"].append({"source": str(path), "reason": str(error)})
    models = [m for rows in report["providers"].values() for m in rows]
    for model in models:
        if model["plan_gated"]:
            report["findings"].append({"finding": "plan-gated per catalog", **model})
    for path in args.consumer:
        try:
            text = path.read_text()
        except (OSError, UnicodeError) as error:
            report["unmeasured"].append({"source": str(path), "reason": str(error)})
            continue
        for number, line in enumerate(text.splitlines(), 1):
            for ident in sorted(set(MODEL.findall(line))):
                matches = [m for m in models if m["id"] == ident]
                classification = ("older" if any(m["classification"] == "older" for m in matches)
                                  else "current" if matches else "unknown")
                report["consumers"].append({"source": str(path), "line": number, "id": ident,
                                            "classification": classification, "evidence": matches})
        for finding in uncovered_defaults(text, models):
            report["findings"].append({"source": str(path), **finding})
    drift = bool(report["findings"]) or any(c["classification"] != "current" for c in report["consumers"])
    report["measurement"] = "unmeasured" if report["unmeasured"] else "drift" if drift else "clean"
    try:
        args.proposal.write_text(json.dumps(report, indent=2) + "\n")
    except OSError as error:
        print(f"unmeasured: cannot write proposal: {error}", file=sys.stderr)
        return 2
    print(json.dumps(report, indent=2))
    return 2 if report["unmeasured"] else 1 if drift else 0


if __name__ == "__main__":
    sys.exit(main())
